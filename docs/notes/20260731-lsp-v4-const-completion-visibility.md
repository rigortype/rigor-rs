# LSP v4 slice — `::` namespace completion, private-visibility filter, union intersection

2026-07-31. Branch `lsp-v4-const-completion-visibility`. Closes the first three
`LSP v4+` items in `PORT_BACKLOG.md`. Reference oracle: pinned submodule
`reference/rigor`, `lib/rigor/language_server/completion_provider.rb`.

## What was wrong

Three defects, all in `crates/rigor-cli/src/lsp.rs`'s `completion`:

1. **`Foo::` answered with SINGLETON METHODS.** The provider spliced one
   lowercase stub after either separator, so `Process::` parsed as
   `Process::rigorCompletionHole` — a class-method call — and the popup offered
   `wait`, `spawn`, … where the user was writing `Process::Status`. Nested
   constants were unreachable from completion entirely.
2. **Private methods were offered on an explicit receiver.** `"x".` listed
   `respond_to_missing?` and `method_missing`; `Time.` listed `private`,
   `public`, `refine`, `using`, `module_function`, `included`, `inherited` —
   every one of them a `NoMethodError` at the receiver they were offered on.
   The index simply did not read RBS visibility: `ClassEntry` had no
   visibility field and `collect_members` ignored both the per-`def`
   `private def x:` form and the bare `private` section modifier.
3. **A union receiver answered with nothing.** `class_name_of` returns `None`
   for `Type::Union`, so `x.` on a `String | Integer` was a null completion.

## The three changes

**`crates/rigor-index/src/rbs.rs`** — `ClassEntry.private_methods`, harvested in
`collect_members` from `MethodDefinitionNode::visibility()` with a running
`section_private` flag for the bare `private` / `public` members, merged in both
`merge` and `merge_qualified` under the SAME first-write-wins reopen gate as
`void_methods` (visibility belongs to the definition being recorded). Read only
by `instance_method_names`, which now filters it per-declaring-ancestor.
Plus `namespace_children(parent_fqn) -> Vec<(&'static str, bool)>` over the
ADR-0042 qualified registry: immediate children only, with `is_module`.

**`crates/rigor-cli/src/lsp.rs`** — `completion` splits on the case of the name
being typed after `::` and routes the constant case to a new
`namespace_completion`, which splices `RigorCompletionHole` (uppercase, so it
parses as a constant path), reads the lowered `ConstantRead`'s dotted name, and
strips the stub segment to get the parent FQN. `method_names_for` gained a
`Type::Union` arm that intersects the arms' sets.

## Where this matches the reference, and where it diverges

Matched:

- **The constant-vs-method split after `::`.** `Foo::` and `Foo::Ba` complete
  constants; `Foo::ba` completes singleton methods. Same behaviour, reached
  differently — see below.
- **Immediate children only.** `enumerate_constant_children` drops any tail
  containing `::`; so does `namespace_children`. `Thread::` offers `Backtrace`,
  never `Backtrace::Location`.
- **The enumeration SURFACE.** The reference walks `RbsLoader`'s
  `known_class_names_set`; the qualified registry is this port's equivalent.
  Neither sees a class defined in the edited buffer — which is also the S4b
  "hover / completion keep the single-file index" carve-out, so it is a
  deliberate scope, not an oversight.
- **Empty ⇒ null.** `return nil if children.empty?` / `if names.is_empty()`.
- **`next nil unless method.public?`** — the private filter, item 2.
- **Union ⇒ intersection, and an UNRESOLVABLE arm is SKIPPED, not a veto.**
  The reference's `intersect_member_methods` uses `filter_map`, so an arm whose
  method set is nil drops out of the reduce rather than emptying the result.
  Ported as `.filter(|s| !s.is_empty())` before the fold.

Diverged, deliberately:

- **How the split is decided.** The reference parses the raw buffer first and
  dispatches on the located Prism node's class (`ConstantPathNode` ⇒ constants,
  `CallNode` ⇒ methods), falling back to an uppercase / lowercase sentinel only
  when the buffer does not parse. rigor-rs decides on the case of the typed
  prefix directly. Ruby's own rule for "constant or method call after `::`" IS
  the first character's case, so the two agree by construction, and this port
  already had the single-stub splice; adopting the reference's probe-then-patch
  shape would have been a second parse per keystroke for the same answer.
- **`CompletionItemKind`.** The reference labels every child `Class` with an
  explicit "slice-7 follow-up may distinguish Module / Constant" note. The
  qualified registry already carries `is_module`, so rigor-rs renders MODULE vs
  CLASS. Same SET, more accurate icon — this is the reference's own stated
  intent, not a behavioural divergence.
- **`sortText` / `insertText` / `filterText`.** The reference sets all three on
  every item. rigor-rs sets none on METHOD items today, and the two kinds never
  appear in the same response, so `sortText`'s `0_`/`1_` ranking has nothing to
  order. Left alone rather than half-ported; `detail` (the child's FQN) IS set,
  matching the reference.
- **Aliases stay public.** An RBS `Members::Alias` carries no visibility field
  in the C parser's AST, so `alias initialize_copy replace` inside a `private`
  section is recorded public. Conservative for an advisory list — it can offer
  one extra name, never hide a real one. In practice the vendored core puts its
  only such alias (`String#initialize_copy`) OUTSIDE a private section, so the
  observable sets agree with the reference anyway.
- **Attributes are not visibility-filtered** because `collect_members` does not
  harvest `attr_reader` / `attr_writer` at all — a pre-existing index gap, not
  touched here.

## The one place the filter deliberately stops

`private_methods` is read ONLY by `instance_method_names`, i.e. only by
completion. The DIAGNOSTIC predicates (`class_has_method`,
`class_has_singleton_method`, …) still see private methods as present, and must:
a private method is genuinely defined, and `send(:foo)` / an implicit-self call
both dispatch to it. Witnessing its absence would be a false positive. This is
why the sweep is expected to be byte-unchanged — and it is (below).

The singleton surface gets the filter for free: `singleton_method_names` folds
in `instance_method_names` of the `extend`ed modules and of
`Class`/`Module`/`Object`/`Kernel`/`BasicObject`, which is exactly where
`Module`'s private reflection methods live.

## Gates

- `cargo test --offline`: 359 + 236 + 180 + 72 + … **all green**; 11 new tests
  (5 index-level, 6 LSP-level), each proven non-vacuous by re-breaking the
  code once — the filter removed, `Node::Private` inverted, the per-`def`
  visibility ignored, the `::` routing disabled, the routing widened to
  lowercase, the grandchild filter removed, the intersection replaced by the
  first arm. Every break failed exactly the tests that claim that behaviour.
- Clippy `--workspace --all-targets -D warnings` in a FRESH `CARGO_TARGET_DIR`:
  clean.
- `ruby harness/run.rb` and `ruby harness/run_snapshot.rb`: 76 fixtures,
  232 matched / 236 reference, 3 gaps, **0 unregistered FPs** — the pre-change
  numbers exactly.
- `python3 harness/fp_audit.py --gaps --sweep`: **0 FP across 9204 files**, and
  the per-corpus gap counts reproduce `harness/CORPUS.md`'s baseline
  entry-for-entry. This is the load-bearing check that the visibility work did
  not leak out of completion into the check pipeline.
- `python3 harness/docs_check.py`: PASS.

## Left open

- **`rootUri`** (the fourth `LSP v4+` item) is untouched, as are temp-file
  `BufferBinding`, incremental UTF-16 `didChange` sync, `--log` wiring, and
  TCP/socket transport.
- **`Type::Intersection`** — the reference UNIONs the members' methods for an
  intersection receiver. rigor-rs's type model has no `Intersection` carrier to
  hang it on, so there is nothing to port.
- **Buffer-local constants after `::`.** Both tools ignore them today. Closing
  it means giving completion the cross-file overlay S4b deliberately withheld
  from the query handlers; it is a cost decision, not a correctness one.
