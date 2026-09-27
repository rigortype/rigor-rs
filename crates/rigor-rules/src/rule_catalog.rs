//! Every rule id and its catalogue entry (severity, evidence tier,
//! documentation URL).

use crate::Severity;

// ---------------------------------------------------------------------------
// Rule catalogue
// ---------------------------------------------------------------------------

/// Per-rule static properties that enrich the JSON output stream but are NOT
/// carried on the `Diagnostic` object itself (ADR-0030 / reference ADR-65).
pub struct RuleEntry {
    pub default_severity: Severity,
    /// Confidence tier for consumers routing attention: `"high"` | `"medium"` |
    /// `"low"`. Omitted (None) for informational / plugin rules.
    pub evidence_tier: &'static str,
    /// Stable per-rule documentation URL.
    pub documentation_url: &'static str,
}

/// Static catalogue of the three rules implemented in this slice.
///
/// `catalog(rule_id)` returns the entry for a known rule, `None` for unknown.
pub fn catalog(rule_id: &str) -> Option<&'static RuleEntry> {
    match rule_id {
        CALL_UNDEFINED_METHOD => Some(&RuleEntry {
            default_severity: Severity::Error,
            evidence_tier: "high",
            documentation_url: "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-call-undefined-method",
        }),
        CALL_WRONG_ARITY => Some(&RuleEntry {
            default_severity: Severity::Error,
            evidence_tier: "high",
            documentation_url: "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-call-wrong-arity",
        }),
        CALL_POSSIBLE_NIL_RECEIVER => Some(&RuleEntry {
            // `error` under the default `balanced` profile (reference
            // severity_profile.rb), matching the sibling call.* rules whose
            // catalog default mirrors their balanced severity. An FP here would
            // be an ERROR on guarded code — hence the zero-FP decline scan.
            default_severity: Severity::Error,
            evidence_tier: "medium",
            documentation_url: "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-call-possible-nil-receiver",
        }),
        CALL_UNRESOLVED_TOPLEVEL => Some(&RuleEntry {
            // Authored `:warning` (balanced), `:off` in lenient. Evidence tier
            // `low`: a firing is frequently a resolution gap (the defining file
            // is outside the analyzed set, or the method is metaprogrammed) that
            // routes to the `pre_eval:` review path, not a definite typo.
            default_severity: Severity::Warning,
            evidence_tier: "low",
            documentation_url: "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-call-unresolved-toplevel",
        }),
        FLOW_DEAD_ASSIGNMENT => Some(&RuleEntry {
            default_severity: Severity::Warning,
            evidence_tier: "medium",
            documentation_url: "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-flow-dead-assignment",
        }),
        DEF_OVERRIDE_VISIBILITY_REDUCED => Some(&RuleEntry {
            default_severity: Severity::Warning,
            // The oracle stamps this rule `high` (a purely structural Liskov
            // signature check over the project ancestor chain); mirror exactly.
            evidence_tier: "high",
            documentation_url: "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-def-override-visibility-reduced",
        }),
        FLOW_UNREACHABLE_BRANCH => Some(&RuleEntry {
            default_severity: Severity::Warning,
            // The oracle stamps this `high` (a purely SYNTACTIC literal-predicate
            // check — no typer, no folding); mirror exactly.
            evidence_tier: "high",
            documentation_url: "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-flow-unreachable-branch",
        }),
        FLOW_ALWAYS_RAISES => Some(&RuleEntry {
            // `error` — a provable `ZeroDivisionError` (the oracle stamps it
            // error / high). An FP here would be an ERROR on correct code, so the
            // decline gate in `check_always_raises` is intentionally strict.
            default_severity: Severity::Error,
            evidence_tier: "high",
            documentation_url: "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-flow-always-raises",
        }),
        FLOW_ALWAYS_TRUTHY_CONDITION => Some(&RuleEntry {
            // The oracle stamps this `warning` / medium (an inferred-constant
            // predicate; the inferred counterpart to the high-evidence syntactic
            // `unreachable-branch`).
            default_severity: Severity::Warning,
            evidence_tier: "medium",
            documentation_url: "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-flow-always-truthy-condition",
        }),
        FLOW_DUPLICATE_HASH_KEY => Some(&RuleEntry {
            // Oracle: warning (balanced) / high — a purely syntactic value-pinned
            // comparison with no metaprogramming escape (Ruby itself warns under `-w`).
            default_severity: Severity::Warning,
            evidence_tier: "high",
            documentation_url: "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-flow-duplicate-hash-key",
        }),
        FLOW_RETURN_IN_ENSURE => Some(&RuleEntry {
            // Oracle: warning (balanced) / high — a syntactic proof with a
            // frame-aware envelope; Ruby's `ensure` semantics make every firing real.
            default_severity: Severity::Warning,
            evidence_tier: "high",
            documentation_url: "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-flow-return-in-ensure",
        }),
        CALL_ARGUMENT_TYPE_MISMATCH => Some(&RuleEntry {
            // Oracle: error across all profiles / high — a positional argument
            // whose statically-inferred type the RBS parameter provably rejects,
            // gated behind a zero-FP envelope (concrete + RBS-known receiver,
            // plain-positional-only, universal-equality skip, coerce-operator
            // skip on the multi-overload non-nil channel, faithful-param gate on
            // the single-overload non-nil channel).
            default_severity: Severity::Error,
            evidence_tier: "high",
            documentation_url: "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-call-argument-type-mismatch",
        }),
        CALL_RAISE_NON_EXCEPTION => Some(&RuleEntry {
            // Oracle: error across all profiles / high — the operand's
            // statically-inferred type is provably not a legal `raise` operand,
            // gated behind the same zero-FP envelope (project-class bail, module
            // bail, duck `#exception`, redefinition, unknown-type decline).
            default_severity: Severity::Error,
            evidence_tier: "high",
            documentation_url: "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-call-raise-non-exception",
        }),
        FLOW_SHADOWED_RESCUE_CLAUSE => Some(&RuleEntry {
            // Oracle: warning (balanced) / high — a purely syntactic + class-
            // hierarchy proof with a strict ancestry-certainty envelope (opaque
            // clauses, module bail, project-superclass gate), no metaprogramming
            // escape. Lenient info / strict error via the profile.
            default_severity: Severity::Warning,
            evidence_tier: "high",
            documentation_url: "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-flow-shadowed-rescue-clause",
        }),
        SUPPRESSION_UNKNOWN_RULE => Some(&RuleEntry {
            // Oracle: warning across ALL profiles / high — pure token-table
            // membership over the same tables the suppression matcher uses.
            default_severity: Severity::Warning,
            evidence_tier: "high",
            documentation_url: "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-suppression-unknown-rule",
        }),
        SUPPRESSION_EMPTY => Some(&RuleEntry {
            // Oracle: warning across ALL profiles / high — the marker word is
            // present and the token list is provably empty.
            default_severity: Severity::Warning,
            evidence_tier: "high",
            documentation_url: "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-suppression-empty",
        }),
        STATIC_VALUE_USE_VOID => Some(&RuleEntry {
            // Oracle: authored :warning, resolved :off by every shipped profile
            // (ADR-50 WD1) — emitted only under the `use-of-void-value`
            // bleeding-edge feature, where it is :warning. rigor-rs gates the
            // COLLECTOR on the feature, so the catalog carries the active
            // severity. Evidence high: only an author-written `-> void` on the
            // direct-dispatch path enters the table.
            default_severity: Severity::Warning,
            evidence_tier: "high",
            documentation_url: "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-static-value-use-void",
        }),
        SUPPRESSION_UNKNOWN_MARKER => Some(&RuleEntry {
            // Oracle: warning across ALL profiles / high — the marker word is
            // present and provably outside the suppression grammar; the prose
            // escape is excluded before firing.
            default_severity: Severity::Warning,
            evidence_tier: "high",
            documentation_url: "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-suppression-unknown-marker",
        }),
        DEF_IVAR_WRITE_MISMATCH => Some(&RuleEntry {
            // Authored `:error`; balanced profile stamps it `:warning` (lenient
            // warning, strict error). rigor-rs emits the balanced-default severity
            // directly, so the catalog default is `warning` — matching the oracle's
            // default text output. Evidence tier `high` (concrete static class of
            // each write, no metaprogramming escape).
            default_severity: Severity::Warning,
            evidence_tier: "high",
            documentation_url: "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-def-ivar-write-mismatch",
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Rule IDs
// ---------------------------------------------------------------------------

/// The stable id of the headline tracer-bullet rule (ADR-0030 taxonomy).
pub const CALL_UNDEFINED_METHOD: &str = "call.undefined-method";

/// `call.wrong-arity`: a call passes a positional-argument count outside the
/// method's known arity envelope (ADR-0030 taxonomy).
pub const CALL_WRONG_ARITY: &str = "call.wrong-arity";

/// `call.argument-type-mismatch`: a call passes a positional argument whose
/// statically-inferred type the matching RBS parameter provably rejects (ADR-64
/// / ADR-0030 taxonomy). Fired INDEPENDENTLY of the arity/undefined chain (the
/// reference emits it alongside `call.wrong-arity` at the same site). Two
/// channels, both zero-FP-gated: a `nil` argument a param that rejects nil, and
/// a non-nil argument whose concrete class the param rejects. See
/// [`check_argument_type_mismatch`].
///
/// [`check_argument_type_mismatch`]: crate::check_argument_type_mismatch
pub const CALL_ARGUMENT_TYPE_MISMATCH: &str = "call.argument-type-mismatch";

/// `call.possible-nil-receiver`: a call whose receiver may be nil on some path
/// (ADR-0030 taxonomy). In this slice only the union case is in scope; the
/// literal-`nil` case is owned by `call.undefined-method` (matching the
/// reference, which routes `nil.foo` to undefined-method).
pub const CALL_POSSIBLE_NIL_RECEIVER: &str = "call.possible-nil-receiver";

/// `call.unresolved-toplevel` (ref ADR-34): an implicit-self call (no explicit
/// receiver) at TOPLEVEL scope — outside any `def`/`class`/`module` body — whose
/// method name resolves against NONE of: a toplevel `def` in the same file, the
/// `Object`/`Kernel` instance surface (`puts`/`require`/`raise`/`loop`/… all
/// declared `def self?.x` in the core RBS, so recorded as instance methods), or
/// an ADR-17 `pre_eval:` monkey-patch. Deliberately does NOT fire on implicit-self
/// calls inside `def`/`class`/`module` bodies (ADR-24 leniency stays there).
pub const CALL_UNRESOLVED_TOPLEVEL: &str = "call.unresolved-toplevel";

/// `flow.dead-assignment`: a local assigned in a method body but never read in
/// that body (ADR-0030 taxonomy). The FIRST `flow.*` rule — a pure AST/structural
/// check (no flow-sensitive scopes, no typer/folding), mirroring the reference's
/// `DeadAssignmentCollector` exactly.
pub const FLOW_DEAD_ASSIGNMENT: &str = "flow.dead-assignment";

/// `def.override-visibility-reduced` (ADR-35 slice 1): an instance-method
/// override whose visibility is STRICTLY MORE RESTRICTIVE than the nearest
/// project-source ancestor method it overrides (public→protected/private or
/// protected→private), breaking substitutability. A purely STRUCTURAL def-family
/// check (no typer, no flow scopes, no unions): the override visibility is read
/// from the source-discovered table and the parent is resolved over the
/// project-source ancestor chain (RBS / third-party ancestors are a deferred
/// follow-on). Mirrors the reference's `override_visibility_diagnostic` exactly.
pub const DEF_OVERRIDE_VISIBILITY_REDUCED: &str = "def.override-visibility-reduced";

/// `flow.always-raises`: an Integer division/modulo by a constant-zero divisor —
/// a provable `ZeroDivisionError` (ADR-0030 taxonomy). Fires iff the receiver is
/// provably Integer-rooted (`Constant[Integer]` / `IntegerRange` /
/// `Nominal[Integer]`), the method is one of `/ % div modulo divmod`, and the
/// single positional argument types to a constant Integer `0`. Float division by
/// zero is `Infinity`, NOT an error, so a Float receiver or a `0.0` divisor is
/// DECLINED — mirroring the reference's `integer_zero_division?` exactly. This is
/// an error-severity rule, so the gate declines on any uncertainty (zero-FP).
pub const FLOW_ALWAYS_RAISES: &str = "flow.always-raises";

/// `flow.unreachable-branch`: an `if`/`unless` (including ternary, which Prism
/// also parses as an `IfNode`) whose predicate is a SYNTACTIC LITERAL that is
/// always truthy or always falsey, making the opposite branch dead — fired only
/// when that dead branch is NON-EMPTY. A purely STRUCTURAL/AST check: it matches
/// LITERAL NODES (`true`/`false`/`nil`/Integer/Float/String/Symbol), never the
/// constant folder — a variable/constant predicate that *would* fold to a literal
/// must NOT flag (the reference uses syntactic detection). Mirrors the reference's
/// `unreachable_branch_diagnostic` + `literal_predicate_polarity` exactly.
///
/// KEYWORD INVERSION (the correctness keystone): for `if`, truthy ⇒ ELSE dead,
/// falsey ⇒ THEN dead; for `unless` the two INVERT (truthy ⇒ THEN dead, falsey ⇒
/// ELSE dead). The lowered `Node::If` collapses both keywords, so the dead-branch
/// selection reads `is_unless` — anchoring on the wrong branch would land the
/// diagnostic on LIVE code (a parity-key mismatch = an effective false positive).
///
/// In practice this fires ~0 times on the real corpus (literal-predicate
/// conditionals are vanishingly rare in production); that is ACCEPTED — the value
/// is a complete, correct rule plus the `is_unless` AST-correctness fix.
pub const FLOW_UNREACHABLE_BRANCH: &str = "flow.unreachable-branch";

/// `flow.always-truthy-condition`: an `if`/`unless`/ternary predicate whose
/// INFERRED type folds to a `Type::Constant` under the dominating flow scope —
/// the inferred-constant counterpart to the syntactic-literal `unreachable-branch`
/// (ADR-0022 first flow slice). Fired only when the predicate is NOT a syntactic
/// literal (owned by `unreachable-branch`), NOT a defensive predicate call
/// (`nil?`/`empty?`/`zero?`/`any?`/`none?`/`all?`/`respond_to?` — the user reading
/// like an explicit runtime check the types disagree with), and NOT lexically
/// inside a loop / block (incomplete loop-mutation modelling makes an in-loop
/// constant suspect). Mirrors the reference's `AlwaysTruthyConditionCollector`
/// skip envelope; the folded type comes from
/// [`rigor_infer::Typer::always_truthy_snapshots`], a strict UNDER-approximation
/// of the reference flow folder, so a surviving constant is zero-FP.
///
/// Like `unreachable-branch`, fires ~0 times on the real corpus (inferred-constant
/// predicates are vanishingly rare in production); ACCEPTED — the value is a
/// complete, correct `flow.*` rule plus the reusable flow-constant substrate it
/// is the first consumer of.
pub const FLOW_ALWAYS_TRUTHY_CONDITION: &str = "flow.always-truthy-condition";

/// `flow.duplicate-hash-key` (v0.3.0): two entries of one Hash literal (braced or
/// bare keyword args) carry the same value-pinned literal key — Ruby keeps the
/// LAST entry silently at runtime, so the earlier value is dead. Purely syntactic
/// (the [`rigor_parse::HashKey`] envelope: symbols / plain strings / integers /
/// floats / `true` / `false` / `nil`, never cross-kind, never interpolation /
/// constants / calls / splats). Mirrors the reference's `DuplicateHashKeyCollector`.
pub const FLOW_DUPLICATE_HASH_KEY: &str = "flow.duplicate-hash-key";

/// `flow.return-in-ensure` (v0.3.0): an explicit `return` lexically inside an
/// `ensure` clause body — it silently discards the method's in-flight return
/// value AND swallows any in-flight exception. Purely syntactic with a frame-aware
/// envelope (nested `def` / lambda / `define_method` blocks are barriers; plain
/// blocks and `proc { }` are not). Mirrors the reference's `ReturnInEnsureCollector`.
pub const FLOW_RETURN_IN_ENSURE: &str = "flow.return-in-ensure";

/// `suppression.unknown-rule` (v0.3.0): a `# rigor:disable[-file]` marker names a
/// token that resolves to no known rule id, alias, family, or engine diagnostic —
/// the suppression silently no-ops (usually a typo). Surveillance over the markers
/// themselves; produced BEFORE `filter_suppressed`, so it is itself suppressible.
pub const SUPPRESSION_UNKNOWN_RULE: &str = "suppression.unknown-rule";

/// `suppression.empty` (v0.3.0): a bare `# rigor:disable[-file]` marker with no
/// rule tokens (only whitespace/commas after it) — it suppresses nothing.
pub const SUPPRESSION_EMPTY: &str = "suppression.empty";

/// `suppression.unknown-marker` (v0.3.0): a `rigor:`-prefixed marker word OUTSIDE
/// Rigor's suppression grammar but reading like an attempted suppression — the
/// RuboCop-reflex `# rigor:disable-next-line <rule>` / `# rigor:enable <rule>`.
/// Such a marker is invisible to the two real patterns (`# rigor:disable[-file]`),
/// so it silently suppresses nothing. Surveillance over the markers themselves,
/// produced BEFORE `filter_suppressed`, so it is itself suppressible.
pub const SUPPRESSION_UNKNOWN_MARKER: &str = "suppression.unknown-marker";

/// `static.value-use.void` (ADR-100, v0.3.0 RC): a value recovered from an
/// author-declared `-> void` return, used in VALUE context (an assignment RHS,
/// a call receiver, or a positional argument). An explicit `-> void` is the
/// strongest "do not rely on this return" signal an author can give. Authored
/// `:warning` but resolved `:off` by every shipped profile — it reaches a user
/// only through the `use-of-void-value` bleeding-edge feature (ADR-50 WD1), so
/// the CLI runs [`void_value_use_diagnostics`] only when that feature is
/// active (the observable equivalent of the reference's severity gate).
///
/// [`void_value_use_diagnostics`]: crate::void_value_use_diagnostics
pub const STATIC_VALUE_USE_VOID: &str = "static.value-use.void";

/// `def.ivar-write-mismatch` (since 0.1.2): within one class's instance methods,
/// the same instance variable `@x` is assigned two DIFFERENT concrete classes —
/// a likely type-confusion bug (`@x = "s"` then `@x = 42`). A faithful port of
/// the reference `IvarWriteCollector` + `ivar_mismatch_diagnostics_for`.
///
/// Collector: over every ClassDef/ModuleDef reachable through class/module bodies,
/// each DIRECT instance `def`'s body is scanned for plain `@x = value` writes
/// (barriers at nested def/class/module; singleton `def self.x` bodies skipped;
/// op-writes `@x ||=`/`@x +=` and `self.x=` are NOT ivar writes and never
/// collected), grouped by (qualified class name, ivar name). The class of a write
/// is `CoreIndex::class_name_of` of its rvalue type with `TrueClass`/`FalseClass`
/// folded to `"bool"` (the boolean-flag idiom `@on = false; @on = true` stays
/// silent).
///
/// Firing (per group of ≥2 writes): the CANONICAL class is the first write whose
/// class is not `NilClass` (leading `@x = nil` placeholders are skipped); if that
/// canonical write's class is unresolvable (Dynamic / union / a non-core Nominal),
/// the WHOLE group is silent. Every LATER write fires iff its class resolves, is
/// not `NilClass` (the clear-to-nil idiom is always silent), and differs from the
/// canonical class. Anchored on the offending write's `@x` name token.
///
/// Two increments feed the two confirmed corpus gaps: (a) a `rescue C => e` /
/// bare `rescue => e` binds `e` to the (single, resolvable) exception class within
/// the clause body, so `@e = "s"; rescue StandardError => e; @e = e` flags
/// String→StandardError; (b) `Integer()`/`Float()`/`String()` on a non-constant
/// argument types NOMINALLY to the conversion class (increment lives in the typer).
pub const DEF_IVAR_WRITE_MISMATCH: &str = "def.ivar-write-mismatch";

/// `call.raise-non-exception` (v0.3.0): an implicit-self `raise` / `fail` whose
/// first positional argument's statically-inferred type is provably NOT a legal
/// raise operand — an Exception class object, an Exception instance, a String
/// (raises RuntimeError), or any object whose class defines `#exception` (the
/// duck protocol `raise` consults at runtime). Anything else (`raise 42`,
/// `raise :sym`, `raise nil`, `raise Array`) raises TypeError at runtime. A
/// faithful port of the reference's `raise_non_exception_diagnostic` +
/// `raise_operand_verdict` (`check_rules.rb`).
///
/// Zero-FP envelope (each gate load-bearing): implicit-self only; `raise`/`fail`
/// not redefined reachably (toplevel def, Object/Kernel reopen, enclosing-class
/// instance or singleton def); no block; a plain first positional arg
/// (splat/kwargs/forwarding bail); a trinary verdict that fires ONLY on a
/// provable `:illegal` (unknown / Dynamic / mixed union stay silent); ANY
/// project-discovered class bails; the instance path bails on the generic
/// carriers (`Class`/`Module`/`Object`/`BasicObject`) and on module-typed values
/// and treats `:superclass` as unknown (asymmetric with the exact singleton path,
/// where `:superclass` fires).
pub const CALL_RAISE_NON_EXCEPTION: &str = "call.raise-non-exception";

/// `flow.shadowed-rescue-clause` (v0.3.0): a `rescue` clause of a `begin`/`def`
/// rescue chain that can never run because an EARLIER clause of the SAME chain
/// already catches a superclass (or the same class) of every exception class the
/// later clause names (`rescue StandardError => e … rescue ArgumentError` — the
/// ArgumentError arm is dead). A faithful port of the reference
/// `ShadowedRescueCollector` (see [`shadowed_rescue`]). Purely syntactic + class
/// ancestry — no Typer.
///
/// Zero-FP envelope (each gate load-bearing): only ConstantRead/ConstantPath
/// exception designators certify; a clause with any splat / local / call
/// designator is fully opaque (never covers, never fires). Modules NEVER certify;
/// a project class certifies ONLY with a discovered `class Foo < Bar` superclass;
/// a later clause naming a superclass of an earlier one (narrow→wide) stays
/// silent; comparisons never cross a nested `begin`.
///
/// [`shadowed_rescue`]: crate::shadowed_rescue
pub const FLOW_SHADOWED_RESCUE_CLAUSE: &str = "flow.shadowed-rescue-clause";
