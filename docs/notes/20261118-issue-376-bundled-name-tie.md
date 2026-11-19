# #376: bundled-name tables — live desync found, mechanical tie added

Issue #376 predicted the shape: `BUNDLED_PLUGIN_NAMES` /
`BUNDLED_SKILL_NAMES` (`crates/rigor-cli/src/main.rs`) were transcriptions of
`reference/rigor/{plugins,examples,skills}` at pin `e59b7b89` with nothing to
catch a bump adding or removing a directory. Landed as PR #609.

## The desync was already live

At pin `bbea1bac` (v0.4.8) `skills/` gained `rigor-rbs-contract`
(upstream `59ca59e`+). `rigor skill nosuchskill` on the oracle lists 18
names; the port listed 17. The name also gated dispatch —
`rigor skill rigor-rbs-contract` was `Unknown skill` (exit 1) in the port
where the reference resolves it (prints SKILL.md; the port defers all skill
bodies, exit 2 — a separate accepted phase divergence). The plugins+examples
table (48 entries) was still exact.

## What landed

- The missing entry, in `discover_skills` order.
- `bundled_name_tables_match_the_pinned_reference` — re-derives both tables
  from the populated submodule exactly as `PluginCommand#discover` /
  `SkillCommand#discover_skills` do (`Dir.children(root).sort`, directories
  only, `SKILL.md` filter for skills). Same mechanism — and same skip — as
  the pre-existing `config_audit::bundled_plugin_gems_match_the_pin`.
- `UPSTREAM.md` step 6: the tables join the bump ritual's re-measure list.

## Where the tie does NOT fire (the review's catch)

Opus round 1 was `Needs fix` on a doc-claim error: the test skips with no
submodule (CI `build + test` checks out without `submodules: true`), and
`gate.sh` runs `cargo test -p` only for crates the branch touched — a
gitlink-only bump (real example: `8b7df43`, the very bump that caused this
desync) runs zero cargo tests. Past bumps got lucky on incidental
`crates/` touches. The tie is the step-6 run in the populated checkout a
bump is always performed in — the docs now say exactly that.

## Follow-ups (filed, not landed)

- **#610** — `gate.sh` should add `rigor-cli` to `CRATES` when `CHANGED`
  contains `reference/rigor` (human-only path; the mechanical version of
  this step).
- **#611** — generate this whole table class from the oracle's own code in
  the submodule-equipped CI job, and assert `SILENT_BUNDLED_IDS` ⊆ pinned
  plugin ids.

Gates at merge: `gate.sh` pass (612 rigor-cli tests); `run_snapshot.rb`
0 unregistered FP; `fp_audit` 0 FP / 8,251 files (4 corpora present here;
mail/Ruby/concurrent-ruby/haml absent → SKIPPED); CI 5/5 on `c0a95e5`.
