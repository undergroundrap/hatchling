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
supervisor_path=usr/local/libexec/hatchling-room
assets=("$image" "$hashes" "$root_hash" "$manifest" "$sbom")
verity=(--no-superblock --format=1 --hash=sha256 --data-block-size=4096 --hash-block-size=4096)
sources=(
    "deb [check-valid-until=no] https://snapshot.debian.org/archive/debian/$snapshot/ trixie main"
    "deb [check-valid-until=no] https://snapshot.debian.org/archive/debian-security/$snapshot/ trixie-security main"
)

# The static supervisor's C toolchain (#517): built from the same snapshot,
# so a release's supervisor can be rebuilt byte for byte whatever machine
# builds it. The packages its bytes come from, and the files a static link
# takes, each pinned: a build root that differs is refused.
toolchain_packages=$'binutils-x86-64-linux-gnu\t2.44-3\ngcc-14\t14.2.0-19\nlibc6-dev\t2.41-12+deb13u4\nlibgcc-14-dev\t14.2.0-19'
toolchain_files() {
    cat <<EOF
679612af043ab6a3eb9af24136b8d0c9ba1e10114f047271386ee7fa37031326  usr/lib/x86_64-linux-gnu/libc.a
4ca31259aee8415b60fe566c00cecfa5858f1103296b8043f175ccf2489f00ec  usr/lib/x86_64-linux-gnu/rcrt1.o
78acef26a7007f5320c98633376f2903e33895e694fbc2b91d189b38addbca8a  usr/lib/x86_64-linux-gnu/crti.o
121f2a5f12b13471dd8c7dabe3ff334df08540c270564d1a2b3c47ecbd8d3101  usr/lib/x86_64-linux-gnu/crtn.o
37af2040527c75078f900a29ea4a71b22576aa5c26f1f29af7b5935cebbe571e  usr/lib/gcc/x86_64-linux-gnu/14/crtbeginS.o
8981687575c10a2e2146947bce387769cbd118f5b8c5dda489f514dc7d2ed4c7  usr/lib/gcc/x86_64-linux-gnu/14/crtendS.o
c53c33f587ceaf0fc9a4b6bb5c5437082068f64ac70eb3b7a0d899e7ac5ffd3d  usr/lib/gcc/x86_64-linux-gnu/14/libgcc.a
4622efa3af50eaea4fbb4bc520ada4c88008c5343e0ad306633821fd2432102d  usr/lib/gcc/x86_64-linux-gnu/14/libgcc_eh.a
EOF
}

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

build_root() {
    local root=$1 keyring=/usr/share/keyrings/debian-archive-keyring.gpg
    [[ $(uname -m) == x86_64 && $EUID == 0 ]] || die 'build-root needs an x86_64 Linux root environment'
    [[ ! -e $root ]] || die "the build root must be new: $root"
    ROOM_IMAGE_SCRIPT=$(realpath -- "$0")
    export ROOM_IMAGE_SCRIPT SOURCE_DATE_EPOCH=$epoch
    # shellcheck disable=SC2016
    mmdebstrap --quiet --mode=root --variant=minbase --architectures=amd64 --keyring="$keyring" \
        --aptopt='APT::Install-Recommends "false"' --aptopt='Acquire::Retries "3"' \
        --extract-hook='bash "$ROOM_IMAGE_SCRIPT" check-debian-indexes "$1"' \
        --include=gcc,libc6-dev,binutils trixie "$root" "${sources[@]}"
    local packages
    packages=$(dpkg-query --admindir="$root/var/lib/dpkg" -W -f='${Package}\t${Version}\n' \
        gcc-14 libgcc-14-dev libc6-dev binutils-x86-64-linux-gnu)
    [[ $packages == "$toolchain_packages" ]] || die "the build root's C toolchain isn't the pinned one: $packages"
    (cd "$root" && toolchain_files | sha256sum --check --strict --quiet) ||
        die "the build root's C files aren't the pinned ones"
}

stage_supervisor() {
    local source=$1 binary=$2 metadata=$3
    [[ -f $source && ! -L $source ]] || die 'expected a regular supervisor binary'
    install -m 0755 -- "$source" "$binary"
    # The previous job supplies bytes, not authority. Parse the ELF as
    # nobody through a read-only fd into this job's private staging area.
    if ! env -i PATH=/usr/bin:/bin LC_ALL=C setpriv --reuid=65534 --regid=65534 \
        --clear-groups --no-new-privs readelf -h -l -d /proc/self/fd/0 < "$binary" > "$metadata" ||
        ! grep -Eq 'Machine: +Advanced Micro Devices X86-64$' "$metadata" ||
        ! grep -Eq 'Type: +(EXEC|DYN) ' "$metadata" ||
        grep -Eq 'INTERP|NEEDED' "$metadata"; then
        die 'expected a static x86-64 supervisor ELF'
    fi
}

build() {
    local out=$1 supervisor=$2 root name version url checksum wheel python_dir node_archive
    [[ $(uname -m) == x86_64 && $EUID == 0 ]] || die 'build needs an x86_64 Linux root environment'
    local script_dir
    script_dir=$(dirname -- "$(realpath -- "$0")")
    tmp=$(mktemp -d)
    trap 'rm -rf -- "$tmp"' EXIT
    stage_supervisor "$supervisor" "$tmp/hatchling-room" "$tmp/supervisor-elf"
    mkdir -- "$out"
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
        --include="${packages[*]}" trixie "$root" "${sources[@]}"

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
        python3 -I -m zipfile -e "$wheel" "$python_dir"
        printf '%s\t%s\t%s\t%s\n' "$name" "$version" amd64 pypi >> "$tmp/packages"
    done < "$(dirname -- "$0")/room-image-wheels.lock"

    # Strip machine state and privilege metadata from the filesystem the cell receives.
    rm -f -- "$root/etc/resolv.conf" "$root/etc/hostname" "$root/etc/hosts" \
        "$root/etc/machine-id" "$root/var/lib/dbus/machine-id"
    find "$root/var/log" -type f -delete
    find "$root" -xdev -type f -perm /6000 -exec chmod a-s -- {} +
    find "$root/dev" -mindepth 1 -delete
    install -d -m 0755 "$root/workspace" "$root/session" "$root/proc" "$root/dev"
    # Only the image can supply the launcher executable. Copy in the static
    # build before verity seals it, and record the digest of the installed bytes.
    install -Dm0755 -- "$tmp/hatchling-room" "$root/$supervisor_path"

    dpkg-query --admindir="$root/var/lib/dpkg" -W \
        -f='${db:Status-Status}\t${Package}\t${Version}\t${Architecture}\n' |
        awk -F '\t' '$1 == "installed" {print $2 "\t" $3 "\t" $4 "\tdebian"}' >> "$tmp/packages"
    printf 'nodejs\t%s\tamd64\tnodejs\n' "$node_version" >> "$tmp/packages"
    version=$(python3 -I -c 'import sys,tomllib; print(tomllib.load(sys.stdin.buffer)["workspace"]["package"]["version"])' \
        < "$script_dir/../Cargo.toml")
    printf 'hatchling-room\t%s\tamd64\thatchling\t%s\n' "$version" \
        "$(digest "$root/$supervisor_path")" >> "$tmp/packages"
    sort -o "$tmp/packages" "$tmp/packages"
    # What the supervisor was built with (#517): from this script's pins,
    # never from the job that built it. Not installed in the image.
    {
        printf '%s\tamd64\tdebian\n' "${toolchain_packages//$'\n'/$'\tamd64\tdebian\n'}"
    } > "$tmp/built-with"

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
        printf 'Name\tVersion\tArchitecture\tSource\tSHA-256 (when recorded)\n'
        cat "$tmp/packages"
        printf '\nThe supervisor was built from the same snapshot with (not installed in the image):\n'
        cat "$tmp/built-with"
        printf '\nIts C files, SHA-256:\n'
        toolchain_files
    } > "$out/$manifest"
    python3 -I "$script_dir/room-image-sbom.py" "$tmp/packages" "$image_digest" "$tmp/built-with" > "$out/$sbom"
}

case ${1:-} in
    build) [[ $# == 3 ]] || die 'usage: room-image.sh build NEW_DIRECTORY SUPERVISOR_BINARY'; build "$2" "$3" ;;
    check-debian-indexes) [[ $# == 2 ]] || die 'missing Debian bootstrap root'; check_debian_indexes "$2" ;;
    build-root) [[ $# == 2 ]] || die 'usage: room-image.sh build-root NEW_DIRECTORY'; build_root "$2" ;;
    compare)
        [[ $# == 3 ]] || die 'usage: room-image.sh compare FIRST_DIRECTORY SECOND_DIRECTORY'
        for name in "${assets[@]}"; do
            cmp -s "$2/$name" "$3/$name" || die "missing or different build output: $name"
        done
        verify "$2" "$(cat "$2/$root_hash")"
        printf 'Byte-identical room image SHA-256 %s\n' "$(digest "$2/$image")"
        ;;
    verify) [[ $# == 3 ]] || die 'usage: room-image.sh verify DIRECTORY TRUSTED_ROOT_HASH'; verify "$2" "$3" ;;
    *) die 'expected build, build-root, compare, or verify' ;;
esac
