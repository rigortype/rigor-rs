#!/usr/bin/env bash
# Run the review gate (docs/agents/review.md) on a PR: both external passes,
# in parallel, against one shared build of the PR head.
#
# Usage: harness/review.sh <PR number> [--full] [--keep]
#
# Run it once per PR, on the final head, after the draft's CI is green — not
# on every push. A PR that changes crates/ gets both passes; a PR that changes
# only harness/, docs/ or CI gets the Opus pass alone. --full forces both.
#
# 1. Checks out the PR head ($OUT/head) and its merge base ($OUT/base) in
#    detached worktrees, populates their reference/rigor submodules, and
#    builds target/release in each. harness/probe.py run from $OUT/head
#    measures the PR; run from $OUT/base it measures the pre-PR port. The
#    passes get both, so neither builds its own copy.
# 2. Starts the passes at once: Grok 4.6:high and Opus 5.5:high (`pi -p`,
#    no edit/write tools, cwd $OUT/head). The contract is docs/agents/review.md
#    as of the PR's base commit, never the PR's copy, so a PR cannot edit the
#    gate it is judged by (a PR that introduces the file falls back to this
#    checkout's copy, with a warning).
# 3. Waits, then prints each pass's verdict line and where its report is.
#
# $OUT defaults to ${TMPDIR:-/tmp}/rigor-review/pr-<N>-<sha>. Every worktree
# under $OUT (including any a pass created) is removed afterwards unless --keep
# is given; the reports are always kept.
# Exit status: 0 when both passes return Approved, 1 otherwise.
set -euo pipefail

usage() { echo "usage: $0 <PR number> [--full] [--keep]" >&2; exit 2; }
[[ $# -ge 1 && $1 =~ ^[0-9]+$ ]] || usage
PR=$1; shift
KEEP=0; FULL=0
for a in "$@"; do
  case $a in --keep) KEEP=1 ;; --full) FULL=1 ;; *) usage ;; esac
done

REPO=$(cd "$(dirname "$0")/.." && pwd)

read -r SHA BASE_REF < <(gh pr view "$PR" --repo rigortype/rigor-rs \
  --json headRefOid,baseRefOid --jq '"\(.headRefOid) \(.baseRefOid)"')
# Run from the PR's own checkout right after a push, GitHub can still report
# the previous head; refuse rather than review a stale commit.
LOCAL=$(git -C "$REPO" rev-parse HEAD)
if [[ $LOCAL != "$SHA" ]] && git -C "$REPO" merge-base --is-ancestor "$SHA" "$LOCAL" 2>/dev/null; then
  echo "GitHub reports head ${SHA:0:7} for #$PR, but this checkout is at ${LOCAL:0:7}," \
       "a descendant: the push has not propagated yet. Retry in a minute." >&2
  exit 2
fi
OUT=${OUT:-${TMPDIR:-/tmp}/rigor-review/pr-$PR-${SHA:0:7}}
mkdir -p "$OUT"
# git records worktrees under their real path; macOS's $TMPDIR is a symlinked
# /var path ending in "/", so normalise or the cleanup below matches nothing.
OUT=$(cd "$OUT" && pwd -P)

cleanup() {
  [[ $KEEP == 1 ]] && return
  git -C "$REPO" worktree list --porcelain | sed -n "s|^worktree \($OUT/.*\)|\1|p" |
    while read -r w; do git -C "$REPO" worktree remove --force "$w"; done
  git -C "$REPO" worktree prune
}
trap cleanup EXIT
echo "PR #$PR at $SHA -> $OUT" >&2

git -C "$REPO" fetch -q origin "pull/$PR/head" "$BASE_REF"
BASE=$(git -C "$REPO" merge-base "$SHA" "$BASE_REF")

CONTRACT=$OUT/contract.md
if ! git -C "$REPO" show "$BASE_REF:docs/agents/review.md" >"$CONTRACT" 2>/dev/null; then
  echo "warning: the PR's base has no docs/agents/review.md; using this checkout's copy" >&2
  cp "$REPO/docs/agents/review.md" "$CONTRACT"
fi

# Engine changes get both passes; they catch different things (on PR #154 only
# the Opus pass found the message regressions, and Grok found shapes Opus did
# not probe). Harness, docs and CI changes get the Opus pass alone.
PASSES=("opus=claude-bridge/claude-opus-5-5:high")
if [[ $FULL == 1 ]] || git -C "$REPO" diff --name-only "$BASE" "$SHA" | grep -q '^crates/'; then
  PASSES=("grok=xai/grok-4.6:high" "${PASSES[@]}")
fi
echo "passes: ${PASSES[*]%%=*}" >&2
for side in head base; do
  rev=$SHA; [[ $side == base ]] && rev=$BASE
  if [[ ! -d $OUT/$side ]]; then
    git -C "$REPO" worktree add -q --detach "$OUT/$side" "$rev"
  fi
  git -C "$OUT/$side" submodule update -q --init reference/rigor
  echo "building $side ${rev:0:7} (cargo build --release) ..." >&2
  (cd "$OUT/$side" && cargo build -q --release --locked -p rigor-cli)
done

# The head's probe.py when it has one (it probes the head's own reference pin),
# else this checkout's; RIGOR_RS_BIN selects which port build it measures.
PROBE=$OUT/head/harness/probe.py
[[ -f $PROBE ]] || PROBE=$REPO/harness/probe.py
PROMPT="Review PR #$PR at head $SHA (gh pr view $PR; gh pr diff $PR). \
The working directory is a checkout of that head. Two port builds are ready: \
the PR at $OUT/head/target/release/rigor and the pre-PR port (merge base \
${BASE:0:7}) at $OUT/base/target/release/rigor. Probe with $PROBE; set \
RIGOR_RS_BIN to one of those binaries to choose the port, so you can tell a \
new divergence from an old one. Do not create worktrees or build another \
copy. Put every file you create under the probe directory given below; write \
nowhere else."

pids=()
for p in "${PASSES[@]}"; do
  name=${p%%=*}; model=${p#*=}
  mkdir -p "$OUT/$name/probes"
  (
    cd "$OUT/head"
    start=$(date +%s)
    set +e
    pi -p --no-session --model "$model" --exclude-tools edit,write \
      --append-system-prompt "$CONTRACT" \
      "$PROMPT Probe directory: $OUT/$name/probes" \
      >"$OUT/$name/report.md" 2>"$OUT/$name/err.log"
    echo "exit=$? secs=$(( $(date +%s) - start ))" >"$OUT/$name/done"
  ) &
  pids+=($!)
  echo "started $name ($model)" >&2
done
wait "${pids[@]}"

status=0
for p in "${PASSES[@]}"; do
  name=${p%%=*}
  verdict=$(awk 'NF { last = $0 } END { print last }' "$OUT/$name/report.md" | sed -nE 's/^[*_ ]*Verdict: *(Approved|Needs fix|Blocked — need human)[*_ .]*$/\1/p')
  echo "$name: ${verdict:-no verdict} ($(cat "$OUT/$name/done")) -> $OUT/$name/report.md"
  if [[ -z $verdict ]]; then tail -3 "$OUT/$name/err.log" | sed 's/^/    /'; fi
  [[ $verdict == Approved ]] || status=1
done

exit $status
