#!/usr/bin/env bash
# Build the room image's untrusted supervisor without running Cargo as root.
set -euo pipefail
export LC_ALL=C

[[ $# == 1 && $EUID != 0 ]] || { echo 'usage: room-image-supervisor.sh BUILD_DIRECTORY (unprivileged)' >&2; exit 64; }
output=$(realpath -m -- "$1")
cd "$(dirname -- "$0")/.."
mkdir -p -- "$output"
target=x86_64-unknown-linux-gnu
cargo_home=$(realpath -m -- "${CARGO_HOME:-$HOME/.cargo}")
export CARGO_ENCODED_RUSTFLAGS="-Ctarget-feature=+crt-static"$'\x1f'"--remap-path-prefix=$cargo_home=/cargo"$'\x1f'"--remap-path-prefix=$PWD=/hatchling"
package=(--locked --target "$target" -p hatchling-browser --features room-supervisor)
cargo tree "${package[@]}" --edges normal,build --prefix none > "$output/graph.txt"
if grep -E '^hatchling-(broker|vault|egress|isolation|fido2|runtime) ' "$output/graph.txt"; then
    echo 'The room supervisor reaches the trusted side' >&2
    exit 1
fi
cargo build "${package[@]}" --release --bin hatchling-room --target-dir "$output/target"
install -m 0755 "$output/target/$target/release/hatchling-room" "$output/hatchling-room"
sha256sum "$output/hatchling-room"
