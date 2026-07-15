# Binding spec — `flow.duplicate-hash-key`, `flow.return-in-ensure`, `suppression.unknown-rule`/`suppression.empty` (v0.3.0 RC)

Oracle: reference `47ec8625` (v0.3.0 RC). Produced by a Sonnet investigation
(source read + live oracle probes, 2026-07-16). Every probe below was run
against the RC with fresh cache state; (line, col) are 1-based as emitted.

---

## 1. `flow.duplicate-hash-key`

**Reference source:** `lib/rigor/analysis/check_rules/duplicate_hash_key_collector.rb`
+ `check_rules.rb:306-317,1826-1839` (builder) + `rule_catalog.rb:464-489`.

### Semantics

- Walks every `Prism::HashNode` (braced) AND `Prism::KeywordHashNode` (bare
  keyword args, `m(a: 1, a: 2)`) independently — nested hash literals are their
  own scope, never cross-compared.
- Per literal, `seen: {tag => first_key_node}`; iterate elements in source order:
  - `AssocSplatNode` (`**h`) skipped WITHOUT resetting `seen` — neither creates
    nor rescues a collision.
  - Non-value-pinned key (call, local, constant, interpolated string/symbol) —
    skipped entirely, never enters `seen`, never fires.
  - Value-pinned tags: `[:symbol, unescaped]`, `[:string, unescaped]`,
    `[:integer, i64]`, `[:float, f64]`, bare `:true/:false/:nil` tags. Kind
    partitions (`:a` ≠ `"a"`, `1` ≠ `1.0`) — Ruby `Hash#eql?` semantics.
  - On collision: diagnostic at the REPEAT node; `seen` is NOT updated on the
    colliding branch ⇒ with N≥2 duplicates every later occurrence references
    the SAME original first occurrence (not the previous one).
- "first set at line N" uses the first key node's start_line (line only).
- `key_label` rendering:
  - Symbol: canonical `:name` — even for `key:` shorthand or `:key =>` source.
  - String: `unescaped.inspect` (Ruby String#inspect).
  - Integer/Float/true/false/nil: **raw source slice of the repeat node,
    verbatim** (NOT re-rendered) — `{ 1.0 => x, 1.00 => y }` renders `` `1.00' ``.

### Diagnostic

- severity `:warning` (profiles: lenient info / balanced warning / strict error),
  evidence_tier high, since 0.3.0.
- message: `` duplicate hash key `<key_label>' in the same literal; this entry overwrites the value first set at line <first_line> ``
  (backtick-quote style `` `x' ``).
- anchor: repeat key node start (line, start_column+1).
- doc URL slug `#rule-flow-duplicate-hash-key`.

### Probe matrix (all verified)

| source | result |
|---|---|
| `h = { a: 1, a: 2 }` | (1,13) label `` `:a' `` first-set line 1 |
| `{ "a" => 1, "a" => 2 }` | fires, label `` `"a"' `` |
| `{ 1 => "x", 1 => "y" }` | fires, label `` `1' `` |
| `{ 1.0 => "x", 1.0 => "y" }` | fires, label `` `1.0' `` |
| `{ 1 => "x", 1.0 => "y" }` | SILENT (int ≠ float kind) |
| `{ nil => 1, nil => 2 }` / `{ true => 1, true => 2 }` | fire, labels `` `nil' `` / `` `true' `` |
| `{ "a" => 1, a: 2 }` | SILENT (string ≠ symbol) |
| `{ foo => 1, foo => 2 }` | SILENT (computed key) |
| `{ **other, a: 1, a: 2 }` | fires (splat inert) |
| `{ "#{x}" => 1, "#{x}" => 2 }` | SILENT (interpolated) |
| `m(a: 1, a: 2)` bare kwargs | FIRES (KeywordHashNode scanned) |
| `{ a: 1, b: { a: 2, a: 3 } }` | only the NESTED pair fires; outer `a:` not compared |
| multi-line `{ a:1, \n b:2, \n a:3 }` | fires at the line of the repeat, "first set at" the first key's line |
| `{ a: 1, a: 2, a: 3 }` | fires TWICE, both "first set at" the ORIGINAL first occurrence |
| `{ 1.0 => "x", 1.00 => "y" }` | FIRES (same f64), label `` `1.00' `` (verbatim slice) |

### rigor-rs attachment points

- `Node::HashLit { elements, all_assoc, span }` lowering flattens splats to ONE
  slot and assocs to TWO ⇒ `elements[2i]`-as-key parity breaks once a splat is
  present. **Lowering change needed**: per-element kind (e.g. parallel
  `Vec<HashElement>` `Assoc{key,value} | Splat(id)` or an is_assoc tag) — check
  interaction with `Typer::hash_shape_or_hash`, which indexes the flat list.
- Key node variants (`SymbolLit`/`StringLit`/`IntegerLit`/`FloatLit`/`NilLit`/
  `TrueLit`/`FalseLit`/`Other`) are preserved — kind-tag logic maps directly.
- Int/Float labels need the RAW SOURCE SLICE via the key node's `Span` (values
  alone can't render `1.00`).
- Wiring: `crates/rigor-rules/src/lib.rs` — new const, `catalog()`,
  `IMPLEMENTED_RULES`/`ALL_CANONICAL_RULES`/`legacy_alias`
  (`"duplicate-hash-key"`); walk analogous to `dead_assignments_in_def`;
  `explain.rs` entry. `RULE_FAMILIES` already has `flow`.

---

## 2. `flow.return-in-ensure`

**Reference source:** `check_rules/return_in_ensure_collector.rb` +
`check_rules.rb:318-327,1841-1850` + `rule_catalog.rb:383-406`.

### Semantics

- Dispatches on every `Prism::BeginNode` with an `ensure_clause`; recursively
  scans the ensure statements (`gather_returns`):
  - bare `ReturnNode` anywhere in the subtree → fires (one diagnostic per
    return; no reachability suppression — two returns = two diagnostics).
  - Descent STOPS at: nested `DefNode`, `LambdaNode` (`-> {}`), nested
    `EnsureNode` (that inner ensure is handled when its own BeginNode is
    visited — no double count).
  - Receiver-less `CallNode` named `lambda`/`define_method` WITH a block:
    args/receiver still descended, block skipped.
  - `proc { return }` is deliberately NOT a barrier (returns from the enclosing
    method) — fires.
  - Plain blocks (`[1].each { return }`) — fire.
- Works at toplevel `begin/ensure` too (no enclosing def needed).
- Anchor: the `return` keyword span start (Prism `keyword_loc`; note
  `ReturnNode.location()` starts at the same offset ⇒ rigor-rs `Node::Return`
  span.start already matches for (line,col)).

### Diagnostic

- severity `:warning` (lenient info / balanced warning / strict error),
  evidence_tier high, since 0.3.0.
- message (STATIC): `` `return' inside `ensure' discards the method's in-flight return value and swallows any in-flight exception ``
- doc slug `#rule-flow-return-in-ensure`.

### Probe matrix

| ensure body | result |
|---|---|
| `return 1` | FIRES |
| `[1].each { return }` | FIRES (block not a barrier) |
| `def nested; return 1; end` | silent |
| `l = lambda { return 1 }` | silent (call barrier) |
| `l = -> { return 1 }` | silent (LambdaNode barrier) |
| `define_method(:foo) { return 1 }` | silent |
| `p = proc { return 1 }` | FIRES (deliberate) |
| bare `return` | FIRES |
| `return 1; return 2` | fires TWICE |
| toplevel `begin ... ensure return end` | FIRES |
| nested begin/ensure inside outer ensure | fires ONCE at the inner return |

### rigor-rs attachment points — blockers

1. `Node::BeginRescue { body, span }` flattens protected/rescue/else/ensure
   statements into ONE vec — **needs an `ensure_body: Vec<NodeId>` field** (the
   lowering already calls `ensure_clause()` at ast.rs:~1040; just route it).
   Keep the shape forward-compatible with the fuller `RescueClause` structure
   the shadowed-rescue slice needs (see typed-rules spec).
2. **No `LambdaNode` lowering at all** — `-> {}` bodies fall into non-recursing
   `Node::Other`; a `return` inside `-> {}` is invisible to EVERY rule (general
   soundness gap). Needs a real lambda variant (or lowering into the existing
   block-carrier shape) so the barrier can be recognized while its surroundings
   stay visible.
3. `Call { receiver: None, method ∈ {lambda, define_method}, block_body }` —
   barrier logic ports directly, no parser change.
4. Nested `Node::Definition` barrier — simple match, no change.
5. Wiring as in rule 1.

---

## 3. `suppression.unknown-rule` + `suppression.empty`

**Reference source:** `check_rules.rb:431-556` + `rule_catalog.rb:649-696`.

### Grammar (verbatim)

```ruby
LINE_SUPPRESSION_PATTERN  = /#\s*rigor:disable(?!-file)\s+(?<rules>[\w.,\s-]+)/
FILE_SUPPRESSION_PATTERN  = /#\s*rigor:disable-file\s+(?<rules>[\w.,\s-]+)/
BARE_SUPPRESSION_MARKER   = /#\s*rigor:disable(?<file>-file)?(?![\w-])(?<rest>.*)/
```

Tried file → line → bare-marker. `rigor:disable-next-line` matches NOTHING
(no suppression, no surveillance). No `rigor:enable` exists. Prose mentions
(`` # this documents `# rigor:disable <rule>` ``) are rejected (token charset +
the bare-marker `rest ~ /\A[\s,]*\z/` guard).

### Known-token resolution (`known_suppression_token?`)

Token is known iff: `"all"` | canonical id (full `ALL_RULES`, 25 at RC — NOT
just implemented ones) | legacy alias | family wildcard
(`call flow assert dump def suppression`) | bare non-check id
(`configuration-error load-error pool-degraded runtime-error
source-rbs-synthesis-failed`) | dotted id whose family ∈
(`rbs_extended dynamic rbs pre-eval plugin`) — `plugin.*` is ALWAYS known.

### Anchor + cardinality

Column = the comment's `#` (start_column+1), NOT the token. One diagnostic per
unknown token; multiple unknowns in one comment share the identical (line,col).

### Messages (note: DIFFERENT quote style from the flow rules — straight
backticks, copy verbatim)

- unknown-rule: `` unknown rule `<token>` in `# <marker>` — the token matches no known rule, alias, or family, so this suppression has no effect. Likely a typo; `rigor explain <rule>` lists the canonical ids. ``
  (marker = `rigor:disable` or `rigor:disable-file`; em-dash U+2014)
- empty: `` `# <marker>` lists no rules, so this suppression has no effect. Name the rules to suppress (`# <marker> call.undefined-method`) or use `# <marker> all`. ``
- Both severity `:warning` (ALL profiles warning), evidence_tier high, since
  0.3.0. `suppression.empty` fires when the token regex matched but yielded
  zero tokens OR the bare marker matched with only whitespace/commas after.

### Self-suppressibility

Generated BEFORE `filter_suppressed`, flow through it like any diagnostic —
`# rigor:disable call.bogus suppression.unknown-rule` suppresses its own
complaint (probe-verified). Ordering in the port must preserve this.

### Probe matrix

| comment | result |
|---|---|
| `# rigor:disable call.no-such-rule` | unknown-rule @ the `#` col |
| `# rigor:disable` | empty (marker `rigor:disable`) |
| `# rigor:disable-file` | empty (marker `rigor:disable-file`) |
| `# rigor:disable call.undefined-method,call.bogus-one, call.bogus-two` | TWO unknown-rule, same (line,col) |
| `# rigor:disable call` | family — known, suppresses call.* |
| `# rigor:disable all` | known |
| `# rigor:disable undefined-method` | legacy alias — known |
| `# rigor:disable rbs_extended.something` | known (non-check family) |
| `# rigor:disable-next-line call.undefined-method` | ignored entirely (no diag, no suppression) |
| prose mention | ignored |
| self-suppression combos | 0 diagnostics (verified) |

### rigor-rs attachment points

1. `rigor_parse::comment_lines` returns `(line, text)` — **add the column**
   (offset − line_start + 1; the data is already at hand in the function).
2. rigor-rs has no bare-marker branch (`suppression.empty` path) and no
   unknown-token surveillance — both new, near `parse_suppression_comments` /
   `filter_suppressed` in `crates/rigor-rules/src/lib.rs:1614-1795`. Current
   `match_directive` already rejects `disable-next-line` — matches reference.
3. Build `known_suppression_token?`: add `"suppression"` to `RULE_FAMILIES`
   (lib.rs:1518); add `NON_CHECK_DIAGNOSTIC_IDS`/`_FAMILIES` tables; validate
   against the FULL `ALL_CANONICAL_RULES` (grow it with the five new v0.3.0
   ids — including `flow.shadowed-rescue-clause` even before it's implemented,
   so it's a known token and `is_inert_builtin_token` handles it correctly).
4. Produce the new diagnostics BEFORE `filter_suppressed` in the same list
   (self-suppression), from the check assembly path in rigor-cli.
