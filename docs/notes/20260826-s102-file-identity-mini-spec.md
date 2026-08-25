# S102 mini-spec — path-derived identity for the constant-consumption gate (2026-08-26)

Implements issue #102. Evidence base: the #92 probe (`HarvestedConst.file_id`
is a process-global counter, compared at the use site — flagged then as the
persistence hazard) and the PR #101 audit (the hazard biting in-memory: a
cache-hit hover lowers the buffer afresh, gets a NEW counter value, and the
C5 per-file gate declines the same-file literal-constant fold —
`FOO : 5` → `FOO : Dynamic[top]`, pinned by
`crossfile_cache_hit_declines_the_same_file_literal_constant_fold`).

## The change

The C5 constant-VALUE consumption gate is per-FILE (source-confirmed against
the reference, [note](20260808-partial-constant-harvest-mini-spec.md)). Its
identity today is `LoweredAst::file_id` equality. Replace the gate's
identity with a **path-derived key** — stable across lowerings, processes,
and (later) persisted harvests:

- `rigor-parse`: `LoweredAst` (or the lowering entry point) carries a stable
  `file_key` derived from the file's canonical path (the same canonical form
  the LSP's held table already uses). `file_id` itself may stay for any
  other role it has — inventory its consumers first and report; if the C5
  gate is its ONLY consumer, retire it rather than leaving two identities.
- `rigor-infer`: `HarvestedConst` stores and compares the path key; the
  comparison sites change mechanically. Semantics are IDENTICAL — the path
  is what "per-file" always meant; the counter was an implementation
  accident.
- Buffers: the LSP analyzes buffer text for a file that exists on disk at a
  known path — the buffer's lowering takes that path's key, which is
  exactly what makes the cache-hit hover match. A pathless input (tests,
  stdin) gets a key derived from whatever identity the caller supplies
  today; two DIFFERENT pathless inputs must not collide (pin with a test).

## Tests

1. **FLIP the pinned regression test**: from documents-the-degradation to
   asserts-none (`FOO : 5` preserved on a cache hit). This is the slice's
   headline acceptance.
2. Two distinct files defining the same constant name still gate per-file
   (the C5 rule itself, unchanged — probe example 3's shape).
3. Pathless-input non-collision (above).
4. Existing suites untouched and green.

## Acceptance gates (BARE, the standard battery)

1. `cargo test --workspace`; clippy fresh `CARGO_TARGET_DIR` `-D warnings`.
2. `harness/run_snapshot.rb` PASS, 0 unregistered extras.
3. Release rebuild + `harness/fp_audit.py --gaps --sweep` — 0 FP / 9,204,
   gap set byte-unchanged (this touches a live gate: the sweep is the
   proof the semantics did not move).
4. `rigor check` byte-identical vs a master binary on mastodon/app under
   both thread modes.

## Non-goals

No persistence (this UNBLOCKS the harvest-cache slice; it does not build
it), no other consumer of the new key, no change to what the C5 gate
admits.
