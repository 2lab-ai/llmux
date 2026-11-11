#!/usr/bin/env bash
# Generic forbidden-pattern scanner over git history + refs.
# Patterns are injected through two variables (extended regex, matched case-insensitively):
#   $SANITIZE_PATTERNS_SHARED — required. The list every 2lab-ai public repository
#     scans for (private host names, other companies' names and domains). Each
#     repository holds it as a secret of the same name with the same value.
#   $SANITIZE_PATTERNS — optional. This repository's own additions.
# The scan matches either list. Keep both plain POSIX ERE: git's regex engine
# locates hits, and it does not know grep-only extensions such as \d, \< or (?i).
# This file intentionally contains no pattern literals — see repo docs on sanitize policy.
#
# One script, two repositories: 2lab-ai/soma-work owns it (its behaviour tests
# live in scripts/__tests__/sanitize-scan.test.ts there) and 2lab-ai/llmux carries
# a byte-identical copy. Change both in the same way, or the gates drift apart.
#
# Scope depends on the event that runs it ($GITHUB_EVENT_NAME):
#
#   pull_request — the commit under test (GitHub's merge of the PR into its base)
#     and everything it reaches, plus the PR's branch name. A PR is judged on what
#     it would merge. It used to be judged on every ref in the runner's clone: on
#     2026-10-27 one dirty branch turned every PR and main's push run red for six
#     hours, and the red landed on whoever ran next.
#
#   anything else (push to main, a local run) — HEAD plus every published branch
#     and tag (refs/remotes/origin/*, refs/tags/*). Not `--all`: the runner's clone
#     is reused and keeps refs/remotes/pull/N/merge from earlier PR runs, so a
#     force-pushed-away commit would stay reachable there and red-light every
#     later run. Unreachable objects are out of scope for the same reason.
#
# A hit names the commits that carry it and the published branches that reach
# them. It never prints the matching text: the repository and its logs are public.
#
# Known leaks (scripts/sanitize-known-leaks.txt, #610): a commit that is already
# published cannot be taken back by failing every later PR. Listed by full id —
# and only if it is already published, so a change cannot list itself: on the
# PR's base branch for a PR; for a push, on main as it was before the push or on
# any other published branch or tag (a commit on deploy/dev or a release tag is
# out, and no push to main can take it back). A listed commit's own commit object
# is not scanned, and the matching text in the added lines of its diff is
# tolerated in file contents wherever it persists. Nothing else is: a different hit, another
# commit adding the same text again (a binary file and a merge's own resolution
# included) or carrying it anywhere in its commit object, any path name, a branch
# or tag name, an annotated tag. The list carries commit ids only, never the text.
set -euo pipefail
if [ -z "${SANITIZE_PATTERNS_SHARED:-}" ]; then echo "SANITIZE_PATTERNS_SHARED env var required" >&2; exit 2; fi
P="${SANITIZE_PATTERNS_SHARED}${SANITIZE_PATTERNS:+|$SANITIZE_PATTERNS}"
# An empty alternative matches every line, so one stray `|` in either list would
# turn every object into a hit. Refuse it rather than report a false red.
case "|$P|" in
  *'||'*) echo "sanitize-scan: a pattern list has an empty alternative (a leading, trailing or doubled |)" >&2; exit 2 ;;
esac

if [ "${GITHUB_EVENT_NAME:-}" = "pull_request" ]; then
  pr=1
  revs=(HEAD)
else
  pr=0
  revs=(HEAD --remotes=origin --tags)
fi

git rev-parse --verify -q HEAD >/dev/null || { echo "sanitize-scan: no HEAD to scan" >&2; exit 2; }

# The added lines of a commit's diff — binary files as text; for a merge, the
# lines no parent had (its own resolution). Hunk lines always start with +, - or
# a space, so `diff ` opens a file header and `@@` closes it: header lines (paths
# included) are never content. A merge's combined diff has one column per parent
# (`@@@` for two): a line is the merge's own only when every column is `+` —
# `+ ` or ` +` is a line one parent already had. Never printed.
diff_added() {
  local patch
  # stderr dropped: git's warnings can quote file content (an invalid attribute
  # name in a .gitattributes, say), and the log is public.
  patch=$(git show -U0 --text --format= --no-color --no-ext-diff --no-textconv "$1" 2>/dev/null) || return 1
  printf '%s\n' "$patch" | awk '
    BEGIN { n = 1 }
    /^diff / { h = 1; next }
    /^@@/ { h = 0; n = index($0, " ") - 2; next }
    !h && match($0, /^[+]+/) && RLENGTH >= n { print substr($0, n + 1) }'
}

# What a commit adds: its whole commit object (headers and message) and its
# diff's added lines.
added_text() {
  local body
  body=$(git cat-file commit "$1" 2>/dev/null) || return 1
  printf '%s\n' "$body"
  diff_added "$1"
}

# The commit a listed leak must already be in: the PR's base branch, main as it
# was before this push (the event file's `before`), or HEAD for a local run.
# None (no base to check against) exempts nothing.
published_base() {
  local before
  if [ "$pr" = 1 ]; then
    [ -n "${GITHUB_BASE_REF:-}" ] || return 1
    git rev-parse --verify -q "refs/remotes/origin/${GITHUB_BASE_REF}^{commit}"
  elif [ "${GITHUB_EVENT_NAME:-}" = push ]; then
    [ -n "${GITHUB_EVENT_PATH:-}" ] || return 1
    before=$(awk 'match($0, /"before": *"[0-9a-f]+"/) { s = substr($0, RSTART, RLENGTH); sub(/.*: *"/, "", s); sub(/"$/, "", s); print s; exit }' "$GITHUB_EVENT_PATH") || return 1
    [[ "$before" =~ ^[0-9a-f]{40}$ ]] || return 1
    git rev-parse --verify -q "${before}^{commit}"
  else
    git rev-parse --verify -q 'HEAD^{commit}'
  fi
}

# On a push, the branch the event names (`ref` in the event file). Every other
# branch and tag in the clone was published before this push. A run that cannot
# name the pushed branch counts no other ref as published.
pushed_branch() {
  local ref
  [ "${GITHUB_EVENT_NAME:-}" = push ] && [ -n "${GITHUB_EVENT_PATH:-}" ] || return 1
  ref=$(awk 'match($0, /"ref": *"refs\/heads\/[^"]+"/) { s = substr($0, RSTART, RLENGTH); sub(/.*: *"refs\/heads\//, "", s); sub(/"$/, "", s); print s; exit }' "$GITHUB_EVENT_PATH") || return 1
  [ -n "$ref" ] || return 1
  printf '%s\n' "$ref"
}

# Whether a published branch or tag other than the pushed branch contains the commit.
# Output is captured, not piped into `grep -q`: an early exit would fail the
# pipeline under pipefail and read as "not published".
on_other_published_ref() { # <sha> <pushed branch>
  local refs
  refs=$(git for-each-ref --contains "$1" --format='%(refname)' refs/remotes/origin refs/tags 2>/dev/null) || return 1
  refs=$(printf '%s\n' "$refs" | grep -v -x -F -e "refs/remotes/origin/$2" -e refs/remotes/origin/HEAD) || return 1
  [ -n "$refs" ]
}

# KNOWN: the matches in listed commits' added lines, one per line. An unreadable
# listed commit is an error, never an empty exemption.
KNOWN=$(mktemp "${TMPDIR:-/tmp}/sanitize-known.XXXXXX") || exit 2
OBJS=$(mktemp "${TMPDIR:-/tmp}/sanitize-objects.XXXXXX") || exit 2
trap 'rm -f "$KNOWN" "$OBJS"' EXIT
exempt=()
list="$(git rev-parse --show-toplevel)/scripts/sanitize-known-leaks.txt"
if [ -f "$list" ]; then
  base=$(published_base) || base=''
  pushed=$(pushed_branch) || pushed=''
  while read -r sha _ || [ -n "${sha:-}" ]; do
    case "$sha" in '' | '#'*) continue ;; esac
    if ! [[ "$sha" =~ ^[0-9a-f]{40}$ ]]; then
      echo "sanitize-scan: $list: entries are full 40-hex commit ids — not a clean result" >&2
      exit 2
    fi
    if [ "$(git cat-file -t "$sha" 2>/dev/null)" != commit ]; then
      echo "sanitize-scan: known leak $sha is not a commit in this clone — nothing exempted for it" >&2
      continue
    fi
    if { [ -z "$base" ] || ! git merge-base --is-ancestor "$sha" "$base" 2>/dev/null; } &&
      { [ -z "$pushed" ] || ! on_other_published_ref "$sha" "$pushed"; }; then
      echo "sanitize-scan: known leak $sha is not on the published base — nothing exempted for it" >&2
      continue
    fi
    text=$(diff_added "$sha") && { printf '%s\n' "$text" | grep -a -i -o -E "$P" >>"$KNOWN" || [ "$?" = 1 ]; } || {
      echo "sanitize-scan: could not read known leak $sha — not a clean result" >&2
      exit 2
    }
    exempt+=("$sha")
    echo "sanitize-scan: known leak $sha — exempted (scripts/sanitize-known-leaks.txt)" >&2
  done <"$list"
fi
is_exempt() {
  local e
  for e in ${exempt[@]+"${exempt[@]}"}; do [ "$e" = "$1" ] && return 0; done
  return 1
}

# Every object to scan, as "<id> <type>": commits, trees, blobs and, outside a
# PR, annotated tags (nested ones included). Every git step must succeed: a
# failed listing is an error, never an empty — and therefore clean — result.
typed_objects() {
  local list
  list=$(git rev-list --objects "${revs[@]}") || return 1
  printf '%s\n' "$list" | cut -d' ' -f1 | sort -u | git cat-file --batch-check='%(objectname) %(objecttype)' 2>/dev/null
}
typed_objects >"$OBJS" && ! awk '$2 !~ /^(commit|tree|blob|tag)$/ { bad = 1 } END { exit !bad }' "$OBJS" || {
  echo "sanitize-scan: could not list the objects to scan — not a clean result" >&2
  exit 2
}

# File contents: the only place a listed commit's additions are tolerated.
blob_contents() {
  awk '$2 == "blob" { print $1 }' "$OBJS" | git cat-file --batch
}

# Never exempt: commit objects (a listed commit's own aside) and trees.
other_contents() {
  awk -v ex="${exempt[*]-}" '
    BEGIN { n = split(ex, e, " "); for (i = 1; i <= n; i++) x[e[i]] = 1 }
    ($2 == "commit" || $2 == "tree") && !($1 in x) { print $1 }' "$OBJS" | git cat-file --batch
}

# Annotated tags (their messages), and the blobs and trees a tag points at
# directly — not through a commit, so no commit adds them: never exempt.
tag_contents() {
  local refs direct
  awk '$2 == "tag" { print $1 }' "$OBJS" | git cat-file --batch || return 1
  [ "$pr" = 0 ] || return 0
  refs=$(git for-each-ref --format='%(objectname)^{}' refs/tags) || return 1
  [ -n "$refs" ] || return 0
  direct=$(printf '%s\n' "$refs" | git cat-file --batch-check='%(objectname) %(objecttype)' 2>/dev/null |
    awk '$2 == "blob" || $2 == "tree" { print $1 }') || return 1
  [ -n "$direct" ] || return 0
  # shellcheck disable=SC2086 # object ids, one word each
  git rev-list --objects $direct | cut -d' ' -f1 | git cat-file --batch
}

ref_names() {
  if [ "$pr" = 1 ]; then
    printf '%s\n' "${GITHUB_HEAD_REF:-}"
  else
    git for-each-ref --format='%(refname)' refs/remotes/origin refs/tags
  fi
}

# Matches in a command's output, as "unknown known": a match equal to text a
# listed commit added is known (`known` is the file of that text — /dev/null for
# what is never exempt). Fails closed: only grep's "no match" (status 1) counts
# as zero; any other failure stops the scan with status 2.
count() {
  local r known=$1
  shift
  r=$("$@" | grep -a -i -o -E "$P" | awk -v kf="$known" '
    BEGIN { while ((getline l < kf) > 0) k[l] = 1 }
    { if ($0 in k) n++; else u++ }
    END { print u + 0, n + 0 }'; st=("${PIPESTATUS[@]}"); echo "${st[0]} ${st[1]} ${st[2]}") || true
  set -- $r
  if [ "$#" != 5 ] || [ "$3" != 0 ] || [ "$4" -gt 1 ] || [ "$5" != 0 ]; then
    echo "sanitize-scan: scan step failed (${*:3}) — not a clean result" >&2
    exit 2
  fi
  echo "$1 $2"
}

# A failing count exits its own subshell with 2: check each before reading it.
r=$(count "$KNOWN" blob_contents) || exit 2
read -r a ka <<<"$r"
r=$(count /dev/null other_contents) || exit 2
read -r o _ <<<"$r"
a=$((a + o))
r=$(count /dev/null git rev-list --objects "${revs[@]}") || exit 2
read -r b _ <<<"$r"
r=$(count /dev/null tag_contents) || exit 2
read -r t _ <<<"$r"
r=$(count /dev/null ref_names) || exit 2
read -r c _ <<<"$r"
known=$ka
echo "sanitize-scan: objects=$a paths=$b tags=$t refs=$c known=$known"
if [ "$a$b$t$c$known" = "00000" ]; then
  echo "sanitize-scan: clean"
  exit 0
fi

# Where. Commits whose diff adds or removes a matching line, or whose message or
# author/committer matches; then which published branches reach each one. Branch names that are
# themselves hits are redacted.
redact() {
  while read -r ref; do
    if printf '%s' "$ref" | grep -q -i -E "$P"; then echo '<redacted ref>'; else echo "$ref"; fi
  done
}
# A commit is a hit when what it adds matches. A listed commit is a known leak;
# a commit that only removes matching lines is the cleanup, and a clean merge
# adds nothing of its own. Unreadable means hit.
adds_hit() {
  local text n
  text=$(added_text "$1") || return 0
  n=$(printf '%s\n' "$text" | grep -a -i -c -E "$P") || true
  [ "${n:-0}" != 0 ]
}
attributed=1
shas=$({
  git log "${revs[@]}" -m --text -i -G"$P" --format=%H 2>/dev/null &&
    git log "${revs[@]}" -i -E --grep="$P" --format=%H 2>/dev/null &&
    git log "${revs[@]}" -i -E --author="$P" --format=%H 2>/dev/null &&
    git log "${revs[@]}" -i -E --committer="$P" --format=%H 2>/dev/null
} | sort -u) || attributed=0
in_head=0
hits=0
for sha in $shas; do
  # Ids only reach the log: anything else from git log is an attribution failure.
  if ! [[ "$sha" =~ ^[0-9a-f]{40}$ ]]; then
    attributed=0
    continue
  fi
  is_exempt "$sha" && continue
  if ! adds_hit "$sha"; then
    echo "sanitize-scan: commit $sha adds no matching text of its own (a removal, or a merge of known text)" >&2
    continue
  fi
  hits=1
  if git merge-base --is-ancestor "$sha" HEAD 2>/dev/null; then
    in_head=1
    where='in HEAD'
  else
    where='not in HEAD'
  fi
  reach=$(git for-each-ref --contains "$sha" --format='%(refname:short)' refs/remotes/origin refs/tags | redact | tr '\n' ' ') ||
    reach='(branch lookup failed)'
  echo "sanitize-scan: hit in commit $sha ($where)${reach:+ — reached by: $reach}" >&2
done
# Only known text was found: clean, once attribution shows no other commit added it.
if [ "$a$b$t$c" = "0000" ] && [ "$attributed" = 1 ] && [ "$hits" = 0 ]; then
  echo "sanitize-scan: clean — $known match(es) are known-leak text, and no other commit adds it"
  exit 0
fi
if [ "$a$b$t$c" = "0000" ] && [ "$attributed" = 0 ]; then
  echo "sanitize-scan: could not tell whether another commit adds known-leak text (git log failed) — not a clean result" >&2
  exit 2
fi
if [ "$hits" = 1 ] && [ "$pr" = 0 ]; then
  if [ "$in_head" = 1 ]; then
    echo "sanitize-scan: HEAD itself carries a hit" >&2
  else
    echo "sanitize-scan: no attributed hit is in HEAD — they are on the branches named above" >&2
  fi
fi
[ "$c" = "0" ] || echo "sanitize-scan: a branch or tag name is a hit (name withheld)" >&2
[ "$t" = "0" ] || echo "sanitize-scan: an annotated tag message is a hit" >&2
if [ "$attributed" = 0 ]; then
  echo "sanitize-scan: could not attribute the hit (git log failed; a pattern git cannot compile?) — rerun the scan locally to locate it" >&2
elif [ "$hits" = 0 ] && [ "$c$t" = "00" ]; then
  echo "sanitize-scan: no commit diff or message matched — the hit is in a path name, another commit header, an annotated tag, or uses a grep-only regex extension" >&2
fi
echo "sanitize-scan: FORBIDDEN PATTERN FOUND" >&2
exit 1
