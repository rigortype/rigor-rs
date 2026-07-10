# sig-gen Writer UPDATE/merge + LayoutIndex — design (2026-07-10)

Design for porting the reference `Writer`'s UPDATE path + `LayoutIndex` into
rigor-rs (`crates/rigor-cli/src/sig_gen.rs`), based on (A) a full source-reading
report of `writer.rb`/`write_result.rb`/`layout_index.rb` and (B) empirical
oracle probes (fresh-dir, byte-exact). Implementation is delegated per the
AGENTS.md protocol; this note is the binding spec.

## Reference architecture (from investigation A)

**Parse-for-location, splice-as-text, reparse after every mutation.**

- `update_existing(source_path, target, candidates)`:
  1. read target text; `RBS::Parser.parse_signature` → decls; parse FAILURE →
     `action: :noop`, file untouched, applied/skipped both empty.
  2. `MergeState { source, decls, applied, skipped }`.
  3. group candidates by `class_name` → `merge_class` each; then
     `merge_class_shells` (shells: deferred in rigor-rs — no `Data.define`
     shell candidates yet).
  4. `action = applied.empty? ? :noop : :updated`; write ONLY when `:updated`.
- `merge_class`: find the class decl (recursive FQN walk). Found →
  `merge_into_existing_class`; not found → `append_new_class`. After either:
  **reparse** `state.source` for fresh byte offsets (reparse failure → keep
  stale decls silently).
- `merge_into_existing_class`:
  - `collect_member_pairs(decl)` = existing `(name, kind)` pairs from members:
    `MethodDefinition` → `(name, kind)`; `AttrReader` → `(name, :instance)`;
    `AttrWriter` → `(name=, :instance)`; `AttrAccessor` → both. `alias` does
    NOT count.
  - partition candidates: pair NOT present → new; present → conflicting.
  - new → `insert_into_class`: splice `"  {rbs}\n"` per method (FIXED 2-space
    indent regardless of nesting depth) immediately before
    `decl.location[:end].start_pos` (the closing `end` keyword's start byte).
    No blank lines added.
  - conflicting (default, no --overwrite) → skipped as `(candidate,
    :user_authored)`, file untouched for that member. NO signature comparison.
- `append_new_class` (class not declared in the existing file): append
  `"\nclass {FQN}{ < Super}\n{  rbs lines}\nend\n"` — COMPACT single header
  (`class Foo::Bar::Baz`), NOT nested modules; leading blank line; a missing
  trailing newline on the original file gets one first. All methods applied.
- `WriteResult { source_path, target_path, action, applied, skipped }`;
  actions: `created | updated | noop | skipped_outside_sig_root`; `to_h`:
  `{ source, target, action, applied: [cand..], skipped: [cand + write_skip_reason] }`.
- Renderer text: `updated <target> (+N, skipped M user-authored)`; all-noop →
  `No changes`.

## LayoutIndex (from investigation A)

- Scan `signature_paths` dirs (fallback `<root>/sig` if unset+exists) with
  sorted `**/*.rbs` glob; parse each; walk class/module decls recursively
  building `FQN → file` map; **first-found-wins** (`||=`); an unparseable file
  is skipped silently (`rescue StandardError`).
- `PathMapper.target_for(source_path, class_name:)` consults
  `layout_index.file_for(class_name)` FIRST; only a miss falls to the 1:1
  mirror. **Grouping is PER-CANDIDATE** (`write_all` groups by
  `target_for(c.path, class_name: c.class_name)`) — one source file's
  candidates can split across a consolidated target and a mirror target.

## rigor-rs substrate (verified)

`ruby-rbs 0.3.0` (already a rigor-index dependency) exposes everything needed:
- `parse(source)` → `SignatureNode.declarations()`; `Node::{Class,Module,
  MethodDefinition,AttrReader,AttrWriter,AttrAccessor,...}`.
- `ClassNode`/`ModuleNode`: `name()`, `members()`, **`end_location()`** →
  `RBSLocationRange { start()/end() }` byte offsets of the closing `end`.
- `MethodDefinitionNode`: `name()`, `kind()` (`Instance|Singleton|
  SingletonInstance`), `location()`.
- Attr nodes: `name()`, `kind()` (`AttributeKind::{Instance,Singleton}`).

Kind mapping for member pairs: `Instance→"instance"`, `Singleton→"singleton"`,
`SingletonInstance→"singleton_instance"` (matches NEITHER candidate kind — the
reference behaves identically since its candidates only carry
:instance/:singleton; do not "fix" this).

## rigor-rs design

New module section in `sig_gen.rs` (or a `sig_gen/` submodule if it grows):

1. **`LayoutIndex`**: `build(signature_paths, project_root) -> HashMap<String,
   PathBuf>` — sorted glob walk, `ruby_rbs::node::parse` per file, recursive
   decl walk accumulating FQNs, first-found-wins, per-file parse failure →
   skip. Fallback `sig/` handled by the existing `sig_root` resolution.
2. **`target_for` gains `class_name`**: consult the LayoutIndex map first.
   `cmd_write` grouping becomes per-candidate `(target, candidate)` (already
   the shape of `tagged`).
3. **`update_existing`** (new): mirrors the reference exactly —
   read → parse (failure → `noop`) → per-class merge with reparse between →
   write only when applied non-empty. Member-pair collection + partition +
   `insert_into_class` splice + `append_new_class` append, byte-for-byte per
   the oracle probes (investigation B pins the exact bytes).
4. **`WriteResult`** extended: `action: "updated" | "noop"` added; `skipped`
   carries `(Candidate, "user_authored")`; JSON `write_skip_reason` field.
5. Text renderer: `updated <target> (+N, skipped M user-authored)`;
   `No changes` when every result is `noop`/`skipped_exists`-free… (exact
   wording from probes).
6. `--overwrite` stays deferred (usage error), `merge_class_shells` deferred
   (no shell candidates in rigor-rs yet — no `Data.define` recognition).

## Parity bar (unchanged)

- Hard guarantee: on scenarios both tools act, the resulting FILE BYTES +
  stdout + JSON are byte-identical (E2E probes from investigation B become the
  gate fixtures).
- rigor-rs may still emit FEWER candidates (inference gaps) → an update may
  apply fewer methods; that is the established sound-subset behavior, never a
  wrong byte.
- check path untouched; harness 54/54 0 FP must hold (sig_gen-only change +
  possibly a small shared helper).

## Investigation-B refinements (oracle-probed, byte-exact — these BIND)

1. **The scenario-4 "indentation quirk" is EMERGENT — replicate by mechanism,
   never special-case.** `insert_into_class` splices at the closing `end`
   TOKEN's start byte (not line start) with a FIXED `"  {rbs}\n"` per line. In
   a nested body (`  end`), the line's leading 2 spaces end up PREFIXING the
   inserted first line (2+2 = 4-space rendering) and the `end` keyword lands at
   column 0. Implementing the same token-start splice + fixed indent reproduces
   the oracle bytes in BOTH the flat and nested cases with zero special-casing.
2. **Equivalence drop is REQUIRED for stdout parity.** The reference classifies
   candidates against the existing RBS env at GENERATION time: an EQUIVALENT
   (declared return == inferred return, erased) candidate never reaches the
   writer, so an idempotent re-run prints `No changes` and a mixed run prints
   the correct `skipped M` count. rigor-rs (no env-based classification yet)
   approximates this AT WRITE TIME: for a conflicting member, extract the
   existing member's RETURN TEXT (the substring after the LAST `->` at bracket
   depth 0 of the member's source slice; for an `attr_*` the declared type
   after `:`), and compare with the candidate's return text (same extraction
   from `candidate.rbs`):
   - EQUAL → equivalent → DROP silently (not applied, not skipped);
   - DIFFERENT → skipped `(candidate, user_authored)`, and the JSON skipped
     entry reports `classification: "tighter_return"` +
     `declared_return_rbs: <extracted text>` (matches every probed case);
   - extraction failure → treat as different, classification stays
     `new_method`, omit `declared_return_rbs` (documented residual).
3. **`action` values**: `created | updated | noop` (probe never produced
   `skipped_outside_sig_root`, keep ours). Appending a NEW CLASS to an existing
   file is `updated` (scenario 3). Malformed existing RBS → `noop`, exit 0,
   stdout `No changes`, file byte-untouched. (The reference also prints an
   env-build warning to STDERR in the malformed case — that comes from its
   project-sig ENV ingestion at generation time, which rigor-rs's sig-gen does
   not do yet; DOCUMENTED divergence, stderr only.)
4. **Result order follows the CLI path-argument order** (scenario 9 —
   reversed args flip the stdout lines). Grouping key is the PER-CANDIDATE
   target; results surface in first-seen candidate order.
5. **stdout formats** (byte-exact):
   - `updated <ABS target> (+N, skipped M user-authored)`
   - `created <ABS target> (K method(s))`
   - `No changes` when nothing was created/updated.
   Note targets print ABSOLUTE paths in both created and updated lines.
6. **JSON skipped entry shape**: candidate `to_h` fields + `write_skip_reason:
   "user_authored"` + (when extractable) `declared_return_rbs`, with
   `classification` as in refinement 2.
7. Fixture scenarios 1–9 from the probe report are the E2E gate — each in a
   FRESH dir, comparing stdout + written-file bytes + (normalized) JSON.

## Delegation plan

- Implementation: Opus subagent on branch `sig-gen-writer-update`, spec =
  this note + both investigation reports + the pitfalls list (below).
- Pitfalls to name explicitly in the prompt:
  - reparse-after-every-mutation (stale byte offsets otherwise);
  - fixed 2-space indent for inserted methods (NOT depth-derived);
  - `append_new_class` uses the COMPACT `class A::B::C` header (not nested
    modules) + leading blank line + trailing-newline repair;
  - parse-failure → `noop` (never treat-as-create / never touch the file);
  - alias does NOT block; attr_writer blocks `name=`;
  - per-candidate target grouping (a file's candidates may split);
  - i32 byte offsets from RBSLocationRange (cast carefully, validate bounds);
  - zsh word-split + reference `.rigor/cache` traps in any probe the
    implementer runs.
- Audit (main): re-run gates + independent byte probes vs the oracle on
  every investigation-B scenario before merge.
