#!/usr/bin/env bash
# Run the review gate (docs/agents/review.md) on a PR: both external passes,
# in parallel, against one shared build of the PR head.
#
# Usage: harness/review.sh <PR number> [--keep]
#
# 1. Checks out the PR head in a detached worktree under $OUT/head, populates
#    its reference/rigor submodule, and builds target/release there, so
#    harness/probe.py run from $OUT/head measures the PR, not master.
# 2. Starts the Grok 4.6:high and Opus 5.5:high passes at once (`pi -p`,
#    read-only tools, cwd $OUT/head). The contract is THIS checkout's
#    docs/agents/review.md, never the PR's copy, so a PR cannot edit the gate
#    it is judged by.
# 3. Waits, then prints each pass's verdict line and where its report is.
#
# $OUT defaults to ${TMPDIR:-/tmp}/rigor-review/pr-<N>-<sha>. The head worktree
# is removed afterwards unless --keep is given; the reports are always kept.
# Exit status: 0 when both passes return Approved, 1 otherwise.
set -euo pipefail

usage() { echo "usage: $0 <PR number> [--keep]" >&2; exit 2; }
[[ $# -ge 1 && $1 =~ ^[0-9]+$ ]] || usage
PR=$1; shift
KEEP=0
for a in "$@"; do [[ $a == --keep ]] && KEEP=1 || usage; done

REPO=$(cd "$(dirname "$0")/.." && pwd)
CONTRACT="$REPO/docs/agents/review.md"
PASSES=("grok=xai/grok-4.6:high" "opus=claude-bridge/claude-opus-5-5:high")

SHA=$(gh pr view "$PR" --repo rigortype/rigor-rs --json headRefOid --jq .headRefOid)
OUT=${OUT:-${TMPDIR:-/tmp}/rigor-review/pr-$PR-${SHA:0:7}}
mkdir -p "$OUT"
echo "PR #$PR at $SHA -> $OUT" >&2

git -C "$REPO" fetch -q origin "pull/$PR/head"
if [[ ! -d $OUT/head ]]; then
  git -C "$REPO" worktree add -q --detach "$OUT/head" "$SHA"
fi
git -C "$OUT/head" submodule update -q --init reference/rigor
echo "building the PR head (cargo build --release) ..." >&2
(cd "$OUT/head" && cargo build -q --release --offline -p rigor-cli)

PROMPT="Review PR #$PR at head $SHA (gh pr view $PR; gh pr diff $PR). \
The working directory is a checkout of that head with target/release/rigor \
already built from it, so harness/probe.py run here measures the PR. \
For the pre-PR port, build the PR's merge base yourself under the probe \
directory. Put every file you create under the probe directory given below; \
write nowhere else."

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

if [[ $KEEP == 0 ]]; then
  git -C "$REPO" worktree remove --force "$OUT/head"
fi
exit $status
