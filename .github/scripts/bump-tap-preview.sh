#!/usr/bin/env bash
# Publish-time Homebrew tap bump for the preview channel.
#
# Renders Formula/llmux-preview.rb + Casks/llmux-islands-preview.rb from the
# assets the publish job just uploaded (the same sha256s brew will download)
# and pushes them to the tap, so `brew upgrade llmux-preview` /
# `llmux-islands-preview` see this build immediately instead of waiting on the
# tap's 6h cron (which stays as a backstop and no-ops on "already at TAG").
#
# Fail-closed: a preview publish is not finished until the tap points at this
# build. Every abort except "the tap already carries this or a newer build"
# exits non-zero so the publish job goes red.
#
# Auth: GH_TOKEN (repo secret TAP_DISPATCH_TOKEN) through `gh auth setup-git`,
# i.e. an ephemeral git credential helper on the runner. The token never enters
# a remote URL, the clone's config, or the log.
#
# Env contract:
#   TAG          required   exactly preview-$BUILD_ID
#   BUILD_ID     required   YYYY-MM-DD-HHMM-<sha12>
#   GH_TOKEN     required   token with push access to the tap
#   RELEASE_DIR  optional   dir holding the published assets (default: release)
#   TAP_REPO     optional   default 2lab-ai/homebrew-tap
#   TAP_REMOTE   optional   default https://github.com/$TAP_REPO.git
#   TAP_BRANCH   optional   default master
#   TAP_DIR      optional   clone destination, must not exist (default: tap-bump)
set -euo pipefail
export LC_ALL=C

fail() {
  echo "error: $*" >&2
  exit 1
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# Renders both files from the tap's templates. A surviving @TOKEN@ means the
# template/render contract drifted, which must not reach brew users.
render() {
  sed -e "s/@VERSION@/$VERSION/g" \
    -e "s/@TAG@/$TAG/g" \
    -e "s/@SHA_MACOS_AARCH64@/$SHA_MACOS_AARCH64/" \
    -e "s/@SHA_MACOS_X86_64@/$SHA_MACOS_X86_64/" \
    -e "s/@SHA_LINUX_AARCH64@/$SHA_LINUX_AARCH64/" \
    -e "s/@SHA_LINUX_X86_64@/$SHA_LINUX_X86_64/" \
    "$TAP_DIR/Formula/llmux-preview.rb.tmpl" > "$TAP_DIR/Formula/llmux-preview.rb"
  sed -e "s/@VERSION@/$VERSION/g" \
    -e "s/@TAG@/$TAG/g" \
    -e "s/@SHA@/$ISLANDS_SHA/" \
    "$TAP_DIR/Casks/llmux-islands-preview.rb.tmpl" > "$TAP_DIR/Casks/llmux-islands-preview.rb"
  ruby -c "$TAP_DIR/Formula/llmux-preview.rb" > /dev/null ||
    fail "rendered formula is not valid ruby"
  ruby -c "$TAP_DIR/Casks/llmux-islands-preview.rb" > /dev/null ||
    fail "rendered cask is not valid ruby"
  if grep -q "@[A-Z_]\+@" "$TAP_DIR/Formula/llmux-preview.rb" \
    "$TAP_DIR/Casks/llmux-islands-preview.rb"; then
    fail "unrendered @TOKEN@ left in the tap files; template contract drifted"
  fi
}

main() {
  local TAG="${TAG:-}" BUILD_ID="${BUILD_ID:-}"
  local RELEASE_DIR="${RELEASE_DIR:-release}"
  local TAP_REPO="${TAP_REPO:-2lab-ai/homebrew-tap}"
  local TAP_REMOTE="${TAP_REMOTE:-https://github.com/${TAP_REPO}.git}"
  local TAP_BRANCH="${TAP_BRANCH:-master}"
  local TAP_DIR="${TAP_DIR:-tap-bump}"

  [ -n "$TAG" ] || fail "TAG is required"
  [ -n "$BUILD_ID" ] || fail "BUILD_ID is required"
  # No silent no-op on a missing credential: that is exactly how this step
  # spent releases pretending to succeed (TAP_PUSH_KEY was never registered).
  [ -n "${GH_TOKEN:-}" ] ||
    fail "GH_TOKEN is empty; register the tap push credential (secret TAP_DISPATCH_TOKEN)"
  command -v gh > /dev/null 2>&1 || fail "gh is not installed"

  # build_id = YYYY-MM-DD-HHMM-<sha12> -> brew version YYYY.MM.DD.HHMM.
  # Anchored at both ends: a partial match would render a version the tap's
  # freshness comparison (minute prefix) can no longer be trusted against.
  [[ "$BUILD_ID" =~ ^([0-9]{4})-([0-9]{2})-([0-9]{2})-([0-9]{4})-[0-9a-f]{12}$ ]] ||
    fail "unexpected build id: $BUILD_ID (want YYYY-MM-DD-HHMM-<sha12>)"
  local VERSION="${BASH_REMATCH[1]}.${BASH_REMATCH[2]}.${BASH_REMATCH[3]}.${BASH_REMATCH[4]}"
  # The formula's version comes from BUILD_ID and its download URLs from TAG.
  # If the two describe different builds, brew gets a formula whose version and
  # URLs disagree, so require the exact relation the preflight job produces.
  [ "$TAG" = "preview-$BUILD_ID" ] ||
    fail "TAG $TAG does not match BUILD_ID $BUILD_ID (want preview-$BUILD_ID)"

  # The clone destination is never deleted: TAP_DIR is caller-supplied, and a
  # stray `rm -rf` on it is a worse failure than refusing to start. Each
  # Actions run gets a fresh workspace, so an existing path means something
  # unexpected is there — including on a re-run, where the step re-clones into
  # a clean workspace.
  if [ -e "$TAP_DIR" ]; then
    fail "clone destination $TAP_DIR already exists; refusing to touch it"
  fi

  # Plain variables, not an associative array: this script must also run under
  # the bash 3.2 a macOS contributor gets from `/usr/bin/env bash`.
  local name
  for name in llmux-macos-aarch64 llmux-macos-x86_64 llmux-linux-aarch64 llmux-linux-x86_64; do
    [ -f "$RELEASE_DIR/$name" ] || fail "missing published asset $RELEASE_DIR/$name"
  done
  local SHA_MACOS_AARCH64 SHA_MACOS_X86_64 SHA_LINUX_AARCH64 SHA_LINUX_X86_64
  SHA_MACOS_AARCH64="$(sha256_of "$RELEASE_DIR/llmux-macos-aarch64")"
  SHA_MACOS_X86_64="$(sha256_of "$RELEASE_DIR/llmux-macos-x86_64")"
  SHA_LINUX_AARCH64="$(sha256_of "$RELEASE_DIR/llmux-linux-aarch64")"
  SHA_LINUX_X86_64="$(sha256_of "$RELEASE_DIR/llmux-linux-x86_64")"
  local ZIP="$RELEASE_DIR/LlmuxIslands-${VERSION}.zip"
  [ -f "$ZIP" ] || fail "missing published asset $ZIP"
  local ISLANDS_SHA
  ISLANDS_SHA="$(sha256_of "$ZIP")"

  # Ephemeral credential helper on this runner; the token stays in the env.
  gh auth setup-git --hostname github.com ||
    fail "gh auth setup-git failed; the tap credential is unusable"

  git clone --depth 1 --branch "$TAP_BRANCH" "$TAP_REMOTE" "$TAP_DIR" ||
    fail "cannot clone $TAP_REPO; check the tap credential's push access"
  git -C "$TAP_DIR" config user.name "github-actions[bot]"
  git -C "$TAP_DIR" config user.email "41898282+github-actions[bot]@users.noreply.github.com"

  local attempt CURRENT CUR_MIN TAG_MIN
  for attempt in 1 2 3; do
    git -C "$TAP_DIR" fetch origin "$TAP_BRANCH"
    git -C "$TAP_DIR" reset --hard "origin/$TAP_BRANCH"
    # Freshness guard: preview-YYYY-MM-DD-HHMM-<sha12> is chronological only
    # down to the minute — the sha12 tail carries no time order. Compare minute
    # prefixes.
    CURRENT="$(sed -n 's|^.*releases/download/\(preview-[^/]*\)/.*|\1|p' \
      "$TAP_DIR/Formula/llmux-preview.rb" 2> /dev/null | head -1 || true)"
    if [ -n "$CURRENT" ] && [ "$CURRENT" != "$TAG" ]; then
      CUR_MIN="${CURRENT%-*}"
      TAG_MIN="${TAG%-*}"
      if [[ "$CUR_MIN" > "$TAG_MIN" ]]; then
        echo "tap already at newer $CURRENT; skipping"
        exit 0
      fi
      if [ "$CUR_MIN" = "$TAG_MIN" ]; then
        # Two builds in the same minute: which one is newer is unknowable from
        # the tag alone. Overwriting could roll brew users back, so stop — and
        # stop loudly, because "the cron will sort it out" is the 6h fallback
        # this step exists to remove.
        fail "tap at $CURRENT from the same minute as $TAG; order unknowable — rerun this job once the ordering is clear"
      fi
    fi
    render
    git -C "$TAP_DIR" add Formula/llmux-preview.rb Casks/llmux-islands-preview.rb
    if git -C "$TAP_DIR" diff --cached --quiet; then
      echo "tap already current at $TAG"
      exit 0
    fi
    git -C "$TAP_DIR" commit -m "llmux-preview + llmux-islands-preview $TAG"
    if git -C "$TAP_DIR" push origin "HEAD:$TAP_BRANCH"; then
      echo "tap bumped to $TAG"
      exit 0
    fi
    echo "push lost a race (attempt $attempt); refetching" >&2
  done
  fail "tap push failed after 3 attempts; preview $TAG is published but brew users cannot see it"
}

main "$@"
