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
# anything is installed. Without gh, or signed out, or offline, it says it
# couldn't check, and the install goes on as it would without this.
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

# Checks $FROM/SHA256SUMS's attestation, saying what it found; fails the
# install when gh can check and it doesn't verify.
release_provenance() {
  PROVENANCE_GH=$(command -v gh 2> /dev/null) || PROVENANCE_GH=
  if [ -z "$PROVENANCE_GH" ]; then
    say "no gh here, so who built these files isn't checked. To check: gh attestation verify SHA256SUMS --repo $PROVENANCE_REPO (docs/releasing.md#verifying-a-release)"
    return 0
  fi
  if ! owner_gh auth status > /dev/null 2>&1; then
    say "gh isn't signed in here as $OWNER_NAME (or GitHub can't be reached), so who built these files isn't checked. To check: gh auth login, then run this again"
    return 0
  fi
  if said=$(owner_gh attestation verify "$FROM/SHA256SUMS" --repo "$PROVENANCE_REPO" \
    --signer-workflow "$PROVENANCE_WORKFLOW" 2>&1); then
    say "SHA256SUMS is attested by this project's release workflow, so every file that matches it is too"
    return 0
  fi
  fail "SHA256SUMS isn't attested by this project's release workflow, so these files may not be the release's: nothing was installed. gh said: $(printf '%s' "$said" | tail -n 3 | tr '\n' ' ')"
}
