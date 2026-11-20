# Sourced by install.sh to admit the agent's computer's image (the Room's),
# from one of two places, named to room_prepare:
#   windows  the payload of an independently verified Windows Setup, in its
#            root-private staging directory: Setup's attestation covers its
#            embedded SHA256SUMS;
#   release  a release's files, as get-hatchling.sh downloads them: the
#            release's SHA256SUMS, which install.sh has checked every file
#            against, is the anchor, as it is for the programs it installs.
#            The directory and each file must be root's or the owner's, and
#            nobody else's to write.
# Either way the manifest and each file are copied into a root-only stage
# first, and only those copies are read and verified: what is in $FROM
# can't change under the check.

room_directory() {
  if [ ! -e "$1" ]; then
    mkdir -m 755 -- "$1" || fail "cannot create $1"
  fi
  [ -n "$(find "$1" -maxdepth 0 -type d -uid 0 ! -perm /022 -print)" ] \
    || fail "Room needs a root-controlled directory at $1"
}

# Whether $1 is root's or the owner's, with no write for group or others.
room_owned() {
  [ ! -L "$1" ] \
    && [ -n "$(find "$1" -maxdepth 0 \( -uid 0 -o -uid "$OWNER" \) ! -perm /022 -print)" ]
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
  ' "$room_manifest"
}

room_cleanup() {
  if [ -n "$room_stage" ]; then
    rm -f -- "$room_stage/$room_image" "$room_stage/$room_tree" "$room_stage/$room_root" \
      "$room_stage/SHA256SUMS"
    rmdir -- "$room_stage"
  fi
  if [ -n "$room_record_stage" ]; then
    rm -f -- "$room_record_stage/room-image.admission"
    rmdir -- "$room_record_stage"
  fi
}

room_prepare() { # windows | release
  [ "$TARGET" = x86_64-unknown-linux-gnu ] || fail "the Room's image requires x86_64"
  case "$1" in
    windows)
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
      room_manifest_name="the Setup manifest"
      ;;
    release)
      [ -x /usr/sbin/veritysetup ] \
        || fail "the Room's image is checked with veritysetup: install cryptsetup-bin, then run this again"
      [ -d "$FROM" ] && room_owned "$FROM" \
        || fail "$FROM holds the Room's image, so it must be root's or $OWNER_NAME's, and nobody else's to write"
      [ -f "$FROM/SHA256SUMS" ] && room_owned "$FROM/SHA256SUMS" \
        || fail "the release's SHA256SUMS must be a file of root's or $OWNER_NAME's, and nobody else's to write"
      room_manifest_name="the release's SHA256SUMS"
      ;;
    *) fail "room_prepare: windows or release" ;;
  esac

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
  # The manifest is read once, into the stage, and only that copy after.
  install -m 0600 -- "$FROM/SHA256SUMS" "$room_stage/SHA256SUMS"
  room_manifest=$room_stage/SHA256SUMS
  for file in "$room_image" "$room_tree" "$room_root"; do
    expected=$(room_digest "$file") || fail "$file needs one checksum in $room_manifest_name"
    [ -f "$FROM/$file" ] && [ ! -L "$FROM/$file" ] || fail "$FROM is missing a regular $file"
    if [ "$1" = release ]; then
      room_owned "$FROM/$file" \
        || fail "$FROM/$file must be root's or $OWNER_NAME's, and nobody else's to write"
    fi
    install -m 0644 -- "$FROM/$file" "$room_stage/$file"
    actual=$(sha256sum < "$room_stage/$file")
    [ "${actual%% *}" = "$expected" ] || fail "$file differs from $room_manifest_name"
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
    || fail "the Room's image failed verity verification"
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
