#!/bin/bash
# A real installer-helper replay with a CI-built image in private mount
# and network namespaces. The fixture supplies the embedded manifest.
set -euo pipefail
[[ $EUID == 0 && $# == 1 ]]
artifacts=$(realpath "$1")
helper=$(realpath "$(dirname "$0")/../packaging/install-room-image.sh")
parent_mount=$(readlink /proc/self/ns/mnt)
parent_net=$(readlink /proc/self/ns/net)
exec unshare --mount --net --propagation private bash -s -- \
    "$artifacts" "$helper" "$parent_mount" "$parent_net" <<'PROOF'
set -euo pipefail
artifacts=$1 helper=$2
[[ $(readlink /proc/self/ns/mnt) != "$3" && $(readlink /proc/self/ns/net) != "$4" ]]
! grep -Eq ' shared:[0-9]+' /proc/self/mountinfo
proof=$(mktemp -d /var/tmp/hatchling-room-install-proof.XXXXXXXX)
mounted=()
clean() {
    local status=$? clean_mounts=1 i
    for ((i = ${#mounted[@]} - 1; i >= 0; i--)); do
        umount -- "${mounted[i]}" || clean_mounts=0
    done
    if (( clean_mounts )); then
        rm -rf --one-file-system -- "$proof" || status=1
    else
        status=1
    fi
    exit "$status"
}
trap clean EXIT
mkdir -m 755 "$proof/etc" "$proof/share"
mkdir -m 700 "$proof/root"
# Keep the real awk executable available if Ubuntu selects it through /etc.
mkdir "$proof/etc/alternatives"
ln -s "$(realpath "$(command -v awk)")" "$proof/etc/alternatives/awk"
mount --bind "$proof/root" /root; mounted+=(/root)
mount --bind "$proof/etc" /etc; mounted+=(/etc)
mount --bind "$proof/share" /usr/local/share; mounted+=(/usr/local/share)

payload=$(mktemp -d /root/hatchling-release.XXXXXXXX)
prefix=room-system-x86_64
for suffix in squashfs verity root-hash; do
    cp -- "$artifacts/$prefix.$suffix" "$payload/"
done
manifest() {
    (cd "$payload"; sha256sum -- "$prefix.squashfs" "$prefix.verity" "$prefix.root-hash" > SHA256SUMS)
}
manifest
cp "$payload/SHA256SUMS" "$proof/bundled-manifest"
record=/etc/hatchling/room-image.admission
room=/usr/local/share/hatchling/room
expected_record() {
    local digest
    digest=$(sha256sum < "$artifacts/$prefix.squashfs")
    printf '%s\n%s\n' "$(cat "$artifacts/$prefix.root-hash")" "${digest%% *}"
}
install_room() {
    /bin/sh -eu -c '
        fail() { printf "%s\n" "$*" >&2; exit 1; }
        FROM=$1
        TARGET=x86_64-unknown-linux-gnu
        . "$2"
        room_prepare windows
        if [ "$3" = interrupt ]; then
            mv() {
                case "$4" in *.verity) return 1 ;; esac
                command mv "$@"
            }
        fi
        room_publish
    ' proof "$payload" "$helper" "${1:-normal}"
}
refuses() {
    local expected=$1
    if install_room >"$proof/refused.log" 2>&1; then
        echo "installer accepted the refused fixture" >&2
        exit 1
    fi
    cat "$proof/refused.log"
    grep -Fq -- "$expected" "$proof/refused.log"
}
record_ok() {
    expected_record | cmp - "$record"
    [[ $(stat -c '%u:%a:%h' "$record") = 0:600:1 ]]
    for suffix in squashfs verity; do
        cmp "$artifacts/$prefix.$suffix" "$room/$prefix.$suffix"
        [[ $(stat -c '%u:%a:%h' "$room/$prefix.$suffix") = 0:644:1 ]]
        [[ ! "$payload/$prefix.$suffix" -ef "$room/$prefix.$suffix" ]]
    done
}

install_room
record_ok
old_image=$(stat -c '%i' "$room/$prefix.squashfs")
old_record=$(stat -c '%i' "$record")
echo 'Room publisher installed verified fresh inodes with no network'

wrong=$(printf '%064d' 0)
[[ $wrong != "$(cat "$artifacts/$prefix.root-hash")" ]]
printf '%s\n' "$wrong" > "$payload/$prefix.root-hash"
refuses "$prefix.root-hash differs from the Setup manifest"
manifest
refuses "the Room's image failed verity verification"
cp "$artifacts/$prefix.root-hash" "$payload/$prefix.root-hash"
cp "$proof/bundled-manifest" "$payload/SHA256SUMS"
grep -F "$prefix.verity" "$proof/bundled-manifest" >> "$payload/SHA256SUMS"
refuses "$prefix.verity needs one checksum in the Setup manifest"
cp "$proof/bundled-manifest" "$payload/SHA256SUMS"
record_ok
[[ $(stat -c '%i' "$record") = "$old_record" && $(stat -c '%i' "$room/$prefix.squashfs") = "$old_image" ]]
echo 'Verification failures left the prior admitted image and record untouched'

if install_room interrupt; then
    echo 'the interrupted Room publication unexpectedly succeeded' >&2
    exit 1
fi
[[ ! -e "$record" ]]
echo 'Interrupted publication left Room admission closed'
install_room
record_ok
echo 'Retry restored verified admission'

# The release path (WO-038): a release's files in a directory of the
# owner's, as get-hatchling.sh leaves them, checked against the release's
# SHA256SUMS.
# A uid that isn't root's: the proof's /etc names no users.
owner=65534
release=$proof/release
mkdir -- "$release"
release_files() {
    local suffix
    rm -rf -- "${release:?}"/*
    for suffix in squashfs verity root-hash; do
        cp -- "$artifacts/$prefix.$suffix" "$release/"
    done
    (cd "$release"; sha256sum -- "$prefix.squashfs" "$prefix.verity" "$prefix.root-hash" > SHA256SUMS)
    chown -R "$owner" "$release"
    chmod 755 "$release"
    chmod 644 "$release"/*
}
install_release() {
    /bin/sh -eu -c '
        fail() { printf "%s\n" "$*" >&2; exit 1; }
        FROM=$1 OWNER=$3 OWNER_NAME=nobody
        TARGET=x86_64-unknown-linux-gnu
        . "$2"
        if [ "$4" = swap ]; then
            # Once the manifest is staged, the release is swapped for a
            # changed image and a manifest that matches it.
            install() {
                command install "$@"
                case "$4" in
                    */SHA256SUMS)
                        printf x >> "$FROM/room-system-x86_64.squashfs"
                        (cd "$FROM" && sha256sum -- room-system-x86_64.squashfs room-system-x86_64.verity \
                            room-system-x86_64.root-hash > SHA256SUMS)
                        ;;
                esac
            }
        fi
        room_prepare release
        room_publish
    ' proof "$release" "$helper" "$owner" "${1:-normal}"
}
release_refuses() {
    local expected=$1
    if install_release "${2:-normal}" >"$proof/refused.log" 2>&1; then
        echo "installer accepted the refused release" >&2
        exit 1
    fi
    cat "$proof/refused.log"
    grep -Fq -- "$expected" "$proof/refused.log"
    record_ok
    [[ $(stat -c '%i' "$record") = "$release_record" && $(stat -c '%i' "$room/$prefix.squashfs") = "$release_image" ]]
}

release_files
install_release
record_ok
release_record=$(stat -c '%i' "$record")
release_image=$(stat -c '%i' "$room/$prefix.squashfs")
echo 'A release in its owner'"'"'s directory installed its verified image'

printf x >> "$release/$prefix.squashfs"
release_refuses "$prefix.squashfs differs from the release's SHA256SUMS"
release_files
printf '%064d\n' 0 > "$release/$prefix.root-hash"
(cd "$release"; sha256sum -- "$prefix.squashfs" "$prefix.verity" "$prefix.root-hash" > SHA256SUMS)
release_refuses "the Room's image failed verity verification"
release_files
rm -- "$release/$prefix.verity"
release_refuses "is missing a regular $prefix.verity"
release_files
grep -v "  $prefix.verity\$" "$release/SHA256SUMS" > "$proof/sums" && cat "$proof/sums" > "$release/SHA256SUMS"
release_refuses "$prefix.verity needs one checksum in the release's SHA256SUMS"
release_files
chmod 777 "$release"
release_refuses "nobody else's to write"
release_files
chmod 666 "$release/SHA256SUMS"
release_refuses "the release's SHA256SUMS must be a file of root's or nobody's"
release_files
chown "$((owner - 1))" "$release/$prefix.squashfs"
release_refuses "$prefix.squashfs must be root's or nobody's"
release_files
mv -- "$release/$prefix.squashfs" "$release/real.squashfs"
ln -s real.squashfs "$release/$prefix.squashfs"
release_refuses "is missing a regular $prefix.squashfs"
release_files
release_refuses "$prefix.squashfs differs from the release's SHA256SUMS" swap
echo 'A changed, missing, unlisted, open or linked release image, or one swapped after the check, was refused, and the admitted image kept'
release_files
install_release
record_ok
echo 'The release installed again once its files were right'
PROOF
