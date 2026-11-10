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
        room_prepare
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
refuses 'the bundled Room image failed verity verification'
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
PROOF
