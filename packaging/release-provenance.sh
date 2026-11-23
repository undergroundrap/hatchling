# Sourced by install.sh: who built the release's files (#556).
#
# install.sh has checked every file it installs against the release's
# SHA256SUMS. The release workflow attests SHA256SUMS with the rest, so
# when SHA256SUMS is attested by this repository's release workflow, so is
# every file that matches it: one check covers them all.
#
# It runs gh as the owner (the user who ran sudo): root has no sign-in of
# its own, and the owner's is theirs. With gh there and signed in, a
# SHA256SUMS the release workflow didn't attest stops the install before
# anything is installed. Without gh, or signed out, or offline, it can't
# check, and that stops the install too (#627), unless the owner asked for
# it with --unverified (PROVENANCE_UNVERIFIED): then it says plainly what
# wasn't checked, and goes on. A release gh checks and finds isn't this
# project's is refused whatever is asked.
#
# refuse_older stops an install of a release older than the one installed,
# unless the owner asked with --allow-older (PROVENANCE_ALLOW_OLDER): a
# genuine old release can carry a fixed vulnerability (#627).
#
# What it can't do: protect the first `curl` + `sh` install by itself
# (whoever can replace the release can replace get-hatchling.sh too: the
# install guide's verify-first step is that check), or tell a release
# built by someone who can run the release workflow from a real one.

PROVENANCE_REPO=undergroundrap/hatchling
PROVENANCE_WORKFLOW=$PROVENANCE_REPO/.github/workflows/release.yml

# gh, run as the owner, in their home: `owner_gh <args>`.
owner_gh() {
  runuser -u "$OWNER_NAME" -- env HOME="$OWNER_HOME" PATH=/usr/local/bin:/usr/bin:/bin "$PROVENANCE_GH" "$@"
}

# Who built these files can't be checked, for the reason $1: goes on only
# when the owner asked with --unverified, saying so; else stops, saying how
# to check ($2).
unchecked() {
  if [ -n "${PROVENANCE_UNVERIFIED:-}" ]; then
    say "$1, so who built these files isn't checked: installing them anyway, as --unverified asks. To check them: gh attestation verify SHA256SUMS --repo $PROVENANCE_REPO (docs/releasing.md#verifying-a-release)"
    return 0
  fi
  fail "$1, so who built these files can't be checked, and nothing was installed. To check: $2. To install them unchecked: pass --unverified"
}

# The version the broker $1 says it is, run from a copy in $2, a directory
# that lets programs run (where the programs go): the directory it came
# from, or /tmp, may not. Nothing when there's no $1 or it doesn't say.
broker_version() {
  [ -f "$1" ] || return 0
  mkdir -p "$2"
  copy=$(mktemp "$2/.hatchling-version.XXXXXX") || return 0
  cp "$1" "$copy" && chmod 700 "$copy" \
    && "$copy" --version 2> /dev/null | sed -n '1s/^hatchling-broker \([^ ]*\).*/\1/p'
  rm -f "$copy"
}

# Stops an install of Hatchling $2 over $1, installed, when $2 is older,
# unless the owner asked with --allow-older. A version with a suffix
# (0.10.0-rc.1) compares by its numbers: the same numbers aren't older.
# Nothing installed ($1 empty) is never older.
refuse_older() {
  [ -n "$1" ] || return 0
  if [ -z "$2" ]; then
    [ -n "${PROVENANCE_ALLOW_OLDER:-}" ] && return 0
    fail "the files' version can't be read, so it isn't compared with $1, installed: nothing was installed. To install them anyway: pass --allow-older"
  fi
  [ "${1%%-*}" = "${2%%-*}" ] && return 0
  lowest=$(printf '%s\n%s\n' "${1%%-*}" "${2%%-*}" | sort -V | head -n 1)
  [ "$lowest" = "${1%%-*}" ] && return 0
  if [ -n "${PROVENANCE_ALLOW_OLDER:-}" ]; then
    say "installing Hatchling $2 over $1, which is newer, as --allow-older asks"
    return 0
  fi
  fail "these files are Hatchling $2, older than $1, installed, and an older release can carry a vulnerability fixed since: nothing was installed. To install it anyway: pass --allow-older"
}

# Checks $FROM/SHA256SUMS's attestation, saying what it found; fails the
# install when gh can check and it doesn't verify.
release_provenance() {
  PROVENANCE_GH=$(command -v gh 2> /dev/null) || PROVENANCE_GH=
  if [ -z "$PROVENANCE_GH" ]; then
    unchecked "no gh here" "install gh and sign in (gh auth login), then run this again"
    return 0
  fi
  if ! owner_gh auth status > /dev/null 2>&1; then
    unchecked "gh isn't signed in here as $OWNER_NAME (or GitHub can't be reached)" "gh auth login, then run this again"
    return 0
  fi
  if said=$(owner_gh attestation verify "$FROM/SHA256SUMS" --repo "$PROVENANCE_REPO" \
    --signer-workflow "$PROVENANCE_WORKFLOW" 2>&1); then
    say "SHA256SUMS is attested by this project's release workflow, so every file that matches it is too"
    return 0
  fi
  fail "SHA256SUMS isn't attested by this project's release workflow, so these files may not be the release's: nothing was installed. gh said: $(printf '%s' "$said" | tail -n 3 | tr '\n' ' ')"
}
