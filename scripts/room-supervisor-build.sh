#!/usr/bin/env bash
# Build the room image's static supervisor for a release (#517): inside a
# build root made from the image's own Debian snapshot (room-image.sh
# build-root), so its C toolchain (gcc, glibc's libc.a, libgcc, binutils)
# is pinned, not the build machine's. Rust is the pinned toolchain, read-only.
#
#   sudo scripts/room-supervisor-build.sh OUT_DIR BUILD_USER RUST_SYSROOT CARGO_HOME
#
# Runs as root from the repository's root, to make the build root and its
# mounts. Cargo never runs as root: the crates are fetched on the host as
# BUILD_USER (the one network step), then built inside the root as
# BUILD_USER with no network, its own mount and pid namespaces, no
# privileges to gain, and the source and toolchain read-only. The link line
# is kept, and every file it names must come from the build root or Rust.
set -euo pipefail
export LC_ALL=C TZ=UTC
umask 022

die() { echo "room supervisor: $*" >&2; exit 1; }
[[ $# == 4 && $EUID == 0 ]] || die 'usage: sudo room-supervisor-build.sh OUT_DIR BUILD_USER RUST_SYSROOT CARGO_HOME'
out=$(realpath -m -- "$1")
user=$2
sysroot=$(realpath -e -- "$3")
cargo_home=$(realpath -e -- "$4")
repo=$(realpath -e -- "$(dirname -- "$0")/..")
uid=$(id -u -- "$user")
gid=$(id -g -- "$user")
[[ $uid != 0 ]] || die 'the build user must not be root'
[[ -x $sysroot/bin/rustc && -x $sysroot/bin/cargo ]] || die "no Rust toolchain at $sysroot"
[[ $(stat -c %u -- "$cargo_home") == "$uid" ]] || die 'CARGO_HOME must be the build user'"'"'s'
target=x86_64-unknown-linux-gnu
package=(--locked --target "$target" -p hatchling-room --features room-supervisor)

tmp=$(mktemp -d)
trap 'umount -R -- "$tmp/root" 2>/dev/null || true; rm -rf --one-file-system -- "$tmp"' EXIT
install -d -o "$uid" -g "$gid" -- "$tmp/target"
mkdir -p -- "$out"

# The build root, from the snapshot, its C toolchain checked against the pins.
bash "$repo/scripts/room-image.sh" build-root "$tmp/root"

as_user() { runuser -u "$user" -- env -i PATH="$sysroot/bin:/usr/bin:/bin" HOME="$tmp" LC_ALL=C TZ=UTC CARGO_HOME="$cargo_home" RUSTC="$sysroot/bin/rustc" "$@"; }
# The crates, on the host: the only step with a network.
as_user "$sysroot/bin/cargo" fetch --locked --manifest-path "$repo/Cargo.toml" > /dev/null
# The supervisor runs in the agent's room: it must not reach the trusted side.
as_user "$sysroot/bin/cargo" tree --offline "${package[@]}" --manifest-path "$repo/Cargo.toml" \
    --edges normal,build --prefix none > "$tmp/graph"
if grep -E '^hatchling-(broker|vault|egress|isolation|fido2|runtime) ' "$tmp/graph"; then
    die 'the room supervisor reaches the trusted side'
fi

flags=(
    -Ctarget-feature=+crt-static
    -Clinker=/usr/bin/x86_64-linux-gnu-gcc
    --remap-path-prefix=/cargo=/cargo
    --remap-path-prefix=/hatchling=/hatchling
    --print=link-args
)
encoded=$(IFS=$'\x1f'; echo "${flags[*]}")
for dir in hatchling rust cargo target tmp; do install -d -- "$tmp/root/$dir"; done
unshare --mount --net --pid --fork --propagation private bash -euo pipefail -c '
    root=$1 repo=$2 sysroot=$3 cargo=$4 target=$5 uid=$6 gid=$7 encoded=$8
    ro() {
        mount --bind -- "$1" "$2"
        mount -o remount,bind,ro,nosuid,nodev -- "$2"
    }
    ro "$repo" "$root/hatchling"
    ro "$sysroot" "$root/rust"
    # Cargo keeps its package lock in CARGO_HOME: the build user'"'"'s own, fresh.
    mount --bind -o nosuid,nodev -- "$cargo" "$root/cargo"
    mount --bind -o nosuid,nodev -- "$target" "$root/target"
    mount -t tmpfs -o mode=1777,nosuid,nodev,noexec tmpfs "$root/tmp"
    mount -t proc -o nosuid,nodev,noexec proc "$root/proc"
    : > "$root/dev/null"
    mount --bind -- /dev/null "$root/dev/null"
    if setpriv --reuid="$uid" --regid="$gid" --clear-groups touch "$root/hatchling/.writable" 2>/dev/null; then
        echo "room supervisor: the source is writable in the build root" >&2
        exit 1
    fi
    exec chroot "$root" setpriv --reuid="$uid" --regid="$gid" --clear-groups --no-new-privs --bounding-set=-all \
        env -i PATH=/rust/bin:/usr/bin:/bin HOME=/tmp LC_ALL=C TZ=UTC TMPDIR=/tmp CARGO_HOME=/cargo \
        RUSTC=/rust/bin/rustc CARGO_ENCODED_RUSTFLAGS="$encoded" \
        /rust/bin/cargo build --offline --locked --release --target x86_64-unknown-linux-gnu \
        -p hatchling-room --features room-supervisor --bin hatchling-room \
        --manifest-path /hatchling/Cargo.toml --target-dir /target
' _ "$tmp/root" "$repo" "$sysroot" "$cargo_home" "$tmp/target" "$uid" "$gid" "$encoded" > "$tmp/build.log" 2>&1 ||
    { tail -40 "$tmp/build.log" >&2; die 'the build failed'; }

# The supervisor's link: every file it names from the build root or Rust.
grep -a -- '"/usr/bin/x86_64-linux-gnu-gcc"' "$tmp/build.log" | grep -a -- 'hatchling_room' | tail -1 > "$out/link-args.txt"
[[ -s $out/link-args.txt ]] || die 'no link line for the supervisor'
if tr ' ' '\n' < "$out/link-args.txt" | tr -d '"' | grep -E '^/' | grep -Ev '^/(rust|target|usr|tmp)/'; then
    die 'the supervisor'"'"'s link names a file outside the build root and the Rust toolchain'
fi
if grep -Eq -- '-fuse-ld|gcc-ld' "$out/link-args.txt"; then
    die 'expected the build root'"'"'s GNU linker, not another'
fi
binary=$tmp/target/$target/release/hatchling-room
install -m 0755 -o "$uid" -g "$gid" -- "$binary" "$out/hatchling-room"
cp -- "$tmp/graph" "$out/graph.txt"
{
    dpkg-query --admindir="$tmp/root/var/lib/dpkg" -W -f='${Package}\t${Version}\n'
} > "$out/build-root-packages.txt"
chown -R "$uid:$gid" -- "$out"
readelf -p .comment "$out/hatchling-room" | sed -n 's/^ *\[ *[0-9a-f]*\] *//p'
sha256sum -- "$out/hatchling-room"
