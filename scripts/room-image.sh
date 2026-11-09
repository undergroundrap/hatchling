#!/usr/bin/env bash
# Build and compare the x86_64 room's read-only system image.
set -euo pipefail
export LC_ALL=C TZ=UTC
umask 022

snapshot=20261107T000000Z
node_version=24.22.2
epoch=${SOURCE_DATE_EPOCH:-1794009600}
image=room-system-x86_64.squashfs
hashes=room-system-x86_64.verity
root_hash=room-system-x86_64.root-hash
manifest=room-system-x86_64.contents.txt
sbom=room-system-x86_64.sbom.json
assets=("$image" "$hashes" "$root_hash" "$manifest" "$sbom")
verity=(--no-superblock --format=1 --hash=sha256 --data-block-size=4096 --hash-block-size=4096)

die() { echo "room image: $*" >&2; exit 1; }
digest() { sha256sum < "$1" | cut -d ' ' -f 1; }

download() {
    local url=$1 expected=$2 file=$3
    curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
        --connect-timeout 15 --max-time 180 --retry 3 --output "$file" "$url"
    [[ $(digest "$file") == "$expected" ]] || die "checksum mismatch for $url"
}

check_debian_indexes() (
    cd "$1/var/lib/apt/lists"
    sha256sum --check --strict <<EOF
0584fba32e13e0ab8285fb16c27adea1ec03a73669c18702821094fd6ca86675  snapshot.debian.org_archive_debian_${snapshot}_dists_trixie_InRelease
478e896e696eab873d40f0a31937edeec37192f2a35a26f97a04744805fd1521  snapshot.debian.org_archive_debian-security_${snapshot}_dists_trixie-security_InRelease
EOF
)

verify() {
    local dir=$1 expected=$2 salt
    [[ $expected =~ ^[0-9a-f]{64}$ ]] || die 'expected a trusted SHA-256 verity root'
    [[ -s $dir/$image && -s $dir/$hashes ]] || die 'missing image or verity tree'
    # The salt is public and derived from the image to make formatting repeatable.
    salt=$(digest "$dir/$image")
    veritysetup verify "${verity[@]}" --salt "$salt" "$dir/$image" "$dir/$hashes" "$expected"
}

build() {
    local out=$1 root name version url checksum wheel python_dir node_archive
    [[ $(uname -m) == x86_64 && $EUID == 0 ]] || die 'build needs an x86_64 Linux root environment'
    mkdir -- "$out"
    tmp=$(mktemp -d)
    trap 'rm -rf -- "$tmp"' EXIT
    root=$tmp/root

    local packages=(
        bash coreutils git ripgrep jq sqlite3 zip tar make diffutils libstdc++6
        python3 python3-pypdf python3-reportlab python3-openpyxl python3-docx
        python3-numpy python3-pandas python3-cryptography python3-charset-normalizer
        python3-xlsxwriter python3-lxml python3-typing-extensions
    )
    local keyring=/usr/share/keyrings/debian-archive-keyring.gpg
    export SOURCE_DATE_EPOCH=$epoch
    ROOM_IMAGE_SCRIPT=$(realpath -- "$0")
    export ROOM_IMAGE_SCRIPT
    # APT checks the signature and .deb digests. Pin the exact signed indexes
    # it used, after download and before mmdebstrap runs package scripts.
    # shellcheck disable=SC2016
    mmdebstrap --mode=root --variant=minbase --architectures=amd64 --keyring="$keyring" \
        --aptopt='APT::Install-Recommends "false"' --aptopt='Acquire::Retries "3"' \
        --extract-hook='bash "$ROOM_IMAGE_SCRIPT" check-debian-indexes "$1"' \
        --include="${packages[*]}" trixie "$root" \
        "deb [check-valid-until=no] https://snapshot.debian.org/archive/debian/$snapshot/ trixie main" \
        "deb [check-valid-until=no] https://snapshot.debian.org/archive/debian-security/$snapshot/ trixie-security main"

    node_archive=node-v$node_version-linux-x64.tar.xz
    download "https://nodejs.org/dist/v$node_version/$node_archive" \
        48cc0e21e048790e54c0cf5749c2725005313d652651c40b7a21feb9697aaf75 "$tmp/$node_archive"
    tar -xJf "$tmp/$node_archive" -C "$tmp" "${node_archive%.tar.xz}/bin/node" "${node_archive%.tar.xz}/LICENSE"
    install -Dm0755 "$tmp/${node_archive%.tar.xz}/bin/node" "$root/usr/local/bin/node"
    install -Dm0644 "$tmp/${node_archive%.tar.xz}/LICENSE" "$root/usr/local/share/doc/nodejs/LICENSE"

    # Only installed library files are needed. Extract the fixed wheels without executing their code.
    python_dir=$root/usr/local/lib/python3.13/dist-packages
    mkdir -p "$python_dir"
    while read -r name version checksum url; do
        wheel=$tmp/${url##*/}
        download "$url" "$checksum" "$wheel"
        python3 -m zipfile -e "$wheel" "$python_dir"
        printf '%s\t%s\t%s\t%s\n' "$name" "$version" amd64 pypi >> "$tmp/packages"
    done < "$(dirname -- "$0")/room-image-wheels.lock"

    # Strip machine state and privilege metadata from the filesystem the cell receives.
    rm -f -- "$root/etc/resolv.conf" "$root/etc/hostname" "$root/etc/hosts" \
        "$root/etc/machine-id" "$root/var/lib/dbus/machine-id"
    find "$root/var/log" -type f -delete
    find "$root" -xdev -type f -perm /6000 -exec chmod a-s -- {} +
    find "$root/dev" -mindepth 1 -delete
    install -d -m 0755 "$root/workspace" "$root/session" "$root/proc" "$root/dev"

    dpkg-query --admindir="$root/var/lib/dpkg" -W \
        -f='${db:Status-Status}\t${Package}\t${Version}\t${Architecture}\n' |
        awk -F '\t' '$1 == "installed" {print $2 "\t" $3 "\t" $4 "\tdebian"}' >> "$tmp/packages"
    printf 'nodejs\t%s\tamd64\tnodejs\n' "$node_version" >> "$tmp/packages"
    sort -o "$tmp/packages" "$tmp/packages"

    # The stock WSL kernel supports SquashFS gzip, but some versions lack zstd.
    env -u SOURCE_DATE_EPOCH mksquashfs "$root" "$out/$image" -noappend -all-root -no-xattrs \
        -processors 1 -comp gzip -repro-time "$epoch" -no-progress
    local image_digest
    image_digest=$(digest "$out/$image")
    veritysetup format "${verity[@]}" --salt "$image_digest" --root-hash-file "$out/$root_hash" \
        "$out/$image" "$out/$hashes"
    chmod 0644 "$out/$root_hash" "$out/$hashes"
    verify "$out" "$(cat "$out/$root_hash")"

    {
        printf 'Hatchling room system image (x86_64)\nDebian snapshot %s\nImage SHA-256 %s\n\n' "$snapshot" "$image_digest"
        printf 'Name\tVersion\tArchitecture\tSource\n'
        cat "$tmp/packages"
    } > "$out/$manifest"
    python3 "$(dirname -- "$0")/room-image-sbom.py" "$tmp/packages" "$image_digest" > "$out/$sbom"
}

case ${1:-} in
    build) [[ $# == 2 ]] || die 'usage: room-image.sh build NEW_DIRECTORY'; build "$2" ;;
    check-debian-indexes) [[ $# == 2 ]] || die 'missing Debian bootstrap root'; check_debian_indexes "$2" ;;
    compare)
        [[ $# == 3 ]] || die 'usage: room-image.sh compare FIRST_DIRECTORY SECOND_DIRECTORY'
        for name in "${assets[@]}"; do
            cmp -s "$2/$name" "$3/$name" || die "missing or different build output: $name"
        done
        verify "$2" "$(cat "$2/$root_hash")"
        printf 'Byte-identical room image SHA-256 %s\n' "$(digest "$2/$image")"
        ;;
    verify) [[ $# == 3 ]] || die 'usage: room-image.sh verify DIRECTORY TRUSTED_ROOT_HASH'; verify "$2" "$3" ;;
    *) die 'expected build, compare, or verify' ;;
esac
