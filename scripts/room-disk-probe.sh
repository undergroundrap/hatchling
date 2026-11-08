#!/usr/bin/env bash
# The room disk probe (0.9): can this machine, as root, do what Hatchling's
# room disk will? Each step is the one the spawner will take, by hand:
#
#   - a 64 MiB sparse image, made LUKS2 (aes-xts-plain64, a 512-bit key,
#     PBKDF2 at its floor), with the key only ever in a pipe;
#   - opened, made ext4, and mounted nodev,nosuid,noexec;
#   - a file written, and a program written into it refused;
#   - unmounted and closed, with what was written nowhere in the raw image;
#   - a wrong key refused, mapping nothing;
#   - the key slot changed to a new key, given through /dev/fd from a pipe,
#     without rewriting the image: the old key then fails and the new one
#     opens.
#
# Run as root: sudo bash scripts/room-disk-probe.sh. It needs cryptsetup
# (cryptsetup-bin) and the dm_crypt module. Every step has a timeout, any
# failure stops it, and it says which. It cleans up after itself whatever
# happens: the mount, the mapping, and its own directory.
set -euo pipefail

fail() { echo "room disk probe: FAILED: $*" >&2; exit 1; }
say() { echo "== $*"; }

[ "$(id -u)" = 0 ] || fail "run it as root"
command -v cryptsetup > /dev/null || fail "no cryptsetup: install cryptsetup-bin"
command -v mkfs.ext4 > /dev/null || fail "no mkfs.ext4"

dir=$(mktemp -d /var/tmp/hatchling-room-probe.XXXXXX)
chmod 700 "$dir"
image=$dir/disk.img
mnt=$dir/mnt
name=hatchling-room-probe-$$
mkdir -m 700 "$mnt"
cleanup() {
    if mountpoint -q "$mnt"; then umount "$mnt" || true; fi
    if [ -e "/dev/mapper/$name" ]; then cryptsetup close "$name" || true; fi
    rm -rf -- "$dir"
}
trap cleanup EXIT

# Two random keys, as hex in this shell's memory alone: each reaches
# cryptsetup only as raw bytes through a pipe.
hex() { python3 -c 'import secrets; print(secrets.token_hex(64))'; }
raw() { python3 -c 'import sys; sys.stdout.buffer.write(bytes.fromhex(sys.argv[1]))' "$1"; }
key=$(hex)
new_key=$(hex)
uuid=$(python3 -c 'import uuid; print(uuid.uuid4())')

say "this machine: $(uname -r), $(cryptsetup --version)"
timeout 30 modprobe dm_crypt || fail "dm_crypt doesn't load"
timeout 30 modprobe loop || true
[ -e /dev/mapper/control ] || fail "no /dev/mapper/control"

say "a 64 MiB sparse image"
truncate -s 64M "$image" || fail "truncate"
chmod 600 "$image"
[ "$(du -k "$image" | cut -f1)" -lt 1024 ] || fail "the new image isn't sparse"

say "LUKS2, the key from a pipe"
raw "$key" | timeout 120 cryptsetup luksFormat --batch-mode --type luks2 \
    --cipher aes-xts-plain64 --key-size 512 --hash sha256 \
    --pbkdf pbkdf2 --pbkdf-force-iterations 1000 --offset 32768 \
    --uuid "$uuid" --label hatchling-room-probe \
    --key-file - --keyfile-size 64 "$image" || fail "luksFormat"
[ "$(timeout 30 cryptsetup luksUUID "$image")" = "$uuid" ] || fail "the image's UUID isn't the one given"

say "opened"
raw "$key" | timeout 60 cryptsetup open --type luks2 --key-file - --keyfile-size 64 \
    "$image" "$name" || fail "open"

say "ext4"
timeout 120 mkfs.ext4 -q -m 0 -E nodiscard,root_owner=0:0 -L hatchling-room \
    "/dev/mapper/$name" || fail "mkfs.ext4"

say "mounted nodev,nosuid,noexec"
timeout 60 mount -t ext4 -o nodev,nosuid,noexec,errors=remount-ro "/dev/mapper/$name" "$mnt" \
    || fail "mount"
options=$(findmnt -n -o OPTIONS --target "$mnt")
for flag in nodev nosuid noexec; do
    case ",$options," in
        *",$flag,"*) ;;
        *) fail "mounted without $flag: $options" ;;
    esac
done

say "written to, and a program in it refused"
canary=hatchling-room-probe-canary-$uuid
echo "$canary" > "$mnt/canary" || fail "write"
printf '#!/bin/sh\nexit 0\n' > "$mnt/program"
chmod 755 "$mnt/program"
if "$mnt/program" 2> /dev/null; then fail "a program on a noexec mount ran"; fi
timeout 60 sync

say "unmounted and closed"
timeout 60 umount "$mnt" || fail "umount"
timeout 60 cryptsetup close "$name" || fail "close"
[ ! -e "/dev/mapper/$name" ] || fail "the mapping is still there after close"
if grep -aqF "$canary" "$image"; then fail "what was written is in the raw image"; fi
say "the image takes $(du -k "$image" | cut -f1) KiB of 65536"

say "a wrong key is refused, and maps nothing"
if raw "$new_key" | timeout 60 cryptsetup open --type luks2 --key-file - --keyfile-size 64 \
    "$image" "$name" 2> /dev/null; then
    fail "a wrong key opened the image"
fi
[ ! -e "/dev/mapper/$name" ] || fail "a wrong key left a mapping"

say "the key slot changed, the new key through /dev/fd, the image not rewritten"
data_before=$(tail -c +$((16 * 1024 * 1024 + 1)) "$image" | sha256sum)
raw "$key" | timeout 120 cryptsetup luksAddKey --batch-mode --pbkdf pbkdf2 \
    --pbkdf-force-iterations 1000 --key-file - --keyfile-size 64 --new-keyfile-size 64 \
    "$image" /dev/fd/3 3< <(raw "$new_key") || fail "luksAddKey with the new key from a pipe"
raw "$key" | timeout 60 cryptsetup luksRemoveKey --batch-mode --key-file - --keyfile-size 64 \
    "$image" || fail "luksRemoveKey of the old key"
if raw "$key" | timeout 60 cryptsetup open --test-passphrase --key-file - --keyfile-size 64 \
    "$image" 2> /dev/null; then
    fail "the old key still opens the image"
fi
raw "$new_key" | timeout 60 cryptsetup open --test-passphrase --key-file - --keyfile-size 64 \
    "$image" || fail "the new key doesn't open the image"
[ "$(tail -c +$((16 * 1024 * 1024 + 1)) "$image" | sha256sum)" = "$data_before" ] \
    || fail "changing the key slot rewrote the image's data"

say "reopened with the new key, and what was written is there"
raw "$new_key" | timeout 60 cryptsetup open --type luks2 --key-file - --keyfile-size 64 \
    "$image" "$name" || fail "open with the new key"
timeout 60 mount -t ext4 -o nodev,nosuid,noexec "/dev/mapper/$name" "$mnt" || fail "mount again"
[ "$(cat "$mnt/canary")" = "$canary" ] || fail "what was written didn't come back"
timeout 60 umount "$mnt" || fail "umount again"
timeout 60 cryptsetup close "$name" || fail "close again"

echo "room disk probe: every step passed"
