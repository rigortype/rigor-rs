# #429: static audit of the ADR-0045 conventions (PR #601)

Dated note for the `analysis_stack_audit` workspace test. The arc's ledger
line goes to `docs/CURRENT_WORK.md` on merge (fold convention: one line per
closed arc, master-side `note:` commit).

## What landed

`crates/rigor-cli/src/analysis_stack_audit.rs` — a `#[cfg(test)]` module
wired in at `main.rs`'s test block, modelled on
`rigor-parse`'s `family_inventory.rs` (`blank`/`close_of`/ident helpers
copied verbatim). One test, ~0.35 s, scans every `crates/*/src/**/*.rs` as
blanked text and fails on:

- **unbounded-parse paths**: `rigor_parse::parse`, `ruby_prism::parse`,
  `crate`/`self`/`super::parse` (rigor-parse files only) — called or taken
  as a value;
- **thread creation**: `thread::spawn`, `thread::scope`,
  `thread::Builder::new(`, `ThreadPoolBuilder::new(`/ `::default(` — unless
  the builder chain names `spawn_pool_thread`/`ANALYSIS_STACK_BYTES`, or
  the file is `rigor-cli/src/threads.rs`;
- **`use` smuggling**: bare `parse` leaf under those roots or `ruby_prism`,
  `spawn`/`scope`/`Builder` under a `thread` segment, and `as`-aliases or
  `*` globs of any flagged root — including `ruby_prism` aliased or
  globbed at a deeper position (`use rigor_parse::ruby_prism as p`,
  through lib.rs's `pub use ruby_prism;`).

`#[cfg(test)]` items are skipped by extent (`;` or the matching `}`) when
an item keyword follows the attribute — `pub` looks one word deeper,
past any `(…)` visibility group, since it also heads a visibility-
qualified field (`struct S { #[cfg(test)] pub x: u8 }`, rigor-rs#653).
On a non-item position (field, variant, parameter, match arm) only the
attribute itself is masked; a keyword-headed one (a `union`/`macro_rules`
field or param name, a `fn()`-typed field) still goes through `item_end`,
which never crosses a `)`/`]`/`}` the item did not open — so the next
item is not swallowed with it. A keyword-headed generic parameter (a
`const` inside `<…>`, #692) is the known limit. Files `#[cfg(test)] mod x;`
resolves to are
skipped whole (Rust 2018 module
resolution: `foo.rs`→`foo/x.rs`, `lib`/`main`/`mod.rs`→`x.rs`; nested
`mod a { mod b; }` followed). `use` items mask call patterns so each site
reports once under its `use:*` tag. The `ALLOW_LIST` pins (file, tag, count,
reason) — exact counts, so a moved/duplicated/removed site fails loudly.

Master's five entries: `lib.rs` + `depth.rs` `ruby_prism::parse` (the two
wrappers), `depth.rs` `thread::scope` + `thread::Builder::new(` (the
refused-tree release thread, sized by source length), `sidecar.rs`
`thread::spawn` (handshake reader, bounded IO). `coverage.rs`'s pool needs
no entry — `.spawn_handler(…spawn_pool_thread)` is chain-proven.

## Design points worth remembering

- **Token sequences, not literals.** `cargo fmt --check` is deliberately
  not a gate, so `std :: thread :: spawn (f)` is reachable code. Patterns
  match token-wise with whitespace allowed between tokens and whole-ident
  matching on each segment.
- **No trailing `(` for spawn/scope/parse** — turbofish
  (`spawn::<F,T>`) and function values (`let f = thread::spawn`) are the
  same hazard. `Builder::new` keeps the paren (`new` has no generics and a
  bare `Builder::new` token isn't a spawn).
- **`rayon::spawn`/`par_iter` unflagged** — work lands on the global pool,
  which `configure_global_pool` already sized; only thread *creation*
  bypasses the stack.
- **Deliberately conservative elsewhere**: `cfg(all(test))`/`cfg_attr`
  items scan as production (a false flag there, never a miss); a builder
  chain split across statements flags (allow-list it with a reason, or
  chain inline like `coverage.rs`).

## Accepted limits (pathological, disclosed in the PR)

`#[path]` modules outside `src/`, `include!` of non-src code, re-export
indirection deeper than the hop-one `use` items the scan does flag (e.g.
a leaf re-exported through a `pub mod` chain under a non-`thread` name),
macro-invented call text, builder `type`/`Default` aliases
(`type B = std::thread::Builder`, `let b: ThreadPoolBuilder =
Default::default()`, `<Builder>::new()`), `extern crate … as`,
`Builder::new` taken as a function value, a stack-proof token in an
unrelated chain link, and `+ use<>` precise capture (masked as a `use`
item). A human reviewer can spot these in a diff; the scan cannot.

## Process note — review delegation stalled

Three review delegates (Opus on `claudeAgent`, Opus on `opencode`, Fable
on `claudeAgent`) all died in `preparing` — `startedAt: null`, no worktree
assigned, ~45 min across two providers. The implementer self-reviewed
instead (maintainer instruction): the second commit's hardening
(whitespace paths, `as`-aliases, globs, turbofish, fn-values, use-item
dedup) is what that pass found, verified by a 21-probe trip-wire file and
an allow-list staleness check. **No independent `Approved` exists** —
self-merge barred; the PR is left ready for a human/independent review.

## Measured outcome

- `harness/gate.sh` pass; clippy 1.88 clean (fresh `CARGO_TARGET_DIR`);
  CI green on `ba418b9` (ubuntu + macOS build/test, docs-check, snapshot
  parity, live-reference snapshot); `mergeStateStatus` CLEAN.
- `fp_audit.py --gaps` on `reference/rigor/lib` (649 files, release):
  **0 FP**. The standing sweep's 8 corpora are not checked out on this
  machine (all SKIPPED); the reference's own lib is the substitute corpus.
- Scan cost ~0.35 s of `cargo test --workspace` — under the brief's 1 s bar.
