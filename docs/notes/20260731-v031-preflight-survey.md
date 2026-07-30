# v0.3.1 pre-flight survey — what the next pin costs (2026-07-31)

`UPSTREAM.md` gates `v0.3.1` on the vendored-RBS bump (upstream follows
rbs 4.1.0; the port vendors rbs-4.0.3). This is the measurement that sizes that
arc **before** committing to it. Nothing here is landed — the pin stays on
`v0.3.0`.

Headline: **the bump is FP-free and roughly coverage-neutral.** The whole
`v0.3.0 → v0.3.1` delta is the rbs version; upstream's own logic contributes
literally nothing on the measured corpora.

## Method — the two axes must be separated

`v0.3.1`'s gemspec still allows `rbs (>= 3.0, < 5.0)`, so the same checkout runs
under either rbs. That makes the delta decomposable:

- **Axis A (upstream logic)** — `v0.3.0` vs `v0.3.1`, both under the ambient
  rbs 4.0.3.
- **Axis B (rbs version)** — `v0.3.1` vs itself, under rbs 4.0.3 vs 4.1.0.
  rbs 4.1.0 was installed into a scratchpad `--install-dir` and selected with
  `GEM_HOME`/`GEM_PATH`, so the user's gem environment is untouched.
- **Axis C (port side)** — rigor-rs with the embedded vendored 4.0.3 set vs
  `RIGOR_RBS_CORE_DIR` pointed at a 4.1.0 tree.

> Axis C needs care: pointing `RIGOR_RBS_CORE_DIR` at the raw rbs-4.1.0 gem
> loads **1137 classes** vs the vendored **538**, because the vendored tree is
> the `DEFAULT_LIBRARIES` transitive closure, not all of `stdlib/`. A diff
> against that is confounded by the extra libs. The measurement below uses a
> **mirror**: 4.1.0's `core/` plus exactly the 49 stdlib libs the vendored tree
> carries → **539 classes**, so the only variable is the signature content.

## Axis A — upstream logic delta: **zero**

Reference self-diff `v0.3.0` vs `v0.3.1`, both under rbs 4.0.3:

| corpus | files | v0.3.0 | v0.3.1 |
|---|---|---|---|
| mastodon `app` | 1236 | 459 | **459** (0 added / 0 dropped) |
| gitlab-foss `lib` | 4676 | 1374 | **1374** |
| survey `mail` | 874 | 7200 | **7200** |
| survey `concurrent-ruby` | 345 | 5804 | **5804** |

7131 files, not one diagnostic moves. The release's analysis-surface commits are
either rbs-4.1-compat plumbing (`a5ef974a` follow rbs 4.1.0, `5c903e73` renamed
core type-parameter names, `74837385` rebuild RBS type names on cache load) or
outside the default profile (`5591e06a` hosts the void-value-use collector on
the shared RuleWalk — still bleeding-edge). The rest is LSP incremental sync,
cache/`generation_cap:`, `skill describe --deep`, docs.

## Axis B — rbs 4.1.0 only ever *drops* diagnostics (5 / 7131 files)

Same checkout (`v0.3.1`), rbs 4.0.3 → 4.1.0:

| corpus | dropped | added |
|---|---|---|
| mastodon `app` | 1 — `call.argument-type-mismatch` `app/lib/request.rb:296` | 0 |
| gitlab-foss `lib` | 1 — `call.possible-nil-receiver` `middleware/go.rb:137` | 0 |
| survey `concurrent-ruby` | 1 — `call.wrong-arity` `erlang_actor.rb:665` | 0 |
| survey `mail` | 2 — `possible-nil-receiver` (rdoc `comment.rb:324`), `wrong-arity` (rspec `contain_exactly.rb:213`) | 0 |

**rigor-rs is currently silent at all five** (each is a reference-only coverage
gap today), so every one of them is a gap that closes for free. Zero FP exposure
from this axis.

## Axis C — port side: 2 diagnostics lost, 0 gained

rigor-rs under the 4.1.0 mirror, same corpora: mastodon / concurrent-ruby / mail
**identical**; gitlab-foss `lib` **1044 → 1042**, both losses being
ActiveSupport methods witnessed on `Hash`:

- `call.undefined-method` — `undefined method 'presence' for Hash`
  (`gitlab/ci/yaml_processor/result.rb:195`)
- `call.undefined-method` — `undefined method 'with_indifferent_access' for Hash`
  (`import/offline/groups/transformers/group_attributes_transformer.rb:39`)

Both are currently **matched** with the oracle, and the reference keeps emitting
them under 4.1.0 (they are not in the Axis-B drop list) — so post-bump they
become **2 new coverage gaps**, not FPs. This is the one behavioural item the
arc actually has to explain: something in 4.1.0's `hash.rbs` / core rewrite moves
`Hash` out of the witness gate's reach where upstream's own resolution still
reaches it.

## End-to-end simulation — the gate after the bump

Both sides moved at once (reference `v0.3.1` + rbs 4.1.0 vs rigor-rs + the 4.1.0
mirror), via `fp_audit.py`:

| corpus | FP | gaps before | gaps after |
|---|---|---|---|
| mastodon `app` | **0** | 49 | **48** |
| gitlab-foss `lib` | **0** | 330 | **331** |

Net: **0 FP, coverage ±1.** The bump is not a risk item; it is a mechanical
re-vendor plus one witness-gate question.

## Re-vendor cost (`crates/rigor-index/vendor/rbs`, 4.0.3 → 4.1.0)

- `core/`: **2702 changed lines**; three new files — `file_constants.rbs`,
  `file_stat.rbs` (File is split up), `rbs/ops.rbs`.
- stdlib closure: 14 of the 49 libs have changed content (`csv`, `json`,
  `resolv`, `stringio`, `strscan`, `fileutils`, `erb`, `etc`, `digest`,
  `delegate`, `abbrev`, `ipaddr`, `monitor`, `shellwords`).
- **`tempfile` gains a `manifest.yaml` in 4.1.0** — so the vendored set must be
  recomputed as a transitive closure over `manifest.yaml` `dependencies:`
  (PROVENANCE.md's recipe), not copied file-for-file.
- Upstream's `DEFAULT_LIBRARIES` **list is unchanged** in `v0.3.1` (the diff is
  comment-only), so `src/rbs.rs` needs no edit on that account.
- Class count moves 538 → 539 under the same closure.

## Arc sketch

1. Re-vendor rbs-4.1.0 by PROVENANCE.md's recipe (closure recomputed) + update
   `PROVENANCE.md`.
2. Bump the submodule to `v0.3.1`, re-baseline snapshots, run both gates.
3. Explain / decide the two `Hash` witness losses above (accept as gaps, or
   restore parity).
4. Re-measure the sweep set; expect 0 FP and the Axis-B gaps to have closed.

## Beyond v0.3.1: what is already on upstream master (+49 commits)

Not a target yet, but it changes what the arc after this one looks like:
a new **inline-RBS "annotation parsed but not honoured" report** (`2ce7655c`),
referenced-type stub passes (`9515c8f8`, `5bd0aac2`), Tuple carrier set
operations (`a2867efd`), sig-gen `Data.define` / `Struct.new` reading
(`da9b045e`), editor mode with whole-project scope (`c106c7c0`), and a
**backport of rbs 4.1's `Resolv#initialize` fix as a core overlay**
(`1416111b`). Notably `14cca5f4` **declines** flipping `parameter_inference:` on
by default (#205) — so it stays opt-in, and the port can keep deferring it.
