#!/usr/bin/env bash
# Run the room image's own programs at everyday work under the Room's
# syscall profile, in a root laid out as a Room's is: the kernel-verified
# image read-only, a workspace on this machine's disk and a session in
# memory, both without exec, the Room's three devices, a /proc of
# processes only, and a user the image's files don't name, as they don't
# name the Room's account.
#
#   sudo bash scripts/room-image-tools-proof.sh IMAGE_DIRECTORY TEST_BINARY
#
# TEST_BINARY is hatchling-sys's `room_image_tools` test, built:
#   cargo test --locked -p hatchling-sys --test room_image_tools --no-run
# It does what the Room's launcher does for each command (root given up,
# the profile installed as the program starts) and says which commands
# didn't end as they must. Everything is mounted in a namespace of this
# script's own, so nothing stays mounted when it ends.
set -euo pipefail
export LC_ALL=C
[[ $# == 2 && $EUID == 0 ]] || { echo 'usage: room-image-tools-proof.sh IMAGE_DIRECTORY TEST_BINARY (as root)' >&2; exit 64; }
if [[ ${HATCHLING_ROOM_TOOLS_PROOF_NAMESPACE:-} != 1 ]]; then
    exec env HATCHLING_ROOM_TOOLS_PROOF_NAMESPACE=1 \
        unshare --mount --net --propagation private bash "$0" "$@"
fi

dir=$(realpath -- "$1")
proof=$(realpath -- "$2")
test -x "$proof"
image=$dir/room-system-x86_64.squashfs
hashes=$dir/room-system-x86_64.verity
trusted=$(cat "$dir/room-system-x86_64.root-hash")
salt=$(sha256sum < "$image" | cut -d ' ' -f 1)
builder=$(dirname -- "$0")/room-image.sh
verity=(--no-superblock --format=1 --hash=sha256 --data-block-size=4096 --hash-block-size=4096 --salt "$salt")
bash "$builder" verify "$dir" "$trusted"
# A user the image's files don't name.
user=4242

tmp=$(mktemp -d /var/tmp/hatchling-room-tools-proof.XXXXXXXX)
mapping=hatchling-room-tools-proof-${tmp##*.}
opened=false
unmount_after_drain() {
    local mountpoint=$1
    # A process just ended may still be going. Use ordinary unmount, so a
    # live holder can never look like closure.
    for _ in {1..40}; do
        if umount "$mountpoint" 2>/dev/null; then return 0; fi
        sleep 0.05
    done
    umount "$mountpoint"
}
cleanup() {
    local status=$?
    trap - EXIT
    # A program a command started may outlive it (what `timeout` runs,
    # when `timeout` is ended first), and would hold the mounts open: end
    # whatever still has this root as its own.
    local entry
    for entry in /proc/[0-9]*; do
        if [[ $(readlink "$entry/root" 2>/dev/null) == "$tmp/mount" ]]; then
            kill -KILL "${entry#/proc/}" 2>/dev/null || true
        fi
    done
    # The mounts are this namespace's own. Take them down here so the
    # mapping under them can be closed, which no namespace does for us.
    local point
    for point in session workspace proc dev ''; do
        if mountpoint --quiet "$tmp/mount/$point"; then
            unmount_after_drain "$tmp/mount/$point" || status=1
        fi
    done
    if "$opened"; then veritysetup close "$mapping" || status=1; fi
    rm -rf -- "$tmp"
    exit "$status"
}
trap cleanup EXIT
mkdir "$tmp/mount" "$tmp/workspace"

veritysetup open "${verity[@]}" "$image" "$mapping" "$hashes" "$trusted"
opened=true
mount -t squashfs -o ro,nodev,nosuid "/dev/mapper/$mapping" "$tmp/mount"
grep -q "^[^:]*:[^:]*:$user:" "$tmp/mount/etc/passwd" && { echo "the image names user $user" >&2; exit 1; }

# The Room's device mount: its three devices and nothing else.
mount -t tmpfs -o nosuid,noexec,size=64k tmpfs "$tmp/mount/dev"
mknod -m 0666 "$tmp/mount/dev/null" c 1 3
mknod -m 0666 "$tmp/mount/dev/zero" c 1 5
mknod -m 0666 "$tmp/mount/dev/urandom" c 1 9
mount -o remount,ro,nosuid,noexec "$tmp/mount/dev"
# The Room's /proc: processes, and nothing else of the machine. A program
# that reads the machine's memory or processors there finds what it finds
# in a Room, which is nothing.
mount -t proc -o ro,nosuid,nodev,noexec,subset=pid proc "$tmp/mount/proc"
# The workspace, on this machine's disk as the Room's is on its own, and
# the session, in memory: two file systems, neither one's files runnable.
chown "$user:$user" "$tmp/workspace"
mount --bind "$tmp/workspace" "$tmp/mount/workspace"
mount -o remount,bind,nosuid,nodev,noexec "$tmp/mount/workspace"
mount -t tmpfs -o "nosuid,nodev,noexec,size=512m,mode=0700,uid=$user,gid=$user" tmpfs "$tmp/mount/session"

HATCHLING_ROOM_IMAGE_ROOT=$tmp/mount HATCHLING_ROOM_IMAGE_USER=$user HATCHLING_REQUIRE_ROOM_IMAGE=1 \
    timeout --kill-after=15 900 "$proof"
