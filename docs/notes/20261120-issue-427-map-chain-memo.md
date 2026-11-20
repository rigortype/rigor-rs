# #427 — the `.map` repro's type building was an env-memo hole (PR #726)

Date: 2026-11-20. Closes rigor-rs#427's last acceptance row after PR #648
(iterative render) and #680 (stackless `Interner::cmp`, iterative
`literal_seed`/sig-gen predicates): the body's `.map` repro — `[1]` +
100 × `.map { |y| [[…9000…[y]…]] }` + `.foo` — never ran to completion.

## Where the time went (master 6427298d, release, aarch64)

The repro is a `Call` chain of 100 block calls. Every chain node is
env-sensitive (`env_sensitive_within` counts a block call), so `type_of`
routes it to `type_of_env_memo` — whose store gate was the FLAT memo's
`type_is_interner_free`. Every link's answer is a deep `Tuple` embedding
interner `TypeId`s, so nothing ever stored. And a block call types its
receiver TWICE — the #479 `literal_concat_declines` gate once,
`type_block_call` once more — so with nothing stored, every driver visit
(the rules type every `Call` node's receiver) re-descended its whole suffix
and the work DOUBLED per link: `2^k − 1` `type_block_call` events for the
k-th link's visit (757 for an 8-link chain, measured in the unit harness),
each re-typed link paying 4 whole-arena scans. Exponential in chain
length: 5 links 0.54 s, 10 links 9.2 s, 20 links >9 min; the 100-link
repro is 2^100 typing work.

## The fix

`EnvExprEntry` already scopes a hit to the interner that produced it
(`interner: uid`, compared first), so an answer's `TypeId`s re-intern
under the SAME arena on a hit — a dedup hit, never a cross-interner
translation. (The entry's env snapshot has always held `TypeId`s under
exactly that trust.) The store gate drops for the env memo only; the
FLAT memo keeps it (a flat entry re-interns under whichever interner the
hit arrives with).

## Measured after (release, aarch64; reviewer's re-measure in the PR)

| input | master | after |
|---|---|---|
| `.map` repro (100 × wrap9000, ~900k levels) | never finishes | exit 1, 67.7–71.4 s / 691–742 MB (budget 2 × 112 s) |
| `rigor lsp` didOpen of the repro | 20 links: no publish in 300 s | publishes in 72–74 s, shutdown exit 0 |
| depth-2 chain 18/20/22/40 links | 0.70/2.23/8.53 s/>100 s | flat 0.20–0.22 s |
| bigwrap70 (630k levels) | 29.5 s / 517 MB | 29.3 s / 515 MB, JSON byte-identical |

Byte-identical diagnostics: master-vs-PR over all 8 sweep corpora
(9,036 files), 16 block-chain rows, and 39 reviewer rows (mutator chains,
captured rebinds, `&.` block calls, def/lambda/class-nested chains,
loop-pass shapes, project `sig/`). Sweep 0 FP; gap totals identical
(212/138/59/18/8/2). `RIGOR_REACH_MEMO_AUDIT=1` clean over the fixtures
AND the 9,036 sweep files. Gem-tree timing unchanged (gitlab-foss/lib
4,892 files: 109.1 s vs 108.6 s).

## Review

Opus 5.5 adversarial: **Approved** (re-probed every claim; confirmed the
exponential scaling and pre-existence of the 4 receiver-display drift
rows). Grok 4.6 advisory and the Fable design companion: unavailable
(402 quota; account rate limit) — recorded in the PR, per the #680
precedent. Round-1 findings: exponential wording everywhere, fail-fast
LINKS=10, the `loop_pass`-not-in-key note (#699), and two filed residuals.

## Residuals (filed)

- #650 display O(depth²) bytes copied — ~60 s of the repro's ~70 s.
- #729 per-event whole-arena scans (`block_level_jumps` ×2,
  `fold_body_has_unmodelled_write` ×2) — the repro's dominant typing cost.
- #731 env > `ENV_TYPE_MEMO_MAX` (32) keeps the exponential path (each
  block call types its receiver twice — the single-typing fix shape).
- #732 pre-existing FP: a `&.`-chain link drops a conditional receiver's
  nil arm (found by the review, fires on master too).
