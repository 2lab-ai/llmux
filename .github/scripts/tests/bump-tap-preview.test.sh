#!/usr/bin/env bash
# Regression test for .github/scripts/bump-tap-preview.sh.
#
# Hermetic: `gh` is mocked (only `auth setup-git` is expected) and the tap is a
# local bare repo, so the real clone/render/commit/push path runs with real git
# and no network or credential.
#
# The bug this pins: the publish step used to skip on a missing credential and
# `exit 0` on a failed push, so a preview could publish while brew users kept
# the old build. Every failure mode below must exit non-zero.
set -euo pipefail

SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/bump-tap-preview.sh"
[ -f "$SCRIPT" ] || {
  echo "missing script under test: $SCRIPT" >&2
  exit 1
}
command -v ruby > /dev/null 2>&1 || {
  echo "ruby is required (the script syntax-checks the rendered files)" >&2
  exit 1
}

ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT

# Sentinel credential: must never surface in output or in the clone.
TOKEN="gho_sentinel_must_not_appear_000000000000"
TAG="preview-2026-09-11-0404-abc123def456"
VERSION="2026.09.11.0404"

FAILURES=0
ok() { printf 'ok   - %s\n' "$1"; }
notok() {
  printf 'FAIL - %s\n' "$1"
  FAILURES=$((FAILURES + 1))
}
expect_zero() {
  if [ "$RC" -eq 0 ]; then ok "$1"; else
    notok "$1 (rc=$RC)"
    printf '%s\n' "$OUT" | sed 's/^/       | /'
  fi
}
expect_nonzero() {
  if [ "$RC" -ne 0 ]; then ok "$1"; else
    notok "$1 (rc=0, expected failure)"
    printf '%s\n' "$OUT" | sed 's/^/       | /'
  fi
}
expect_out() {
  case "$OUT" in
    *"$1"*) ok "$2" ;;
    *)
      notok "$2 (output lacks '$1')"
      printf '%s\n' "$OUT" | sed 's/^/       | /'
      ;;
  esac
}
expect_true() { if "${@:2}"; then ok "$1"; else notok "$1"; fi; }
# passes when the command fails, i.e. when the pattern is absent
expect_absent() { if "${@:2}"; then notok "$1"; else ok "$1"; fi; }

# --- hermetic environment -----------------------------------------------
mkdir -p "$ROOT/bin"
cat > "$ROOT/bin/gh" << 'MOCK'
#!/usr/bin/env bash
# The local tap remote needs no credential helper, so setup-git is a no-op.
if [ "${1:-}" = "auth" ] && [ "${2:-}" = "setup-git" ]; then exit 0; fi
echo "unexpected gh invocation: $*" >&2
exit 1
MOCK
chmod +x "$ROOT/bin/gh"
export PATH="$ROOT/bin:$PATH"
# Never read or write the caller's ~/.gitconfig.
export GIT_CONFIG_GLOBAL="$ROOT/gitconfig"
export GIT_CONFIG_NOSYSTEM=1
git config --global user.name "test"
git config --global user.email "test@example.com"
git config --global init.defaultBranch master

FORMULA_TMPL="$ROOT/Formula.tmpl"
CASK_TMPL="$ROOT/Cask.tmpl"
cat > "$FORMULA_TMPL" << 'TMPL'
class LlmuxPreview < Formula
  desc "test fixture mirroring the tap template"
  homepage "https://github.com/2lab-ai/llmux"
  version "@VERSION@"

  on_macos do
    on_arm do
      url "https://github.com/2lab-ai/llmux/releases/download/@TAG@/llmux-macos-aarch64"
      sha256 "@SHA_MACOS_AARCH64@"
    end
    on_intel do
      url "https://github.com/2lab-ai/llmux/releases/download/@TAG@/llmux-macos-x86_64"
      sha256 "@SHA_MACOS_X86_64@"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/2lab-ai/llmux/releases/download/@TAG@/llmux-linux-aarch64"
      sha256 "@SHA_LINUX_AARCH64@"
    end
    on_intel do
      url "https://github.com/2lab-ai/llmux/releases/download/@TAG@/llmux-linux-x86_64"
      sha256 "@SHA_LINUX_X86_64@"
    end
  end
end
TMPL
cat > "$CASK_TMPL" << 'TMPL'
cask "llmux-islands-preview" do
  version "@VERSION@"
  sha256 "@SHA@"
  url "https://github.com/2lab-ai/llmux/releases/download/@TAG@/LlmuxIslands-#{version}.zip"
end
TMPL

sha256_of() {
  if command -v sha256sum > /dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# seed_tap <bare-repo> [current-tag] — a tap whose rendered formula already
# points at <current-tag> (omit for a tap with templates only).
seed_tap() {
  local origin="$1" current="${2:-}"
  local work="${origin%.git}-seed"
  git init --quiet --bare "$origin"
  git -C "$origin" symbolic-ref HEAD refs/heads/master
  git init --quiet "$work"
  mkdir -p "$work/Formula" "$work/Casks"
  cp "$FORMULA_TMPL" "$work/Formula/llmux-preview.rb.tmpl"
  cp "$CASK_TMPL" "$work/Casks/llmux-islands-preview.rb.tmpl"
  if [ -n "$current" ]; then
    local ver
    ver="$(echo "${current#preview-}" | awk -F- '{print $1"."$2"."$3"."$4}')"
    sed -e "s/@VERSION@/$ver/g" -e "s/@TAG@/$current/g" -e "s/@SHA[A-Z_0-9]*@/00seed/g" \
      "$FORMULA_TMPL" > "$work/Formula/llmux-preview.rb"
    sed -e "s/@VERSION@/$ver/g" -e "s/@TAG@/$current/g" -e "s/@SHA@/00seed/g" \
      "$CASK_TMPL" > "$work/Casks/llmux-islands-preview.rb"
  fi
  git -C "$work" add -A
  git -C "$work" commit --quiet -m "seed"
  git -C "$work" push --quiet "$origin" HEAD:master
}

# make_release <dir> — the four binaries + the islands zip this build published.
make_release() {
  local dir="$1"
  mkdir -p "$dir"
  local name
  for name in llmux-macos-aarch64 llmux-macos-x86_64 llmux-linux-aarch64 llmux-linux-x86_64; do
    echo "$name binary for $TAG" > "$dir/$name"
  done
  echo "islands zip for $TAG" > "$dir/LlmuxIslands-${VERSION}.zip"
}

# new_case <name> [current-tag] — sets CASE_DIR/ORIGIN/WORK.
new_case() {
  CASE_DIR="$ROOT/case-$1"
  ORIGIN="$CASE_DIR/tap.git"
  WORK="$CASE_DIR/work"
  mkdir -p "$WORK"
  seed_tap "$ORIGIN" "${2:-}"
  make_release "$WORK/release"
}

# run_bump [tag] [token] [build_id] — runs the script in WORK against ORIGIN;
# sets OUT/RC/TAP_CLONE. Each run clones into its own destination, the way a
# fresh runner workspace hands the step an empty path (TAP_DIR_FORCE overrides).
RUN_N=0
run_bump() {
  local tag="${1:-$TAG}" token="${2-$TOKEN}"
  local build="${3-}"
  [ -n "$build" ] || build="${tag#preview-}"
  RUN_N=$((RUN_N + 1))
  TAP_CLONE="${TAP_DIR_FORCE:-tap-bump-$RUN_N}"
  set +e
  OUT="$(cd "$WORK" && TAG="$tag" BUILD_ID="$build" GH_TOKEN="$token" \
    RELEASE_DIR=release TAP_REMOTE="$ORIGIN" TAP_BRANCH=master TAP_DIR="$TAP_CLONE" \
    bash "$SCRIPT" 2>&1)"
  RC=$?
  set -e
}

pushed_formula() { git -C "$ORIGIN" show "master:Formula/llmux-preview.rb"; }
pushed_cask() { git -C "$ORIGIN" show "master:Casks/llmux-islands-preview.rb"; }
commit_count() { git -C "$ORIGIN" rev-list --count master; }

# --- case: no credential registered (the shipped bug) --------------------
new_case no-token
before="$(commit_count)"
run_bump "$TAG" ""
expect_nonzero "missing GH_TOKEN fails the step instead of skipping"
expect_out "TAP_DISPATCH_TOKEN" "missing-token error names the credential to register"
expect_true "missing GH_TOKEN pushes nothing" [ "$before" = "$(commit_count)" ]

# --- case: missing artifacts --------------------------------------------
new_case no-zip
rm -f "$WORK/release/LlmuxIslands-${VERSION}.zip"
run_bump
expect_nonzero "missing islands zip fails the step"
expect_out "LlmuxIslands-${VERSION}.zip" "missing-zip error names the asset"

new_case no-binary
rm -f "$WORK/release/llmux-linux-aarch64"
run_bump
expect_nonzero "missing binary asset fails the step"

# --- case: input validation ----------------------------------------------
new_case bad-build-id
run_bump "preview-2026-09-11-0404-nothex123456"
expect_nonzero "a build id whose sha12 is not hex fails the step"
expect_out "unexpected build id" "malformed build id is named in the error"

new_case tag-build-mismatch
run_bump "$TAG" "$TOKEN" "2026-09-11-0500-abc123def456"
expect_nonzero "a TAG that disagrees with BUILD_ID fails the step"
expect_out "does not match" "tag/build mismatch is named in the error"

# --- case: the clone destination is never clobbered -----------------------
# The script must refuse an occupied destination rather than delete a path it
# was handed (TAP_DIR is caller-supplied).
new_case occupied-dest "preview-2026-09-10-1200-000000000000"
mkdir -p "$WORK/occupied/nested"
echo "do-not-delete" > "$WORK/occupied/nested/sentinel"
before="$(commit_count)"
TAP_DIR_FORCE=occupied run_bump
unset TAP_DIR_FORCE
expect_nonzero "an existing clone destination fails the step"
expect_out "already exists" "occupied-destination error says what it refused"
expect_true "the occupied destination is left untouched" \
  [ "$(cat "$WORK/occupied/nested/sentinel" 2> /dev/null)" = "do-not-delete" ]
expect_true "the occupied-destination case pushes nothing" \
  [ "$before" = "$(commit_count)" ]

# --- case: happy path ----------------------------------------------------
new_case happy "preview-2026-09-10-1200-000000000000"
run_bump
expect_zero "happy path succeeds"
expect_out "tap bumped to $TAG" "happy path reports the push"
formula="$(pushed_formula)"
cask="$(pushed_cask)"
expect_true "pushed formula carries the brew version" \
  grep -q "version \"$VERSION\"" <<< "$formula"
expect_true "pushed formula points at this tag" grep -q "download/$TAG/" <<< "$formula"
expect_true "pushed formula carries the published macos-aarch64 sha256" \
  grep -q "$(sha256_of "$WORK/release/llmux-macos-aarch64")" <<< "$formula"
expect_true "pushed formula carries the published linux-x86_64 sha256" \
  grep -q "$(sha256_of "$WORK/release/llmux-linux-x86_64")" <<< "$formula"
expect_true "pushed cask carries the published zip sha256" \
  grep -q "$(sha256_of "$WORK/release/LlmuxIslands-${VERSION}.zip")" <<< "$cask"
expect_absent "no unrendered template token reaches the tap" \
  grep -q "@[A-Z_]\+@" <<< "$formula$cask"
expect_absent "the token never reaches the log" grep -qF "$TOKEN" <<< "$OUT"
expect_absent "the token never reaches the clone" \
  grep -rqF "$TOKEN" "$WORK/$TAP_CLONE/.git"

# rerunning the same build is a no-op, not a second commit
after_first="$(commit_count)"
run_bump
expect_zero "rerunning the same build succeeds"
expect_out "already current" "rerun detects the tap is already at this build"
expect_true "rerun adds no commit" [ "$after_first" = "$(commit_count)" ]

# --- case: freshness guards ---------------------------------------------
new_case newer-tap "preview-2026-09-12-0100-ffffffffffff"
before="$(commit_count)"
run_bump
expect_zero "a newer tap is left alone"
expect_out "already at newer" "newer-tap skip is reported"
expect_true "newer tap is not rolled back" [ "$before" = "$(commit_count)" ]

new_case same-minute "preview-2026-09-11-0404-ffffffffffff"
before="$(commit_count)"
run_bump
expect_nonzero "same-minute ambiguity fails instead of guessing"
expect_out "same minute" "same-minute error explains the ambiguity"
expect_true "same-minute case pushes nothing" [ "$before" = "$(commit_count)" ]

# --- case: push failures -------------------------------------------------
new_case push-rejected "preview-2026-09-10-1200-000000000000"
printf '#!/bin/sh\necho "simulated rejection" >&2\nexit 1\n' > "$ORIGIN/hooks/pre-receive"
chmod +x "$ORIGIN/hooks/pre-receive"
before="$(commit_count)"
run_bump
expect_nonzero "a rejected push fails the step"
expect_out "after 3 attempts" "push failure reports the retry exhaustion"
expect_true "rejected push leaves the tap untouched" [ "$before" = "$(commit_count)" ]

new_case push-race "preview-2026-09-10-1200-000000000000"
printf '#!/bin/sh\nif [ -f ./reject-once ]; then rm -f ./reject-once; echo "simulated race" >&2; exit 1; fi\nexit 0\n' \
  > "$ORIGIN/hooks/pre-receive"
chmod +x "$ORIGIN/hooks/pre-receive"
: > "$ORIGIN/reject-once"
run_bump
expect_zero "a lost push race is retried to success"
expect_out "tap bumped to $TAG" "retry reaches the push"

# --- case: template drift ------------------------------------------------
new_case drift
seed_work="$CASE_DIR/tap-seed"
printf '  sha256 "@SHA_UNEXPECTED@"\n' >> "$seed_work/Formula/llmux-preview.rb.tmpl"
git -C "$seed_work" commit --quiet -am "drifted template"
git -C "$seed_work" push --quiet "$ORIGIN" HEAD:master
run_bump
expect_nonzero "an unrendered template token fails the step"

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "all tap-bump regression cases passed"
else
  echo "$FAILURES tap-bump regression case(s) failed"
  exit 1
fi
