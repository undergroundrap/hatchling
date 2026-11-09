#!/usr/bin/env bash
# Exercise the artifact in a disposable Linux mount namespace.
set -euo pipefail
export LC_ALL=C
[[ $# == 1 && $EUID == 0 ]] || { echo 'usage: room-image-probe.sh IMAGE_DIRECTORY (as root)' >&2; exit 64; }

dir=$1
image=$dir/room-system-x86_64.squashfs
hashes=$dir/room-system-x86_64.verity
trusted=$(cat "$dir/room-system-x86_64.root-hash")
salt=$(sha256sum < "$image" | cut -d ' ' -f 1)
builder=$(dirname -- "$0")/room-image.sh
verity=(--no-superblock --format=1 --hash=sha256 --data-block-size=4096 --hash-block-size=4096 --salt "$salt")
bash "$builder" verify "$dir" "$trusted"

tmp=$(mktemp -d)
mapping=hatchling-room-probe-${tmp##*/}
mounted=false
dev_mounted=false
opened=false
cleanup() {
    local status=$?
    if "$dev_mounted"; then umount "$tmp/mount/dev" || exit 1; fi
    if "$mounted"; then umount "$tmp/mount" || exit 1; fi
    if "$opened"; then veritysetup close "$mapping" || exit 1; fi
    rm -rf -- "$tmp"
    exit "$status"
}
trap cleanup EXIT
mkdir "$tmp/mount"

veritysetup open "${verity[@]}" "$image" "$mapping" "$hashes" "$trusted"
opened=true
mount -t squashfs -o ro,nodev,nosuid "/dev/mapper/$mapping" "$tmp/mount"
mounted=true
flags=$(findmnt --noheadings --output OPTIONS --mountpoint "$tmp/mount")
for option in ro nodev nosuid; do
    [[ ,$flags, == *,$option,* ]] || { echo "missing mount flag: $option" >&2; exit 1; }
done
# Model the room's device mount with only the three specified devices.
mount -t tmpfs -o nosuid,noexec,size=64k tmpfs "$tmp/mount/dev"
dev_mounted=true
mknod -m 0666 "$tmp/mount/dev/null" c 1 3
mknod -m 0666 "$tmp/mount/dev/zero" c 1 5
mknod -m 0666 "$tmp/mount/dev/urandom" c 1 9
mount -o remount,ro,nosuid,noexec "$tmp/mount/dev"
run_image() {
    env -i PATH=/usr/local/bin:/usr/bin:/bin LC_ALL=C \
        "$(command -v chroot)" --userspec=65534:65534 --groups='' "$tmp/mount" "$@"
}
run_image /usr/local/bin/node -e 'console.log(process.version, require("node:path").basename("/session/test"))'
run_image /usr/bin/python3 -I -B - <<'PY'
from importlib.metadata import version
from io import BytesIO

import docx
import numpy
import openpyxl
import pandas
import pdfplumber
import PIL.Image
import pptx
import pypdf
import reportlab.pdfgen.canvas

pdf = BytesIO()
writer = pypdf.PdfWriter()
writer.add_blank_page(width=72, height=72)
writer.write(pdf)
with pdfplumber.open(pdf) as document:
    assert len(document.pages) == 1
assert numpy.array([1, 2]).sum() == 3
assert pandas.Series([1, 2]).sum() == 3
print("Python libraries imported and PDF and array probes passed")
print("Pillow", version("Pillow"))
PY
run_image /usr/bin/git --version
run_image /usr/bin/rg --version
[[ $(run_image /usr/bin/sqlite3 :memory: 'select 42;') == 42 ]]
umount "$tmp/mount/dev"
dev_mounted=false
umount "$tmp/mount"
mounted=false
veritysetup close "$mapping"
opened=false

# Use the original salt and root with one byte changed in the data device.
# A failure here must come from the verity check against the original tree.
cp --reflink=auto -- "$image" "$tmp/tampered"
printf '\0' | dd of="$tmp/tampered" bs=1 count=1 conv=notrunc status=none
if veritysetup verify "${verity[@]}" "$tmp/tampered" "$hashes" "$trusted"; then
    echo 'verity accepted a tampered image' >&2
    exit 1
fi
veritysetup open "${verity[@]}" "$tmp/tampered" "$mapping" "$hashes" "$trusted"
opened=true
[[ -b /dev/mapper/$mapping ]]
if dd if="/dev/mapper/$mapping" of=/dev/null bs=4096 count=1 status=none 2> "$tmp/read-error"; then
    echo 'kernel verity mapping returned a tampered block' >&2
    exit 1
fi
grep -F 'Input/output error' "$tmp/read-error"
veritysetup close "$mapping"
opened=false
cp -- "$hashes" "$tmp/room-system-x86_64.verity"
if bash "$builder" verify "$tmp" "$trusted"; then
    echo 'verification accepted a missing image' >&2
    exit 1
fi
echo 'Verified kernel mount and rejected tampered and missing images'
