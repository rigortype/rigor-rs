# Triage Labels

The skills speak in terms of five canonical triage roles. This file maps those roles to the actual label strings used in this repo's issue tracker.

| Label in mattpocock/skills | Label in our tracker | Meaning                                  |
| -------------------------- | -------------------- | ---------------------------------------- |
| `needs-triage`             | `needs-triage`       | Maintainer needs to evaluate this issue  |
| `needs-info`               | `needs-info`         | Waiting on reporter for more information |
| `ready-for-agent`          | `ready-for-agent`    | Fully specified, ready for an AFK agent  |
| `ready-for-human`          | `ready-for-human`    | Requires human implementation            |
| `wontfix`                  | `wontfix`            | Will not be actioned                     |

When a skill mentions a role (e.g. "apply the AFK-ready triage label"), use the corresponding label string from this table.

## Claim marker (not a triage role)

`in-progress` marks a `ready-for-agent` issue an agent has claimed but has not
yet opened a draft PR for. It sits beside the state role rather than replacing
it, and is removed once the draft PR (with `Closes #N`) exists. The protocol is
in `AGENTS.md` → "Work loop".

## Review tiers (pull requests)

These labels select the review bar in `docs/agents/review.md`. They are not
triage roles and do not replace `ready-for-agent` / `in-progress`. A PR
carries at most one. **No review-tier label means `review:full`.**

| Label          | Meaning                                                                 |
| -------------- | ----------------------------------------------------------------------- |
| `review:full`  | Default. Adversarial oracle re-probe; standing-set fp_audit before merge. |
| `review:light` | Docs or comments only. Not for `harness/`, `docs_check.py`, `AGENTS.md`, or `docs/agents/review.md`. Short review; `docs.yml` green is enough. |
| `review:human` | Human-only path (or so labelled). Agents do not self-merge.             |

