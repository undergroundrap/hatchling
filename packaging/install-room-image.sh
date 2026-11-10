# Sourced by install.sh for the payload of an independently verified
# Windows Setup. Setup's attestation covers its embedded SHA256SUMS.

room_directory() {
  if [ ! -e "$1" ]; then
    mkdir -m 755 -- "$1" || fail "cannot create $1"
  fi
  [ -n "$(find "$1" -maxdepth 0 -type d -uid 0 ! -perm /022 -print)" ] \
    || fail "Room needs a root-controlled directory at $1"
}

room_digest() {
  # Require a single text-mode entry for this exact name. Never follow
  # paths named by the manifest.
  LC_ALL=C awk -v name="$1" '
    substr($0, 67) == name {
      count++
      hash = substr($0, 1, 64)
      if (substr($0, 65, 2) != "  " || hash ~ /[^0-9a-f]/) bad = 1
    }
    END {
      if (count != 1 || bad) exit 1
      print hash
    }
  ' "$FROM/SHA256SUMS"
}

room_cleanup() {
  if [ -n "$room_stage" ]; then
    rm -f -- "$room_stage/$room_image" "$room_stage/$room_tree" "$room_stage/$room_root"
    rmdir -- "$room_stage"
  fi
  if [ -n "$room_record_stage" ]; then
    rm -f -- "$room_record_stage/room-image.admission"
    rmdir -- "$room_record_stage"
  fi
}

room_prepare() {
  [ "$TARGET" = x86_64-unknown-linux-gnu ] || fail "the bundled Room image requires x86_64"
  [ -x /usr/sbin/cryptsetup ] && [ -x /usr/sbin/veritysetup ] \
    || fail "prepare WSL with cryptsetup-bin before installing the bundled Room offline"
  [ -n "$(find /root -maxdepth 0 -type d -uid 0 ! -perm /077 -print)" ] \
    || fail "Room needs a root-private /root for Setup staging"
  case "$FROM" in
    /root/hatchling-release.????????) ;;
    *) fail "the Room bundle must come from Windows Setup's private staging directory" ;;
  esac
  [ -n "$(find "$FROM" -maxdepth 0 -type d -uid 0 -perm 0700 -print)" ] \
    || fail "the Windows Setup staging directory is not root-private"
  [ -f "$FROM/SHA256SUMS" ] && [ ! -L "$FROM/SHA256SUMS" ] \
    || fail "the Windows Setup payload has no checksum manifest"

  room_image=room-system-x86_64.squashfs
  room_tree=room-system-x86_64.verity
  room_root=room-system-x86_64.root-hash
  room_dir=/usr/local/share/hatchling/room
  for dir in / /etc /etc/hatchling /usr /usr/local /usr/local/share /usr/local/share/hatchling "$room_dir"; do
    room_directory "$dir"
  done
  # Keep the lock through publication and service restart. A failed
  # verification leaves the previous image and record in place.
  exec 9>/etc/hatchling/.room-image.lock
  flock -x 9 || fail "cannot lock the Room image installation"
  room_stage= room_record_stage=
  trap room_cleanup EXIT
  trap 'exit 130' INT TERM
  room_stage=$(mktemp -d "$room_dir/.install.XXXXXXXX")
  room_record_stage=$(mktemp -d /etc/hatchling/.room-image.XXXXXXXX)
  for file in "$room_image" "$room_tree" "$room_root"; do
    expected=$(room_digest "$file") || fail "$file needs one checksum in the Setup manifest"
    [ -f "$FROM/$file" ] && [ ! -L "$FROM/$file" ] || fail "Setup is missing a regular $file"
    install -m 0644 -- "$FROM/$file" "$room_stage/$file"
    actual=$(sha256sum < "$room_stage/$file")
    [ "${actual%% *}" = "$expected" ] || fail "$file differs from the Setup manifest"
    if [ "$file" = "$room_image" ]; then
      image_digest=$expected
    fi
  done
  expected_root=$(cat "$room_stage/$room_root")
  case "$expected_root" in *[!0-9a-f]*) fail "invalid Room verity root" ;; esac
  [ "${#expected_root}" = 64 ] || fail "invalid Room verity root"
  if ! printf '%s' "$expected_root" | cmp -s - "$room_stage/$room_root" \
    && ! printf '%s\n' "$expected_root" | cmp -s - "$room_stage/$room_root"; then
    fail "invalid Room verity root"
  fi
  /usr/sbin/veritysetup verify --no-superblock --format=1 --hash=sha256 \
    --data-block-size=4096 --hash-block-size=4096 --salt "$image_digest" \
    "$room_stage/$room_image" "$room_stage/$room_tree" "$expected_root" \
    || fail "the bundled Room image failed verity verification"
  (umask 077; printf '%s\n%s\n' "$expected_root" "$image_digest" > "$room_record_stage/room-image.admission")
}

room_publish() {
  # Replace fresh inodes, leaving any already pinned image alone. With
  # the old record removed first, an interrupted update stays closed.
  rm -f -- /etc/hatchling/room-image.admission
  mv -fT -- "$room_stage/$room_image" "$room_dir/$room_image"
  mv -fT -- "$room_stage/$room_tree" "$room_dir/$room_tree"
  mv -fT -- "$room_record_stage/room-image.admission" /etc/hatchling/room-image.admission
}
