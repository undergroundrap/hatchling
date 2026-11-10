#!/usr/bin/env bash
# Kernel fixture for the mount plan, not a broker or release-image test.
# sudo bash scripts/room-mount-proof.sh TEST_BINARY [full|fast|writable]
# By default build a minimal SquashFS from the runner's static busybox.
# HATCHLING_ROOM_PROBE_IMAGE_DIR may name a separately built room image
# and its verity files for local replay; the installer verifies provenance.
set -euo pipefail
[[ $EUID == 0 ]]
probe=$(realpath "${1:?room mount test executable}")
mode=${2:-full}
case "$mode" in full|fast|writable) ;; *) exit 64 ;; esac
test -x "$probe"
umask 077
proof=$(mktemp -d /var/tmp/hatchling-room-mount-proof.XXXXXXXX)
tag=${proof##*.}
verity=hatchling-mount-proof-verity-$tag
luks=hatchling-mount-proof-luks-$tag
mapper_exists() {
    local name
    for name in /sys/block/dm-*/dm/name; do
        if [[ -r "$name" && $(<"$name") == "$1" ]]; then return 0; fi
    done
    return 1
}
cleanup() {
    local rc=$?
    local verified_removed=1
    trap - EXIT
    # Close devices from outside the test namespace, after unshare has
    # exited and the kernel has dropped all its mount references, even
    # if a broken private-mount guard propagated mounts inside it.
    if test -b "/dev/mapper/$luks"; then cryptsetup close "$luks" || rc=1; fi
    if test -b "/dev/mapper/$verity"; then veritysetup close "$verity" || rc=1; fi
    if [[ -f "$proof/verified-mapper" ]]; then
        local verified
        while IFS= read -r verified; do
            if [[ "$verified" =~ ^hatchling-room-verity-[0-9a-f]{32}$ ]]; then
                if mapper_exists "$verified"; then veritysetup close "$verified" || rc=1; fi
                if mapper_exists "$verified"; then verified_removed=0; rc=1; fi
            else
                verified_removed=0
                rc=1
            fi
        done < "$proof/verified-mapper"
    fi
    if (( verified_removed )) && ! test -b "/dev/mapper/$luks" && ! test -b "/dev/mapper/$verity"; then
        rm -rf --one-file-system -- "$proof" || rc=1
    fi
    exit "$rc"
}
trap cleanup EXIT
unshare --mount --propagation private bash -s -- "$probe" "$mode" "$proof" "$verity" "$luks" <<'PROOF'
set -euo pipefail
umask 077
probe=$1
mode=$2
proof=$3
verity=$4
luks=$5
mkdir "$proof/image" "$proof/disk" "$proof/stage"
printf 'outside-room\n' > "$proof/outside-canary"
if [[ $mode == writable ]]; then
    # SquashFS is inherently read-only: use a writable source to distinguish
    # the launcher's RDONLY flag from a restriction inherited from the source.
    mount -t tmpfs -o size=1M tmpfs "$proof/image"
    mkdir "$proof/image/"{workspace,session,proc,dev}
else
    opts=(--no-superblock --format=1 --hash=sha256 --data-block-size=4096 --hash-block-size=4096)
    if [[ -n ${HATCHLING_ROOM_PROBE_IMAGE_DIR:-} ]]; then
        prefix="$HATCHLING_ROOM_PROBE_IMAGE_DIR/room-system-x86_64"
    else
        prefix="$proof/fixture"
        mkdir -m 0755 "$proof/image-tree"
        mkdir -m 0755 "$proof/image-tree/"{bin,workspace,session,proc,dev}
        install -m 0755 /usr/bin/busybox "$proof/image-tree/bin/busybox"
        ln -s busybox "$proof/image-tree/bin/sh"
        ln -s busybox "$proof/image-tree/bin/head"
        mksquashfs "$proof/image-tree" "$prefix.squashfs" -noappend -all-root -no-progress -processors 2
        digest=$(sha256sum "$prefix.squashfs" | cut -d ' ' -f 1)
        veritysetup format "${opts[@]}" --salt "$digest" --root-hash-file "$prefix.root-hash" \
            "$prefix.squashfs" "$prefix.verity"
    fi
    digest=$(sha256sum "$prefix.squashfs" | cut -d ' ' -f 1)
    root_hash=$(cat "$prefix.root-hash")
    printf 'room fixture image %s; verity root %s\n' "$digest" "$root_hash"
    veritysetup verify "${opts[@]}" --salt "$digest" "$prefix.squashfs" "$prefix.verity" "$root_hash"
    veritysetup open "${opts[@]}" --salt "$digest" "$prefix.squashfs" "$verity" "$prefix.verity" "$root_hash"
    mount -t squashfs -o ro,nodev,nosuid "/dev/mapper/$verity" "$proof/image"
    mkdir "$proof/install"
    cp "$prefix.squashfs" "$proof/install/room-system-x86_64.squashfs"
    cp "$prefix.verity" "$proof/install/room-system-x86_64.verity"
fi
truncate -s 96M "$proof/fixture.luks"
head -c 32 /dev/urandom > "$proof/key"
cryptsetup luksFormat --batch-mode --type luks2 --pbkdf pbkdf2 --pbkdf-force-iterations 1000 \
    "$proof/fixture.luks" "$proof/key"
cryptsetup open "$proof/fixture.luks" "$luks" --key-file "$proof/key"
rm -- "$proof/key"
mkfs.ext4 -q "/dev/mapper/$luks"
# Deliberately no noexec here: inherited noexec would mask a broken room flag.
mount -o nodev,nosuid "/dev/mapper/$luks" "$proof/disk"
mkdir -m 0755 "$proof/disk/workspace" "$proof/disk/inbox"
mkdir "$proof/disk/workspace/"{host-mount,shadow}
chown 65534:65534 "$proof/disk/workspace"
mount -t tmpfs -o size=64K tmpfs "$proof/disk/workspace/host-mount"
printf 'host-only\n' > "$proof/disk/workspace/host-mount/canary"
printf inbox-content > "$proof/disk/inbox/fixture"
chmod 0644 "$proof/disk/inbox/fixture"
if [[ $mode != writable ]]; then
    install -m 0755 "$proof/image/bin/sh" "$proof/disk/workspace/payload"
fi
export HATCHLING_ROOM_MOUNT_FIXTURE=$proof HATCHLING_REQUIRE_ROOM_MOUNTS=1
# Begin with shared mounts *within* this already isolated test namespace.
# The room must make its cloned namespace private before pivoting.
mount --make-rshared /
run_probe() {
    local ipc=(--ipc)
    if [[ $1 == verified ]]; then ipc=(); fi
    HATCHLING_ROOM_MOUNT_MODE=$1 timeout 60 unshare --pid --fork --kill-child --net "${ipc[@]}" "$probe"
}
run_probe threads
if [[ $mode == writable ]]; then
    mkdir -m 0755 "$proof/disk/workspace/inbox"
    HATCHLING_ROOM_WRITABLE_IMAGE_FIXTURE=1 run_probe view
    test ! -e "$proof/image/room-mount-probe"
    echo 'Room root rejected writes with a writable source'
    exit
fi
ln -s shadow "$proof/disk/workspace/inbox"
HATCHLING_ROOM_BAD_INBOX=1 run_probe view
rm -- "$proof/disk/workspace/inbox"
mkdir -m 0755 "$proof/disk/workspace/inbox"
if [[ $mode == full ]]; then
    HATCHLING_ROOM_FILL_SCRATCH=1 run_probe view
    echo 'Room scratch write returned ENOSPC at 512 MiB'
else
    run_probe view
fi
test "$(cat "$proof/disk/workspace/marker")" = persistent
test "$(cat "$proof/disk/workspace/host-mount/canary")" = host-only
test ! -e "$proof/disk/inbox/refused"
if [[ $mode == full ]]; then run_probe view; fi
mount -o remount,nodev,nosuid,noexec "$proof/disk"
export HATCHLING_ROOM_PROOF_ROOT=$root_hash HATCHLING_ROOM_PROOF_DIGEST=$digest
HATCHLING_ROOM_PROOF_WORKSPACE_DEVICE=$(stat -Lc '%Hr:%Lr' "/dev/mapper/$luks")
export HATCHLING_ROOM_PROOF_WORKSPACE_DEVICE
mkdir "$proof/substitute"
mount --bind "$proof/disk/workspace" "$proof/substitute"
HATCHLING_ROOM_PROOF_SUBSTITUTE_VOLUME=1 \
    HATCHLING_ROOM_PROOF_SOURCE_ERROR='check room workspace mount' run_probe verified
umount "$proof/substitute"
mount --bind "$proof/disk" "$proof/disk/inbox"
HATCHLING_ROOM_PROOF_SOURCE_ERROR='pin room mount target' run_probe verified
umount "$proof/disk/inbox"
chmod 0775 "$proof/disk/inbox"
HATCHLING_ROOM_PROOF_SOURCE_ERROR='verify room inbox ownership' run_probe verified
chmod 0755 "$proof/disk/inbox"
chown 65534:65534 "$proof/disk/inbox"
HATCHLING_ROOM_PROOF_SOURCE_ERROR='verify room inbox ownership' run_probe verified
chown 0:0 "$proof/disk/inbox"
chmod 0777 "$proof/disk"
HATCHLING_ROOM_PROOF_SOURCE_ERROR='verify room volume root ownership' run_probe verified
chmod 0755 "$proof/disk"
run_probe verified
test -s "$proof/verified-mapper"
echo 'Room refused substituted sources and ran a program with its verified image and encrypted volume'
echo "Room mount proof passed ($mode)"
PROOF
