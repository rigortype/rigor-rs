# Upstream pin

rigor-rs is a port of the Ruby **Rigor** reference implementation
([`rigortype/rigor`](https://github.com/rigortype/rigor)). The reference is the
parity oracle ([ADR-0002](docs/adr/0002-diagnostic-set-parity.md)): for a given
input, rigor-rs's `(rule id, location)` diagnostic set must match the reference's
(message wording may improve; the set must match).

To make that comparison **reproducible**, the reference is pinned as a git
submodule rather than tracked against a drifting local checkout.

## Pinned version

| | |
|---|---|
| Upstream repo | `git@github.com:rigortype/rigor.git` |
| Submodule path | `reference/rigor` |
| **Pinned ref** | **v0.3.0 release candidate** (post-`v0.2.9` master; `--version` still prints 0.2.9) |
| Commit | `7a69f142` (Merge PR #188) |

> The pin is a **commit, not a tag**: upstream is at the v0.3.0
> release-candidate stage and the port tracks the RC's gap set ahead of the
> tag. Re-pin to the real `v0.3.0` tag when it lands. Previous pin: `47ec8625`
> (Merge PR #109); the `47ec8625 → 7a69f142` bump (80 commits) landed two new
> parity divergences — `suppression.unknown-marker` (a new rule) and the Kernel
> intrinsic explicit-`Kernel.`-receiver fold — both closed with 0 FP; the rest
> of the RC's inference precision (void→top, `(?)` return, `Array#join` /
> `Data.define` / `Struct` folds, regex-match narrowing) only widens coverage
> gaps (reference-only), which stay FP-safe and shrink as the port progresses.

The differential harness (`harness/run.rb`, `harness/snapshot.rb`) defaults
`REFERENCE_RIGOR_DIR` to this submodule (`harness/lib.rb`). The reference-free
snapshot gate (`harness/run_snapshot.rb`, the CI `parity` job) never touches it —
it replays the pinned snapshots under `harness/snapshots/`, which were generated
from this exact reference version.

Note: the vendored RBS (`crates/rigor-index/vendor/rbs`, **rbs-4.0.3**) is pinned
**independently** of the reference tag — see its `PROVENANCE.md`. The reference
bundles rbs-4.0.3 from `v0.2.7` through the current v0.3.0-RC pin, so the two
pins match exactly.

## First-time setup

```sh
git submodule update --init reference/rigor
# The reference is plain Ruby run in place — no build step:
ruby -I reference/rigor/lib reference/rigor/exe/rigor --version   # -> rigor 0.2.9 (v0.3.0 RC)
```

## Bumping the pin (following upstream)

1. Fetch + check out the new tag inside the submodule:
   ```sh
   cd reference/rigor && git fetch --tags && git checkout vX.Y.Z && cd -
   ```
2. Record the new gitlink in the superproject: `git add reference/rigor`.
3. **Re-baseline the harness** against the new reference:
   ```sh
   ruby harness/snapshot.rb        # regenerate harness/snapshots/*.json
   ruby harness/run.rb             # live differential — must PASS, 0 FP
   ruby harness/run_snapshot.rb    # reference-free gate — must PASS
   ```
4. Review the snapshot diff: any newly-appearing reference diagnostics are
   candidate coverage to port (new rules / behaviours in `vX.Y.Z`); any that
   rigor-rs now emits but the reference dropped is a regression to fix or a
   divergence to register ([ADR-0011](docs/adr/0011-reference-oracle-exceptions.md)).
5. **Re-measure the ported reference-implementation constants** — they can move
   silently across releases. Currently: the shape-tier thresholds
   ([ADR-0039](docs/adr/0039-shape-typing-tier.md)) — `ARRAY_NEW_TUPLE_LIMIT`
   (grep `method_dispatcher.rb`, empirically probe `Array.new(n)`-slice
   possible-nil around the boundary) and, once ported, the other
   `constant_folding.rb` / `shape_dispatch.rb` limits.
6. Update the tag/commit in this file and note the bump in `docs/CURRENT_WORK.md`.
