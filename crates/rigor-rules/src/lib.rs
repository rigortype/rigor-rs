//! Diagnostic rules + the structured `Diagnostic` type (ADR-0014: rule id,
//! severity, primary/secondary annotations, subdiagnostics). All rules run in a
//! single converged AST walk (ADR-0005), not one pass per rule. The tracer
//! bullet's first rule is `call.undefined-method`.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use rigor_index::{CoreIndex, OverloadSignature, RetainedParamType};
use rigor_infer::Typer;
use rigor_parse::{HashKeyTag, LoweredAst, Node, NodeId};
use rigor_types::{Interner, Scalar, Type};

mod shadowed_rescue;
pub use shadowed_rescue::shadowed_rescue_diagnostics;

pub mod dead_version_guard;
pub use dead_version_guard::{
    filter_dead_version_guard_arms, filter_dead_version_guard_arms_with, RubyRuntime,
};

// ---------------------------------------------------------------------------
// Severity enum
// ---------------------------------------------------------------------------

/// The three severity levels (ADR-0030). Matches the reference's
/// `:error` / `:warning` / `:info` atoms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    /// Render as the reference spells it in JSON/text output.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }
}

// ---------------------------------------------------------------------------
// Diagnostic struct
// ---------------------------------------------------------------------------

/// The `rule_id` of a **ruleless** diagnostic — one no rule produced.
///
/// The reference models this as `rule: nil` on its `Diagnostic` and reads it
/// back through `Diagnostic#qualified_rule`, which returns `nil`; the producers
/// it names are "parse errors, path errors, internal analyzer errors"
/// (`lib/rigor/analysis/diagnostic.rb`). rigor-rs keeps `rule_id: &'static str`
/// — over fifty call sites read it as a plain string (the `disable:` matcher,
/// the severity stamp, the baseline binner, the LSP `code`) and an `Option`
/// there would be a mechanical rewrite of all of them for one producer — and
/// spells the absent rule as this empty-string sentinel instead.
///
/// **Never format `rule_id` directly.** Go through
/// [`Diagnostic::qualified_rule`], which maps the sentinel back to `None`, so
/// that the decision "what does a ruleless diagnostic look like here" is taken
/// once per emitter and cannot silently render as `""`. A `""` in the JSON
/// `rule` field is a DIFFERENT `(rule, line, column)` key from the reference's
/// `nil` for `harness/lib.rb`'s `DiagKey`, so the row would count as both a
/// coverage gap and an unregistered extra.
pub const NO_RULE: &str = "";

/// A diagnostic finding, identified by `rule_id` + location (ADR-0002 parity
/// is defined over this pair).
///
/// `receiver_type` and `method_name` are omitted from the struct (None) for
/// rules that don't operate on a call dispatch subject.
///
/// # TODO(spec)
/// - `project_definition_site: Option<String>` — `"path:line"` for
///   `call.undefined-method` when the project defines the called method via a
///   monkey-patch or `pre_eval:`. Set by `call.undefined-method` once the
///   project-index layer is implemented (ADR-0017).
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub rule_id: &'static str,
    pub start_offset: usize,
    pub end_offset: usize,
    pub message: String,
    /// Authored severity before any profile re-stamping.
    pub severity: Severity,
    /// Identifies the rule source: `"builtin"` for all rules shipped with
    /// rigor-rs. Future values: `"plugin.<id>"`, `"rbs_extended"`,
    /// `"generated.<provider>"` (ADR-0030).
    ///
    /// # TODO(spec)
    /// Implement the full source_family set once plugins / RBS extensions land.
    pub source_family: &'static str,
    /// Rendered receiver class/type for call/def rules; `None` for other rules.
    pub receiver_type: Option<String>,
    /// Called / defined method name for call/def rules; `None` otherwise.
    pub method_name: Option<String>,
}

impl Diagnostic {
    /// The qualified rule identifier, or `None` for a ruleless diagnostic
    /// (see [`NO_RULE`]).
    ///
    /// Mirrors the reference's `Diagnostic#qualified_rule`. rigor-rs keeps the
    /// `builtin` family bare in `rule_id`, so for every rule-produced
    /// diagnostic this is `rule_id` unchanged; the whole job of the accessor is
    /// to give every emitter ONE place to decide what "no rule" renders as.
    pub fn qualified_rule(&self) -> Option<&'static str> {
        (self.rule_id != NO_RULE).then_some(self.rule_id)
    }
}

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

/// The Integer division/modulo operators that raise `ZeroDivisionError` on a
/// zero Integer divisor — verbatim the reference's `INTEGER_RAISING_OPERATORS`
/// (`%i[/ % div modulo divmod]`). The op set is closed: Float `/` returns
/// `Infinity` (no raise), and other methods are not modeled here.
const INTEGER_RAISING_OPERATORS: &[&str] = &["/", "%", "div", "modulo", "divmod"];

// ---------------------------------------------------------------------------
// analyze()
// ---------------------------------------------------------------------------

/// Analyze a lowered AST and return all diagnostics, in source order.
///
/// This is the single converged walk (ADR-0005): it builds the top-level type
/// environment once, then visits every node, applying every call rule
/// (`call.undefined-method`, `call.wrong-arity`, `call.possible-nil-receiver`)
/// in the SAME pass. At most one diagnostic is emitted per call site, matching
/// the reference's one-diagnostic-per-offending-call discipline.
pub fn analyze(ast: &LoweredAst, interner: &mut Interner, index: &CoreIndex) -> Vec<Diagnostic> {
    // Single-file API: build a per-file source index then delegate. Preserves the
    // existing signature + tests. The project pass (the CLI) builds ONE
    // project-wide source over all files and calls `analyze_with_source` directly.
    let source = rigor_infer::SourceIndex::build(ast, index);
    analyze_with_source(ast, interner, index, &source)
}

/// Analyze a lowered AST against an EXTERNALLY-built [`SourceIndex`] — the
/// project-wide variant the CLI builds once over every file. Splitting this out
/// lets the bare-constant singleton gate (`!source.knows_class(name)`) see class
/// names defined in OTHER files, so a project model referenced in a file that
/// does not define it (`Group.where(...)`) is never singleton-typed and stays
/// silent (the cross-file zero-FP keystone).
pub fn analyze_with_source(
    ast: &LoweredAst,
    interner: &mut Interner,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
) -> Vec<Diagnostic> {
    analyze_with_source_and_folder(ast, interner, index, source, None)
}

/// As [`analyze_with_source`], plus the optional ADR-0008 real-Ruby folder for
/// sidecar-routed constant folds (full-fidelity mode). `folder = None` is
/// byte-identical to [`analyze_with_source`] (the sound subset). The folder must
/// be `Sync` — one instance is shared across the file-parallel analysis.
pub fn analyze_with_source_and_folder(
    ast: &LoweredAst,
    interner: &mut Interner,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
    folder: Option<&(dyn rigor_infer::RubyFolder + Sync)>,
) -> Vec<Diagnostic> {
    // A typer over the real RBS index AND the (project-wide) source index, so
    // `X.new` types to an instance and a bare constant `X` types to its class
    // object (`Singleton(X)`) for class-method witnessing. The source index also
    // drives RETURN-TYPE inference for chaining. The folder (if wired) lets a
    // sidecar-foldable literal call the Rust core declined resolve to a `Constant`.
    // C1: attach the current file's lexical class/module scopes so the typer's
    // `ConstantRead` arm resolves each use site's lexical prefix (span
    // containment) and applies the precise constant-shadow gate.
    let scopes = rigor_infer::lexical_scopes(ast);
    let typer = Typer::with_source_and_folder(index, source, folder)
        .with_lexical_scopes(&scopes)
        .with_file_key(ast.file_key());
    let env = ScopedEnv::build(&typer, ast, interner);
    // ADR-0038 Slice 1: the per-call nil-receiver snapshot map (call node id ->
    // non-nil core arm), computed ONCE over the whole program via the threaded
    // flow-eval. `check_nil_receiver` fires from it (block / top-level scopes,
    // not only inside a named `def`).
    let nil_snaps = typer.nilable_receiver_snapshots(ast, interner);
    // Class-narrowing snapshot map (census mechanism 1): call node id -> class
    // name `C` the receiver local was `is_a?`/`case-when` narrowed to (from
    // `Dynamic`/`Top` only). `check_narrowed_call` fires `call.undefined-method`
    // from it — and ONLY that rule (spec pitfall 7: a narrowed receiver must not
    // become witnessable by wrong-arity/ATM in this slice).
    // …and, from the SAME pass, the disjoint-guard suppression set: call node
    // ids whose bare-local receiver the reference collapsed to `Bot`
    // (docs/notes/20260808-disjoint-guard-suppression.md). `Bot` has no dispatch
    // surface, so the reference emits NOTHING at those sites — measured across
    // `undefined-method`, `wrong-arity` and `argument-type-mismatch` — and the
    // whole call site is skipped rather than one rule.
    let narrowing = typer.class_narrowing_pass(ast, interner);
    let class_snaps = narrowing.calls;
    let dead_calls = narrowing.dead;
    // Collection-shape snapshot map (collection-shape slice, stage 1): call node
    // id -> "Array"/"Hash", the class a bare-local receiver's collection carrier
    // (literal seed, mutator-widened kept nominal, or tier-folded chain result)
    // dispatches on. `check_collection_call` fires `call.undefined-method` from
    // it — and ONLY that rule, same envelope as the class-narrowing map.
    let coll_snaps = typer.collection_shape_snapshots(ast, interner);
    let mut out = Vec::new();

    // Visit nodes in id order, which is source-discovery order, so diagnostics
    // come out deterministically (ADR-0020 determinism).
    let calls: Vec<_> = ast
        .iter()
        .filter_map(|(id, node)| match node {
            Node::Call {
                receiver: Some(recv),
                method,
                args,
                block_body,
                message_span,
                safe_nav,
                args_all_plain,
                args_plain_positional,
                ..
            } => Some((
                id,
                *recv,
                method.clone(),
                args.clone(),
                !block_body.is_empty(),
                *message_span,
                *safe_nav,
                *args_all_plain,
                *args_plain_positional,
            )),
            _ => None,
        })
        .collect();

    for (call_id, recv, method, args, has_block, message_span, safe_nav, args_all_plain, args_plain_positional) in calls {
        // DEAD RECEIVER (disjoint-guard suppression): the reference's guarded
        // scope binds this receiver to `Bot`, which responds to no method and
        // carries no signature, so no receiver-driven rule of the reference can
        // fire here — `undefined-method`, `wrong-arity` and
        // `argument-type-mismatch` are each measured silent inside the branch
        // while an unguarded control fires. Skipping the whole site (including
        // the independent argument-type axis below) is what the oracle shows;
        // calls whose receiver is a DIFFERENT local, and a call nested in this
        // one's ARGUMENTS, keep their own node ids and are untouched (probes
        // `scope_other_local`, `nest_arg_other`).
        if dead_calls.contains(&call_id) {
            continue;
        }
        // Rule precedence at one call site (avoid double-emit):
        //   1. undefined-method  (method absent on the receiver class, incl. nil)
        //   2. wrong-arity       (method present but arg count out of envelope)
        //   3. possible-nil-receiver (union receiver with a nil arm)
        // The reference emits exactly one of these per call; we mirror that by
        // returning the first that fires.
        // Ruby method bodies are independent local scopes, so a use site inside a
        // `def` never reads the file's top-level locals (`ScopedEnv::at`).
        let gate_env = env.gate_at(message_span);
        let env = env.at(message_span);
        // `nil&.m` never dispatches: the reference's `safe_navigation_receiver`
        // turns a receiver that is exactly nil into `bot` for undefined-method.
        // A `T | nil` union still flows through unchanged, as it does there.
        let nil_skip = safe_nav && {
            let recv_ty = typer.type_of(ast, recv, env, interner);
            arg_is_pure_nil(interner, index, typer.source(), recv_ty)
        };
        let diag = (!nil_skip)
            .then(|| {
                check_call(
                    ast, recv, &method, message_span, safe_nav, env, &typer, interner, index,
                )
            })
            .flatten()
            .or_else(|| {
                check_narrowed_call(
                    call_id, ast, recv, &method, message_span, safe_nav, gate_env, &typer,
                    interner, index, &class_snaps,
                )
            })
            .or_else(|| {
                check_collection_call(
                    call_id, ast, recv, &method, message_span, safe_nav, gate_env, &typer,
                    interner, index, &coll_snaps,
                )
            })
            .or_else(|| {
                check_wrong_arity(
                    ast, recv, &method, &args, args_plain_positional, has_block, message_span,
                    env, &typer, interner, index,
                )
            })
            .or_else(|| {
                check_nil_receiver(call_id, &method, message_span, safe_nav, &nil_snaps, index)
            })
            .or_else(|| {
                check_always_raises(
                    ast, recv, &method, &args, has_block, message_span, env, &typer, interner,
                    index,
                )
            });
        if let Some(diag) = diag {
            out.push(diag);
        }

        // `call.argument-type-mismatch` is an INDEPENDENT axis (argument types),
        // NOT part of the one-per-site validity precedence above: the reference
        // emits it ALONGSIDE `call.wrong-arity` at the same call site (a bad-arity
        // AND wrong-typed-first-arg call yields both). Its own gate keeps it off
        // sites the undefined-method rule owns (it requires the method to be
        // RBS-known on the receiver).
        if let Some(diag) = check_argument_type_mismatch(
            ast,
            recv,
            &method,
            &args,
            args_all_plain,
            env,
            &typer,
            interner,
            index,
        ) {
            out.push(diag);
        }
    }

    // Second pass — `flow.dead-assignment` (ADR-0030). A pure AST/structural
    // check, independent of the typer/index above: it walks each NAMED method
    // body and fires on a plain local write never read in that body. Mirrors the
    // reference `DeadAssignmentCollector` exactly (see `dead_assignments_in_def`).
    // Every NAMED `def` — top-level, class/module body, or nested — lowers to a
    // `Node::Definition { name: Some(..) }` in the arena (a class's direct `def`s
    // are lowered statements, not synthetic copies), so iterating the arena hits
    // each method body EXACTLY ONCE, matching the reference's full DFS over every
    // `DefNode`. A name-less Definition (`class << self`) is skipped — the
    // reference fires only inside named `DefNode`s. The `MethodBody` harvest on
    // ClassDef/ModuleDef is a duplicate VIEW of these same defs (for tier-4b
    // return inference); we deliberately do NOT walk it here, to avoid a double
    // emit.
    for (def_id, node) in ast.iter() {
        if let Node::Definition {
            name: Some(def_name),
            body,
            span,
            param_span,
            ..
        } = node
        {
            dead_assignments_in_def(ast, def_id, def_name, body, *span, *param_span, &mut out);
        }
    }

    // Third pass — `def.override-visibility-reduced` (ADR-35 slice 1). A purely
    // STRUCTURAL def-family check: iterate every `ClassDef`/`ModuleDef`, and for
    // each instance method in its discovered visibility table, fire iff the
    // override strictly REDUCES the visibility of the nearest project ancestor
    // method it overrides. The override span is the method-NAME token of the
    // matching `Definition` in the class body. The OVERRIDING class is identified
    // by its FULLY LEXICALLY-QUALIFIED name (so the project-wide qualified
    // override index resolves its ancestors precisely — the zero-FP keystone).
    // See `check_override_visibility` for the full gate.
    let qualified_names = qualified_class_names(ast);
    for (class_id, node) in ast.iter() {
        let (body, method_visibilities) = match node {
            Node::ClassDef { name, body, method_visibilities, .. }
            | Node::ModuleDef { name, body, method_visibilities, .. }
                if !name.is_empty() =>
            {
                (body, method_visibilities)
            }
            _ => continue,
        };
        let Some(qualified) = qualified_names.get(&class_id) else {
            continue; // un-namable ⇒ skip.
        };
        // Iterate the class body's DIRECT named `Definition` children (the
        // overriding defs), anchoring on each one's name token. A def's recorded
        // visibility comes from the per-node table (by name); a method-name with
        // no direct Definition child (e.g. the untracked `private def foo` form,
        // whose def is a call argument, not a body statement) is simply not seen
        // here — which is correct (that form is silent anyway).
        for &child_id in body {
            let Node::Definition {
                name: Some(method),
                name_span: Some(name_span),
                ..
            } = ast.get(child_id)
            else {
                continue;
            };
            let Some(override_vis) = method_visibilities
                .iter()
                .find(|(m, _)| m == method)
                .map(|(_, v)| *v)
            else {
                continue; // not in the table (singleton / untracked) ⇒ silent.
            };
            if let Some(diag) =
                check_override_visibility(source, qualified, method, override_vis, *name_span)
            {
                out.push(diag);
            }
        }
    }

    // Fourth pass — `flow.unreachable-branch` (ADR-0030). A purely SYNTACTIC,
    // AST/structural check, independent of the typer/index above: it walks every
    // `Node::If` (`if`/`unless`/ternary — Prism parses a ternary as an IfNode too)
    // and fires iff the predicate is a LITERAL node and the resulting dead branch
    // is non-empty. The keyword-inversion (read from `is_unless`) decides which
    // branch is dead, so the diagnostic anchors on the DEAD branch — never on live
    // code. Mirrors the reference's `unreachable_branch_diagnostic`. Iterating the
    // arena hits every `if`/`unless` exactly once (each lowers to one Node::If).
    for (_id, node) in ast.iter() {
        if let Node::If {
            predicate,
            then_body,
            else_body,
            is_unless,
            ..
        } = node
        {
            if let Some(diag) =
                check_unreachable_branch(ast, *predicate, then_body, else_body, *is_unless)
            {
                out.push(diag);
            }
        }
    }

    // Fifth pass — `flow.always-truthy-condition` (ADR-0022 first flow slice). The
    // inferred-constant counterpart to the syntactic `unreachable-branch`: a
    // predicate that the dominating flow scope folds to a `Type::Constant` (e.g.
    // `x = 5; if x`). `always_truthy_snapshots` runs ONE flow-sensitive
    // constant-propagation pass over the file and records, per non-loop/block
    // `if`/`unless`, the predicate's folded type under branch-joined bindings —
    // a strict under-approximation of the reference folder (zero-FP keystone).
    // The rule then applies the reference's remaining skip envelope (syntactic
    // literal → owned by unreachable-branch; defensive predicate call) and fires
    // when the snapshot is a constant. Loop/block suppression is already baked in
    // (those predicates are absent from the snapshot map).
    let truthy_snapshots = typer.always_truthy_snapshots(ast, interner);
    for (id, node) in ast.iter() {
        if let Node::If { predicate, .. } = node {
            if let Some(diag) =
                check_always_truthy(ast, id, *predicate, &truthy_snapshots, interner)
            {
                out.push(diag);
            }
        }
    }

    // Sixth pass — `call.unresolved-toplevel` (ref ADR-34). An implicit-self call
    // (`receiver: None`) at TOPLEVEL scope whose name resolves against NEITHER the
    // `Object`/`Kernel` instance surface NOR a same-file toplevel `def`. Toplevel
    // = the call's span is not contained in any `def`/`class`/`module` span
    // (span-containment, orphan-proof; ADR-24 leniency keeps in-body implicit-self
    // calls silent). See `check_unresolved_toplevel` for the gate.
    unresolved_toplevel_diagnostics(ast, index, source, &mut out);

    // Seventh pass — `call.raise-non-exception` (v0.3.0). Its OWN walk over
    // receiver-None `raise`/`fail` calls (the main call walk is receiver-Some
    // only) — NOT toplevel-restricted, so it fires inside method bodies too. The
    // operand is typed through the shared typer; the verdict + FP gates
    // (project-class bail, module bail, duck `#exception`, redefinition, unknown
    // decline) mirror the reference exactly.
    raise_non_exception_diagnostics(ast, index, source, &typer, &env, interner, &mut out);

    // Eighth pass — `flow.duplicate-hash-key` (v0.3.0). Purely syntactic: walk
    // every Hash literal's precomputed value-pinned key list and fire on a repeat.
    duplicate_hash_key_diagnostics(ast, &mut out);

    // Ninth pass — `flow.return-in-ensure` (v0.3.0). Purely syntactic with a
    // frame-aware envelope: walk every `begin/ensure`'s ensure body for `return`s.
    return_in_ensure_diagnostics(ast, &mut out);

    // Tenth pass — `def.ivar-write-mismatch` (since 0.1.2). Groups each class's
    // instance-method `@x = value` writes by (qualified class, ivar) and fires
    // when a later write's concrete class differs from the canonical one. Types
    // each rvalue through the shared typer (empty local env) plus the rescue-bound
    // exception resolution (increment a); the `Integer()`/`Float()`/`String()`
    // NOMINAL fold (increment b) lives in the typer, so it flows through
    // `type_of` transparently here.
    ivar_write_mismatch_diagnostics(ast, interner, index, source, &typer, &mut out);

    out
}

/// Toplevel `Kernel` methods that the RUNTIME Ruby injects but the vendored
/// RBS does not model, so `class_has_method("Object", …)` misses them. The
/// reference resolves these via runtime reflection on `Object`; rigor-rs mirrors
/// that result with this small, FP-safe allowlist. `gem` is RubyGems' `Kernel#gem`
/// (the only core-only case the corpus FP audit surfaced). Extend as real signal
/// appears — never a false positive, only a missed witness if wrong.
const RUNTIME_KERNEL_TOPLEVEL: &[&str] = &["gem"];

/// Emit `call.unresolved-toplevel` for every toplevel implicit-self call whose
/// name is unresolved. Zero-FP gate (fires ⊆ the reference): suppress on the
/// `Object` RBS surface (`class_has_method("Object", …)` — witnessed-absent only
/// when Object's full core chain is loaded, so a miss there stays silent) AND on
/// same-file toplevel `def` names AND on in-source `Object`/`Kernel`/`BasicObject`
/// reopen methods (the reference's `source_declared_method?` path). `pre_eval:`
/// monkey-patches are not modeled (rigor-rs has no `pre_eval`), so a project that
/// injects toplevel methods that way would see a firing — the reference routes the
/// same case to `pre_eval:` in the message; on the config-less corpus/harness the
/// two agree exactly.
fn unresolved_toplevel_diagnostics(
    ast: &LoweredAst,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
    out: &mut Vec<Diagnostic>,
) {
    // Spans of every CLASS/MODULE body — the NON-toplevel regions. `def` spans are
    // deliberately EXCLUDED: the reference's `scope.toplevel?` means "outside any
    // class/module body", so a TOPLEVEL `def`'s body is still toplevel (the rule
    // fires on an unresolved implicit-self call there) — only a `def` nested in a
    // class/module (a method) is non-toplevel, and its calls fall inside the
    // enclosing class/module span.
    let mut scope_spans: Vec<rigor_parse::Span> = ast
        .iter()
        .filter_map(|(_, n)| match n {
            // A `class << X` singleton-class body is a CLASS scope too — the
            // reference stays silent on implicit-self calls inside it (FP audit:
            // net-ssh/algorithms fired here). A method `def` (name-less or not) is
            // NOT a class scope: a toplevel `def` body still fires.
            Node::ClassDef { .. } | Node::ModuleDef { .. } => Some(n.span()),
            Node::Definition { is_singleton_class: true, .. } => Some(n.span()),
            _ => None,
        })
        .collect();
    scope_spans.extend(meta_new_block_body_spans(ast));
    scope_spans.extend(receiver_eval_block_spans(ast));

    for (_, n) in ast.iter() {
        if let Node::Call { receiver: None, method, message_span, .. } = n {
            // Not at toplevel (nested in a class/module) ⇒ silent (ADR-24).
            if span_contained_in_any(n.span(), &scope_spans) {
                continue;
            }
            // Resolves against a PROJECT-WIDE toplevel `def` / Object-reopen ⇒
            // silent. Cross-file (not just same-file) matches the reference's
            // project-mode resolution — a `def` in a required file resolves the
            // call — which is what keeps the multi-file corpus zero-FP.
            if source.is_toplevel_def(Some(ast.file_key()), method) {
                continue;
            }
            // Present on the Object/Kernel instance surface ⇒ silent. (`false`
            // is witnessed-absent only when Object's whole core chain is loaded;
            // an incomplete chain returns `true` ⇒ we stay silent — never an FP.)
            if index.class_has_method("Object", method) {
                continue;
            }
            // Runtime-injected Kernel toplevel methods the vendored RBS doesn't
            // model, but the live Ruby does — so the reference (which resolves via
            // runtime reflection on `Object`, `check_rules.rb`) stays silent. `gem`
            // (RubyGems' `Kernel#gem`) is the core-only case the net-ssh FP audit
            // surfaced. FP-safe: this only ever silences.
            if RUNTIME_KERNEL_TOPLEVEL.contains(&method.as_str()) {
                continue;
            }
            let severity = catalog(CALL_UNRESOLVED_TOPLEVEL)
                .map(|e| e.default_severity)
                .unwrap_or(Severity::Warning);
            out.push(Diagnostic {
                rule_id: CALL_UNRESOLVED_TOPLEVEL,
                start_offset: message_span.0,
                end_offset: message_span.1,
                message: format!(
                    "unresolved toplevel call to `{method}`. If a project file defines \
                     `{method}` via a toplevel `def` or a monkey-patch on Object/Kernel, list \
                     that file in `.rigor.yml`'s `pre_eval:` (ADR-17) so the analyzer sees it."
                ),
                severity,
                source_family: "builtin",
                receiver_type: None,
                method_name: Some(method.clone()),
            });
        }
    }
}

/// The class-creating meta calls whose literal block body is a CLASS BODY at
/// runtime — the reference's `AnonymousMetaClass::META_NEW_SELECTORS`
/// (upstream #319 / `189c498b`, pin `v0.3.4`). `Struct.new` / `Data.define` are
/// here for the same reason `Class.new` / `Module.new` are: the block is
/// `class_eval`'d on the generated class, so `self` inside it is that class.
const META_NEW_SELECTORS: &[(&str, &str)] = &[
    ("Class", "new"),
    ("Module", "new"),
    ("Struct", "new"),
    ("Data", "define"),
];

/// The block-body regions of every `Class.new do … end` / `Module.new { … }` /
/// `Struct.new(…) do … end` / `Data.define(…) do … end` in the file — NON-toplevel
/// scopes for `call.unresolved-toplevel`.
///
/// Ruby evaluates such a block with `self` bound to the freshly created class, so
/// `attr_reader` there is a class-level macro and not an unresolved toplevel call.
/// The reference learned this away from constant-write position at `v0.3.4`
/// (upstream #319): the body now enters under `Singleton[<anonymous>]`, and
/// `Scope#toplevel?` is `self_type.nil?`, so the rule can no longer fire inside
/// one. Before the fix it did, and rigor-rs matched; at the `v0.3.4` pin the same
/// output became **48 false positives** across the standing sweep (dependabot-core
/// `base_spec.rb` ×40, concurrent-ruby `erlang_actor_spec.rb` ×8 — RSpec's
/// `Class.new(described_class) do … end` / `Module.new do … end` idiom).
///
/// Only the BLOCK BODY is a class scope; the receiver and the arguments keep the
/// enclosing scope, so an implicit-self call in `Class.new(parent_of(x)) { … }`'s
/// arguments still fires. The returned span covers the body statements — a call
/// nested anywhere inside one of them is contained in it.
///
/// The identity half of the upstream change (a synthetic `#<Class:path:line:col>`
/// name, under which the body's `def`s / `attr_*` are registered and which
/// `class_new_lift` returns instead of `Singleton[Object]`) is NOT ported here:
/// it only ever makes the reference emit MORE, so it is coverage, not FP safety.
///
/// **The constant-write rvalue is included**, since upstream #590 / `b3d688f7`
/// (pin `v0.3.8`). It used to be carved out because the reference's own
/// `StatementEvaluator` had no `ConstantWriteNode` handler at all: the rvalue fell
/// to the pure-expression default, its block was never walked, and
/// `ScopeIndexer.propagate` handed every node inside the ENCLOSING scope — a nil
/// `self_type` at file top level, which is exactly what `Scope#toplevel?` keys on.
/// `b3d688f7` added the handler and routes the rvalue block through the same
/// `enter_meta_class_body` the #319 arm uses, so `Registry = Class.new do
/// attr_reader :entries end` is now silent on both sides. Measured at the
/// `v0.3.8` pin (`ffb456b0`): rows d1-d4, d8 and d10' of
/// `docs/notes/20260909-repin-v038-port-spec.md` are silent on the oracle.
///
/// Two spellings where the ORACLE still fires and rigor-rs is silent are coverage
/// gaps, not FP risk, and are deliberately not chased: `X = Class.new do … end
/// .freeze` and `X ||= Class.new do … end` — in both the constant's rvalue is not
/// the meta-new call itself, so upstream's `meta_new_constant_body_context`
/// declines while rigor-rs's span scan sees the inner `Class.new` block regardless.
/// `Outer::F = …` lowers to the recovery carrier (no owned `ConstantPathWrite`
/// variant) and is silent on BOTH sides (row d5).
fn meta_new_block_body_spans(ast: &LoweredAst) -> Vec<rigor_parse::Span> {
    let mut out = Vec::new();
    for (_, n) in ast.iter() {
        let Node::Call {
            receiver: Some(recv),
            method,
            block_body,
            ..
        } = n
        else {
            continue;
        };
        if block_body.is_empty() {
            continue;
        }
        // The receiver MUST be the bare constant (`Class`, or its `::`-rooted
        // spelling, which lowers to the same name) — a call through a variable
        // has no statically known identity, exactly as the reference requires.
        let Node::ConstantRead { name, .. } = ast.get(*recv) else {
            continue;
        };
        if !META_NEW_SELECTORS
            .iter()
            .any(|(konst, sel)| name == konst && method == sel)
        {
            continue;
        }
        let start = block_body.iter().map(|id| ast.get(*id).span().0).min();
        let end = block_body.iter().map(|id| ast.get(*id).span().1).max();
        if let (Some(start), Some(end)) = (start, end) {
            out.push((start, end));
        }
    }
    out
}

/// The reference's `RECEIVER_EVAL_CALL_NAMES` (`check_rules.rb`, upstream #1135
/// / `ee33407e`): the calls whose literal block Ruby evaluates with `self`
/// rebound to the receiver.
const RECEIVER_EVAL_CALL_NAMES: &[&str] = &[
    "class_eval",
    "module_eval",
    "class_exec",
    "module_exec",
    "instance_eval",
    "instance_exec",
];

/// The literal-block regions of every `class_eval` / `module_eval` /
/// `class_exec` / `module_exec` / `instance_eval` / `instance_exec` call in the
/// file — `call.unresolved-toplevel` declines any receiverless call inside one.
///
/// A faithful port of the reference's `receiver_eval_block_ranges` +
/// `call_inside_receiver_eval_ranges?` (upstream #1135, `ee33407e` / `f918c6f0`,
/// pin `e59b7b89`): an eval body is morally a class body, so ADR-34 stays silent
/// there — on a genuinely undefined name too. The reference keys on the call's
/// NAME alone and on offsets alone, so, unlike [`meta_new_block_body_spans`]:
///
/// * the receiver shape is irrelevant — a constant, a local, `self`, another
///   call, or NO receiver at all (a bare `class_eval do … end`) all qualify;
/// * the range is the whole `BlockNode` (`{ … }` and `do … end` alike,
///   parameters and delimiters included), so a call inside a `def` or a nested
///   block within the eval block is covered, and so is a heredoc body that sits
///   textually inside it;
/// * only a LITERAL block counts: the string form `class_eval("…")` and the
///   block-pass `class_eval(&blk)` carry no `BlockNode`, and the arguments keep
///   the enclosing scope — `Foo.class_eval(helper) do … end` still fires on
///   `helper`.
///
/// The measured motive: 9 standing-sweep false positives in rspec's
/// `minitest_integration.rb` (`Minitest::Test.class_eval do include
/// ::RSpec::Matchers … end`). Upstream's companion `fb781023` (moving eval-block
/// `def`s off the toplevel-def table) is deliberately NOT ported with it: alone
/// it only removes resolutions, i.e. would create firings.
fn receiver_eval_block_spans(ast: &LoweredAst) -> Vec<rigor_parse::Span> {
    ast.iter()
        .filter_map(|(_, n)| match n {
            Node::Call { method, block_span: Some(block), .. }
                if RECEIVER_EVAL_CALL_NAMES.contains(&method.as_str()) =>
            {
                Some(*block)
            }
            _ => None,
        })
        .collect()
}

/// Whether `span` is contained in ANY of `spans` (non-strict). Used to decide a
/// call is inside some def/class/module body (a call span never equals a scope
/// span, so no self-match).
fn span_contained_in_any(span: rigor_parse::Span, spans: &[rigor_parse::Span]) -> bool {
    spans.iter().any(|s| s.0 <= span.0 && span.1 <= s.1)
}

/// The defensive predicate selectors the reference's
/// `AlwaysTruthyConditionCollector` skips: a predicate call to one of these reads
/// like an explicit runtime check the (strict-on-returns) type system disagrees
/// with — skipping them keeps the rule on genuine logic errors, not defensive
/// code. Verbatim the reference's `DEFENSIVE_PREDICATES`.
const DEFENSIVE_PREDICATES: &[&str] =
    &["nil?", "empty?", "zero?", "any?", "none?", "all?", "respond_to?"];

/// Build the `flow.always-truthy-condition` diagnostic for one `Node::If`, or
/// `None` (a DECLINE — never a false positive). Fires iff the predicate folds to
/// a `Type::Constant` in the recorded flow snapshot AND is not in the reference's
/// skip envelope:
///   - a SYNTACTIC literal predicate (owned by `flow.unreachable-branch`) →
///     declined here so the two rules never double-fire;
///   - a defensive predicate call (`nil?`/`empty?`/…) → declined;
///   - a loop/block-nested predicate → already absent from `snapshots`.
///
/// The diagnostic anchors on the predicate node span (the reference's
/// `Diagnostic.from_node(predicate_node)`).
fn check_always_truthy(
    ast: &LoweredAst,
    if_id: rigor_parse::NodeId,
    predicate: rigor_parse::NodeId,
    snapshots: &std::collections::HashMap<rigor_parse::NodeId, rigor_types::TypeId>,
    interner: &Interner,
) -> Option<Diagnostic> {
    // Skip syntactic literals (unreachable-branch's domain) and defensive calls.
    if literal_predicate_truthy(ast, predicate).is_some() {
        return None;
    }
    if matches!(ast.get(predicate), Node::Call { method, .. } if DEFENSIVE_PREDICATES.contains(&method.as_str()))
    {
        return None;
    }
    let ty = *snapshots.get(&if_id)?;
    let polarity = constant_polarity(interner, ty)?;

    let span = ast.get(predicate).span();
    let severity = catalog(FLOW_ALWAYS_TRUTHY_CONDITION)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Warning);

    Some(Diagnostic {
        rule_id: FLOW_ALWAYS_TRUTHY_CONDITION,
        start_offset: span.0,
        end_offset: span.1,
        message: format!(
            "condition is always {polarity} (the surrounding flow proves it folds to a constant)"
        ),
        severity,
        source_family: "builtin",
        receiver_type: None,
        method_name: None,
    })
}

/// The polarity word for a constant predicate, or `None` if `ty` is not a
/// `Type::Constant`. Mirrors the reference exactly: a `nil` or `false` constant
/// is `falsey`, every other constant (Integer/Float/String/Symbol/`true`) is
/// `truthy` (in Ruby only `nil`/`false` are falsey).
fn constant_polarity(interner: &Interner, ty: rigor_types::TypeId) -> Option<&'static str> {
    match interner.get(ty) {
        Type::Constant(Scalar::Nil) | Type::Constant(Scalar::Bool(false)) => Some("falsey"),
        Type::Constant(_) => Some("truthy"),
        _ => None,
    }
}

/// `:truthy` / `:falsey` polarity of a SYNTACTICALLY-LITERAL predicate, or `None`
/// for anything else (a variable, constant, call, interpolated string, …). In
/// Ruby every value except `false`/`nil` is truthy — so `true`/Integer/Float/
/// String/Symbol literals are truthy, and only `false`/`nil` are falsey. This
/// mirrors the reference's `TRUTHY_LITERAL_NODES`/`FALSEY_LITERAL_NODES` exactly,
/// with two parity notes carried from the oracle:
///   - An INTERPOLATED string (`"a#{x}"`, a `Node::InterpolatedString`) is NOT a
///     literal here — the reference matches `StringNode` only, not
///     `InterpolatedStringNode` — so it is declined.
///   - A bare-regexp predicate (`if /re/`) is a `MatchLastLineNode` in Prism, not
///     a `RegularExpressionNode`, so the reference does not flag it; rigor-rs has
///     no regexp-literal node at all, so the case is naturally absent.
fn literal_predicate_truthy(ast: &LoweredAst, predicate: rigor_parse::NodeId) -> Option<bool> {
    match ast.get(predicate) {
        Node::TrueLit { .. }
        | Node::IntegerLit { .. }
        | Node::FloatLit { .. }
        | Node::StringLit { .. }
        | Node::SymbolLit { .. } => Some(true),
        Node::FalseLit { .. } | Node::NilLit { .. } => Some(false),
        _ => None,
    }
}

/// Build the `flow.unreachable-branch` diagnostic for one `Node::If`, or `None`
/// (a DECLINE — never a false positive) when the predicate is not a literal or
/// the dead branch is empty/absent. The keyword-inversion is the keystone: for an
/// `if`, a truthy predicate kills the ELSE branch and a falsey one kills the THEN
/// branch; an `unless` INVERTS both. The diagnostic anchors on the DEAD branch:
///   - THEN dead → the then-body's first statement (the reference anchors on the
///     `StatementsNode`, whose start is its first statement — col matches).
///   - ELSE dead → the lowered `else`/subsequent node, whose span starts at the
///     `else` keyword (matching the reference's `from_node(node.subsequent)` /
///     `from_node(node.else_clause)`).
fn check_unreachable_branch(
    ast: &LoweredAst,
    predicate: rigor_parse::NodeId,
    then_body: &[rigor_parse::NodeId],
    else_body: &[rigor_parse::NodeId],
    is_unless: bool,
) -> Option<Diagnostic> {
    let truthy = literal_predicate_truthy(ast, predicate)?;

    // Which branch is dead, accounting for the keyword. For `if`: truthy ⇒ else
    // dead, falsey ⇒ then dead. `unless` inverts (truthy ⇒ then dead, falsey ⇒
    // else dead). `then_dead == truthy` for `unless`, `!truthy` for `if`.
    let then_dead = if is_unless { truthy } else { !truthy };

    // Resolve the dead branch's anchor span. A then-branch is a `Vec` of
    // statements — anchor first-statement-start to last-statement-end (the
    // reference's StatementsNode span). An else-branch is a single lowered node
    // whose span already starts at the `else` keyword. Empty/absent ⇒ DECLINE.
    let span = if then_dead {
        let first = *then_body.first()?;
        let last = *then_body.last()?;
        (ast.get(first).span().0, ast.get(last).span().1)
    } else {
        let dead = *else_body.first()?;
        let s = ast.get(dead).span();
        (s.0, s.1)
    };

    // Byte-exact polarity word (verified against the oracle):
    //   "unreachable branch: literal predicate is always <truthy|falsey>".
    let polarity = if truthy { "truthy" } else { "falsey" };

    let severity = catalog(FLOW_UNREACHABLE_BRANCH)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Warning);

    Some(Diagnostic {
        rule_id: FLOW_UNREACHABLE_BRANCH,
        start_offset: span.0,
        end_offset: span.1,
        message: format!("unreachable branch: literal predicate is always {polarity}"),
        severity,
        source_family: "builtin",
        receiver_type: None,
        method_name: None,
    })
}

/// ADR-35 slice 1: map every `ClassDef`/`ModuleDef` arena id to its FULLY
/// LEXICALLY-QUALIFIED name (`module Outer; module Inner` -> `Inner` maps to
/// `Outer::Inner`), by a recursive walk from the program root tracking the
/// enclosing class/module prefix. This is the SAME qualification the source
/// index's override walk uses, so a subclass and its ancestors key consistently
/// — the zero-FP keystone against last-component name collisions. A declaration
/// whose name is itself a path (`class Foo::Bar`) qualifies head-first.
fn qualified_class_names(ast: &LoweredAst) -> std::collections::HashMap<rigor_parse::NodeId, String> {
    let mut map = std::collections::HashMap::new();
    walk_qualified(ast, ast.root(), &[], &mut map);
    map
}

fn walk_qualified(
    ast: &LoweredAst,
    node: rigor_parse::NodeId,
    prefix: &[String],
    map: &mut std::collections::HashMap<rigor_parse::NodeId, String>,
) {
    match ast.get(node) {
        Node::Program { body, .. } | Node::Statements { body, .. } => {
            for &child in body {
                walk_qualified(ast, child, prefix, map);
            }
        }
        Node::ClassDef { name, body, .. } | Node::ModuleDef { name, body, .. } => {
            if name.is_empty() {
                return;
            }
            let qualified = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{}::{}", prefix.join("::"), name)
            };
            let child_prefix: Vec<String> =
                qualified.split("::").map(|s| s.to_string()).collect();
            map.insert(node, qualified);
            for &child in body {
                walk_qualified(ast, child, &child_prefix, map);
            }
        }
        _ => {}
    }
}

/// The numeric rank of a visibility under the `public > protected > private`
/// ordering (ADR-35 slice 1). A STRICTLY lower override rank than the parent's
/// is a reduction. Mirrors the reference's `VISIBILITY_RANK`.
fn visibility_rank(v: rigor_parse::Visibility) -> u8 {
    match v {
        rigor_parse::Visibility::Public => 2,
        rigor_parse::Visibility::Protected => 1,
        rigor_parse::Visibility::Private => 0,
    }
}

/// Render a visibility as the reference spells it in the diagnostic message
/// (lowercase, NO colon): `public` / `protected` / `private`.
fn visibility_word(v: rigor_parse::Visibility) -> &'static str {
    match v {
        rigor_parse::Visibility::Public => "public",
        rigor_parse::Visibility::Protected => "protected",
        rigor_parse::Visibility::Private => "private",
    }
}

/// Apply `def.override-visibility-reduced` to one overriding instance method.
///
/// Fires (returns `Some`) iff ALL of these hold — each `None` is a DECLINE (a
/// missed witness, NEVER a false positive):
///
///   1. The override is an instance method present in the visibility table
///      (`override_vis` — singleton defs are excluded upstream by lowering).
///   2. [`SourceIndex::nearest_ancestor_defining`] finds a PROJECT-source
///      ancestor that defines `method` (RBS / third-party ancestors are not
///      walked — slice-1 carve-out; an unresolvable / absent ancestor declines).
///   3. **The parent visibility is KNOWN (`Some`).** We NEVER synthesize `Public`
///      from a missing/absent ancestor visibility entry — this is THE documented
///      false-positive cluster in the reference (Mastodon 160 → 35). Only compare
///      when the nearest defining ancestor genuinely records the method in its
///      visibility table.
///   4. The override's rank is STRICTLY LOWER than the parent's
///      (`rank(override) < rank(parent)`). Same-or-wider (a widening
///      `private→protected`, `protected→public`) declines.
///
/// The diagnostic anchors on the overriding def's name token (`name_span`) and
/// reproduces the reference's byte-exact message:
/// `` visibility of `m' reduced from <parent> to <override> (overrides
/// Parent#m); breaks substitutability ``.
fn check_override_visibility(
    source: &rigor_infer::SourceIndex,
    // The FULLY LEXICALLY-QUALIFIED name of the overriding class (e.g.
    // `Organizations::GroupsController`), so the ancestor walk resolves against
    // the project-wide qualified override index precisely.
    qualified_class: &str,
    method: &str,
    override_vis: rigor_parse::Visibility,
    name_span: (usize, usize),
) -> Option<Diagnostic> {
    // Gate 2: a project ancestor must DEFINE the method.
    let (parent_class, parent_vis) = source.nearest_ancestor_defining(qualified_class, method)?;
    // Gate 3 (the keystone): the parent visibility must be KNOWN — NEVER
    // synthesize Public from a missing entry.
    let parent_vis = parent_vis?;
    // Gate 4: strict reduction only.
    if visibility_rank(override_vis) >= visibility_rank(parent_vis) {
        return None;
    }

    let severity = catalog(DEF_OVERRIDE_VISIBILITY_REDUCED)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Warning);
    let message = format!(
        "visibility of `{method}' reduced from {} to {} (overrides {parent_class}#{method}); breaks substitutability",
        visibility_word(parent_vis),
        visibility_word(override_vis),
    );
    Some(Diagnostic {
        rule_id: DEF_OVERRIDE_VISIBILITY_REDUCED,
        start_offset: name_span.0,
        end_offset: name_span.1,
        message,
        severity,
        source_family: "builtin",
        receiver_type: None,
        method_name: Some(method.to_string()),
    })
}

// ---------------------------------------------------------------------------
// Rule implementations
// ---------------------------------------------------------------------------

/// The reference's `METACLASS_ARMS` (`analysis/check_rules.rb`), verbatim: a value
/// typed `Class` or `Module` is SOME class or module object, and its singleton
/// methods cannot be read off the metaclass — `def self.included(base)` receives
/// the includer, so `base.class_attribute :main_menu` is a call on whatever
/// included the module.
const METACLASS_ARMS: &[&str] = &["Class", "Module"];

/// Whether `call.undefined-method` must DECLINE on a receiver whose resolved class
/// is `class_name`, because that class's method surface is not enumerable.
///
/// The port of upstream #742 / PR #743 (`23341a87`) — `unenumerable_receiver?`,
/// which is `METACLASS_ARMS ∪ unbounded_receiver_surface?` and runs on BOTH the
/// instance and the singleton side. rigor-rs has no `unbounded_receiver_surface?`
/// analogue at this seam (ADR-26 open classes and synthesized stubs are handled by
/// the conservative `class_has_method` completeness gate), so only the metaclass
/// half needs porting. Measured at pin `ffb456b0`: `Class.frobnicate` and
/// `Module.frobnicate` — SINGLETON reads of the two constants — are silent on the
/// oracle and were rigor-rs false positives (rows x3/x4 of
/// `docs/notes/20260909-repin-v038-rules-families.md`).
fn unenumerable_metaclass_receiver(class_name: &str) -> bool {
    METACLASS_ARMS.contains(&class_name.strip_prefix("::").unwrap_or(class_name))
}

/// Whether `call.undefined-method` must DECLINE on an INSTANCE-side receiver whose
/// resolved class is `class_name`.
///
/// The port of upstream #739 / PR #741 (`3636649f`) joined with the metaclass half
/// above. A value typed as a mixin MODULE is an instance of whatever class includes
/// the module, and that class contributes an arbitrary surface: in RBS a parameter
/// typed `Taggable` means "something whose class includes Taggable", not "something
/// whose methods are Taggable's". Nothing there can prove a method absent, so the
/// reference stopped retrying the lookup against `Object` and declines outright
/// (`module_mixin_receiver?` in `last_resort_surface_answers?`).
///
/// SCOPE, three ways, each measured rather than argued:
///
/// * INSTANCE side only for the module half. The reference's `module_mixin_receiver?`
///   tests `receiver_type.is_a?(Type::Nominal)`, so a SINGLETON module receiver keeps
///   firing — `Comparable.typo`, `Digest::Instance.typo` (rows c4/c5) — because a
///   namespace module's `module_function` / `def self.` surface is real and
///   enumerable. Declining every `Singleton` receiver would silence those and `String
///   .typo` (c11); that is the control an over-broad fix fails.
/// * `call.undefined-method` only. Upstream moved nothing else: `unenumerable_receiver?`
///   is called from the undefined-method diagnostic alone, and the arity rule reads the
///   narrower `unbounded_receiver_surface?`. Probed at the pin: `v.hexdigest(1, 2, 3)`
///   behind a `Digest::Instance` guard still reports `call.wrong-arity` on the oracle
///   (row x1) — rigor-rs is silent there for an unrelated, pre-existing reason.
/// * The QUALIFIED name, not the short key. `is_qualified_module` asks the isolated
///   registry exactly as the reference's `rbs_module?` asks `env.class_decls`; the
///   short-key map answers `false` for `Digest::Instance` and would leak a nested
///   module's moduleness onto an unrelated project class of the same leaf name.
///
/// The one shape upstream keys on SYNTAX rather than type — `mixin_self_class_receiver?`,
/// i.e. `v.class.typo` where `v` is mixin-typed — needs no port: rigor-rs is already
/// silent there on both engines (row c6), because `.class` on a Dynamic receiver
/// yields no witnessable carrier.
fn unenumerable_instance_receiver(index: &CoreIndex, class_name: &str) -> bool {
    unenumerable_metaclass_receiver(class_name) || index.is_qualified_module(class_name)
}

/// Apply `call.undefined-method` to a single call with a receiver.
///
/// Zero-false-positive gate (ADR-0023): emit *only* when the receiver's concrete
/// class is **RBS-known in the core surface** AND that class is known to lack the
/// method. If the receiver is `Dynamic`/unknown, or its class is a project-defined
/// (in-source) or non-core `.new` instance, emit nothing — never guess.
///
/// ## Why in-source / non-core `.new` instances are NOT witnessed
///
/// The reference gates this rule on `rbs_class_known?(class_name)`
/// (`check_rules.rb:556`): a project-defined class — or a non-core class reached
/// only through `X.new` — is treated **leniently**. A method MISS on such a
/// receiver stays `Dynamic[top]` and silent, because Ruby routinely defines
/// methods dynamically (ADR-0023 tier-4: "on a miss, the call stays Dynamic").
/// Empirically the reference is silent on `Point.new.typo`, `MyError.new.typo`,
/// `Pathname.new.typo`, `Set.new.typo`, and `Struct.new(...).new`, while it DOES
/// witness on literals, RBS-method returns, and core `X.new` (`Array.new.typo`).
///
/// The in-source/registry surface ([`rigor_infer::SourceIndex`]) still types such
/// instances — for chained RETURN inference and `X.new` identity — but it is
/// never a *witnessing* surface for this rule. Honouring that boundary is the
/// keystone that keeps real project code (incl. Rails models) false-positive-free.
// too_many_arguments: a rule-check fn threading the full typing context (ast, receiver,
// span, env, typer, interner, index); bundling into a struct would obscure the call sites.
#[allow(clippy::too_many_arguments)]
fn check_call(
    ast: &LoweredAst,
    receiver: rigor_parse::NodeId,
    method: &str,
    message_span: (usize, usize),
    safe_nav: bool,
    env: &rigor_infer::TypeEnv,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
) -> Option<Diagnostic> {
    let recv_ty = typer.type_of(ast, receiver, env, interner);

    // Singleton (class-object) receiver: a bare constant `C` typed to
    // `Type::Singleton(class)` (see the typer's `ConstantRead` arm + its zero-FP
    // gate). Witness a CLASS-method typo (`Time.current`) against the RBS
    // class-method surface. This branch MUST come first: `class_name_of` returns
    // `None` for a Singleton carrier, so the instance path below would skip it.
    if let Type::Singleton(class) = interner.get(recv_ty) {
        let class = *class;
        let Some(name) = typer.source().class_name_for_id(class) else {
            return None; // not round-trippable ⇒ silent (never guess).
        };
        // Upstream #742 — `Class.typo` / `Module.typo`. The reference's
        // `unenumerable_receiver?` runs before the surface lookup on BOTH sides,
        // so the two generic metaclasses decline here as well. The MODULE half of
        // the retraction deliberately does NOT reach this branch: a named module's
        // singleton surface is real and enumerable, and `Comparable.typo` /
        // `Digest::Instance.typo` keep firing (rows c4/c5).
        if unenumerable_metaclass_receiver(name) {
            return None;
        }
        // `class_has_singleton_method` is conservative: `false` only when the
        // class-method surface is fully known and lacks the method (handles
        // `extend`ed modules; incomplete/unknown ⇒ `true` ⇒ silent).
        if index.class_has_singleton_method(name, method) {
            return None;
        }
        let receiver_render = format!("singleton({name})");
        let message = format!("undefined method `{method}' for {receiver_render}");
        let severity = catalog(CALL_UNDEFINED_METHOD)
            .map(|e| e.default_severity)
            .unwrap_or(Severity::Error);
        return Some(Diagnostic {
            rule_id: CALL_UNDEFINED_METHOD,
            start_offset: message_span.0,
            end_offset: message_span.1,
            message,
            severity,
            source_family: "builtin",
            receiver_type: Some(receiver_render),
            method_name: Some(method.to_string()),
        });
    }

    // RBS-known class instance carried by a source-registry `Nominal` that
    // `class_name_of` (core-id only) will not resolve — recover the name from
    // the source registry and witness when the loaded RBS models the class
    // (`knows_class`): a project-`sig/` class (ADR-0033, `Widget.new` — project
    // sig is authoritative) AND a stdlib/core instance minted by a
    // declaration-driven path (a singleton RBS return — `Date.today`,
    // `Time.now` — or a C5 `Range` constant), which the reference witnesses
    // identically (probed: `Date.today.end_of_month`, `R.frobnicate` for a
    // Range constant both fire there). An in-source-only class (not in any
    // loaded RBS) stays lenient, and the stdlib `.new` leniency
    // (`Pathname.new("x").nope`) now lives in `type_dot_new`, which declines
    // the mint for a bundled non-core class, so no such Nominal reaches this
    // gate. `class_has_method` keeps its conservative completeness gate (an
    // incomplete ancestor chain ⇒ `true` ⇒ silent).
    // Gated on `knows_toplevel_class` (∪ project-sig), NOT `knows_class`: a
    // name the index knows only via a namespaced short-key registration
    // (`Instance` = some RBS `…::Instance`; the defect-2 set) can equally be a
    // PROJECT model's bare name (`Clusters::Instance` — gitlab), which the
    // reference resolves lexically to the project class and stays silent on;
    // witnessing it against the unrelated stdlib surface is an FP (caught by
    // fp_audit on gitlab app/models the first time this gate was
    // `knows_class`-wide).
    if index.class_name_of(interner, recv_ty).is_none() {
        // UNION receiver — the reference's `union_undefined_method_diagnostic`
        // (`check_rules.rb:1921`), reached exactly where the scalar path finds
        // no single concrete class (`class_name.nil?`). Fire only when EVERY
        // arm is a fully-known, bounded, non-nil instance class that lacks the
        // method — `x = [1, 2].tap { break "s"; break 1 }; x.push 3` witnesses
        // `"s" | 1` (rigor-rs#140). Any nil-bearing, safe-navigated,
        // Dynamic/unknown, singleton, metaclass, or module-mixin arm — or a
        // union whose arms collapse to ONE class — declines to silence, the
        // zero-FP direction the same reference encodes (`any?` permissive on
        // uncertainty). A single-class union (`"a" | "b"` — both String) is a
        // join artifact the scalar rule already owns.
        if let Type::Union(_) = interner.get(recv_ty) {
            return check_union_call(recv_ty, method, message_span, safe_nav, typer, interner, index);
        }
        if let Some(name) = typer.source().class_name_for_id_of(interner, recv_ty) {
            // `knows_toplevel_class` ALONE (ADR-0042 gate probe s5): a
            // TOPLEVEL project-sig class (`Widget`) is in the toplevel set via
            // its authoritative registration, so the former
            // `|| is_project_sig_class` arm only ever ADDED nested-only sig
            // classes (`module Outer; class Inner` reached by a bare `Inner`
            // read) — a short-key artifact door the reference does not have:
            // it witnesses `Outer::Inner` through the QUALIFIED path only and
            // keeps bare `Inner` (which resolves to nothing at runtime)
            // silent. Probed: rigor-rs fired `spni' for Inner` where the
            // reference is silent — an oracle FP shape, now closed.
            // ADR-0042 Slice 3: check the ISOLATED qualified surface, not the
            // short-key merge. A toplevel project-sig `Status` colliding with a
            // NESTED stdlib `Process::Status` no longer silently inherits
            // `exited?` from the stdlib class (fixture 70 — residual defect-2
            // unsoundness). For a non-colliding class or a toplevel-vs-toplevel
            // collision, this is identical to `class_has_method`.
            // ADR-0042 Slice 4: fire for a TOPLEVEL known class (unchanged) OR
            // a NESTED project-sig class (`Outer::Inner`) — the reference
            // witnesses the latter through the qualified path, which
            // `knows_toplevel_class` refuses for the defect-2 reason. Both use
            // the ISOLATED qualified surface.
            // MultiWrite substrate Slice 2: fire for a NESTED name the loaded
            // RBS models under its QUALIFIED key (`Process::Status`, reached
            // through `Process.wait2`'s tuple return). `knows_toplevel_class`
            // refuses every namespaced name for the defect-2 reason (a SHORT key
            // like `Status` may be a project class), but the qualified key is an
            // isolated entry that no project name can collide with — the same
            // argument the typer's `ConstantRead` arm already makes for
            // `ERB::Util`, and the same surface `qualified_class_has_method`
            // checks. Adds nothing for a bare name: a top-level class is already
            // in `knows_toplevel_class`, and a nested-only class's SHORT key has
            // no qualified entry.
            //
            // The DECLARATION-ONLY restriction was measured, not theorised —
            // but on a premise that has since EXPIRED. In 2026-07 rigor-rs's
            // surface for a namespaced GEM class was weaker than the oracle's
            // (the reference's `data/vendored_gem_sigs/`, then unvendored) and
            // `Gem::Version.new("1.0").segments` fired here while the oracle
            // stayed silent. `800b3a1` vendored those sigs on 2026-07-31; the
            // port's surface for those classes is now COMPLETE and that FP
            // cannot re-open (measured 2026-09-09, 651 names per class). What
            // holds the restriction up today is rigor-rs#123's 26 ancestor-
            // closure holes. See `SourceIndex::is_declaration_only_class` for
            // the full argument and the audit of what remains reachable.
            // A project class carries only its SHORT key here, so a bundled RBS
            // class of the same bare name is a DIFFERENT class and its method
            // table is the wrong surface to witness against — WHEREVER Ruby's
            // lexical lookup would reach the project one. The reference resolves
            // the constant lexically and stays silent there
            // (`RSpec::Core::DidYouMean#call` vs stdlib's `module DidYouMean`).
            // The gate is the SAME lexical predicate the C1 constant-shadow gate
            // uses, not a global "the project defines this name somewhere": a
            // gitlab-foss `Gitlab::Database::Partitioning::Time` must not silence
            // `Time.parse(x).in_time_zone` over in `Gitlab::GithubImport`, where
            // the project `Time` is not visible and the oracle does fire.
            // The shadow test applies ONLY to the bundled-RBS arm. A project
            // SIG class is authoritative for its own name (fixture 70), and the
            // declaration-only arm is already isolated by its qualified key.
            // Upstream #739/#742 — an INSTANCE receiver whose class is an RBS
            // module (its includer contributes an arbitrary surface) or the
            // generic `Class`/`Module`. Placed before the resolution gates for the
            // same reason the reference puts `unenumerable_receiver?` at the top of
            // the diagnostic: the question is about the receiver, not about which
            // surface happens to model it.
            if unenumerable_instance_receiver(index, name) {
                return None;
            }
            let use_prefix = typer.enclosing_prefix(message_span);
            let bundled_toplevel = index.knows_toplevel_class(name)
                && !typer.source().constant_shadowed(name, use_prefix);
            if (bundled_toplevel
                || index.is_qualified_project_sig_class(name)
                || (index.knows_qualified_class(name)
                    && typer.source().is_declaration_only_class(name)))
                && !index.qualified_class_has_method(name, method)
            {
                let receiver_render = render_receiver(interner, index, typer.source(), recv_ty);
                let message = format!("undefined method `{method}' for {receiver_render}");
                let severity = catalog(CALL_UNDEFINED_METHOD)
                    .map(|e| e.default_severity)
                    .unwrap_or(Severity::Error);
                return Some(Diagnostic {
                    rule_id: CALL_UNDEFINED_METHOD,
                    start_offset: message_span.0,
                    end_offset: message_span.1,
                    message,
                    severity,
                    source_family: "builtin",
                    receiver_type: Some(receiver_render),
                    method_name: Some(method.to_string()),
                });
            }
        }
    }

    // Witness ONLY over a class the core (RBS/CORE_CLASSES) surface models and
    // round-trips by id. A receiver that resolves only through the in-source /
    // registry surface (a project class, or a non-core `X.new` like Pathname)
    // returns `None` here ⇒ silent (reference leniency, see the rustdoc above).
    let class_name = index.class_name_of(interner, recv_ty)?;
    // Upstream #739/#742 — the core-id twin of the decline above. `class_name_of`
    // answers only with a `CORE_CLASSES` name today, none of which is a module or a
    // metaclass, so this is defence in depth: it keeps the two instance paths
    // answering the same question the same way should that array ever widen.
    if unenumerable_instance_receiver(index, class_name) {
        return None;
    }
    if !index.knows_class(class_name) {
        return None;
    }
    if index.class_has_method(class_name, method) {
        return None;
    }
    // The project's OWN declaration of the method on this class wins over the RBS
    // surface, exactly as the reference's `source_declared_method?` gate does
    // (checked before it reaches `Reflection.rbs_class_known?`). A reopened core
    // class contributes methods RBS cannot know about — rake's `class String`
    // adds `#ext` and `#pathmap_explode` — and witnessing their absence against
    // RBS alone is a false positive.
    if typer.source().project_declares_method(typer.file_key(), class_name, method) {
        return None;
    }
    // `last_resort_surface_answers?`'s `ancestry_declares_method?` — the same
    // question asked through the project's OWN ancestry (`class String;
    // include M` makes `String#m` exist for Rigor without touching RBS).
    // Asked last, exactly like the reference: this is the one probe that
    // walks the class graph.
    if typer
        .source()
        .project_declares_method_through_ancestors(typer.file_key(), class_name, method)
    {
        return None;
    }

    // We have witnessed absence over a core/RBS class. Render the receiver in the
    // reference's spelling (value-pinned for a Constant/Tuple, else the class
    // name) via the shared display layer.
    let receiver_render = render_receiver(interner, index, typer.source(), recv_ty);
    let message = format!("undefined method `{method}' for {receiver_render}");

    let severity = catalog(CALL_UNDEFINED_METHOD)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Error);

    // `receiver_type` in the structured field matches the reference's rendering:
    // for a Constant receiver it is the rendered value (e.g. `"\"Hello\""` for
    // a String literal, `"nil"` for nil), not the bare class name. This matches
    // the reference's JSON output which sets `receiver_type` to `"\"Hello\""`.
    Some(Diagnostic {
        rule_id: CALL_UNDEFINED_METHOD,
        start_offset: message_span.0,
        end_offset: message_span.1,
        message,
        severity,
        source_family: "builtin",
        receiver_type: Some(receiver_render),
        method_name: Some(method.to_string()),
    })
}

/// `call.undefined-method` over a UNION receiver — the port of the reference's
/// `union_undefined_method_diagnostic` (`check_rules.rb:1921`), reached where
/// the scalar path finds no single concrete class. It fires only when EVERY
/// arm is a fully-known, bounded, non-nil, instance-side class that lacks the
/// method (`"s" | 1` calling `push`), mirroring the reference's gate order:
///
/// - `safe_navigation?` declines outright;
/// - any nil arm (`Constant[nil]` / `NilClass`) keeps `T | nil` silent — the
///   deliberate N3 decision (ADR-62);
/// - any UNANSWERABLE arm — `class_name_of` resolving `None` (Dynamic / Top /
///   Bot / Singleton / a non-core surface; `Singleton` covers the reference's
///   explicit singleton bail), a metaclass (`Class` / `Module`), or an RBS
///   module mixin — declines, since no sound "absent on every arm" verdict
///   exists there;
/// - a class the bundled surface does not model (`!knows_class`) is likewise
///   unanswerable — the reference's permissive `return true` from
///   `method_present_anywhere?`;
/// - the method PRESENT on any arm (project `def` — `source_declared_method?`
///   — or the conservative `class_has_method` surface) declines;
/// - fewer than two DISTINCT arm classes (`"a" | "b"`, one `String`) is a
///   join artifact — the scalar rule's job, never this one's.
fn check_union_call(
    recv_ty: rigor_types::TypeId,
    method: &str,
    message_span: (usize, usize),
    safe_nav: bool,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
) -> Option<Diagnostic> {
    let Type::Union(members) = interner.get(recv_ty) else {
        return None;
    };
    let members = members.clone();
    if safe_nav {
        return None;
    }
    let mut arm_classes: Vec<&'static str> = Vec::new();
    for member in members {
        let is_nil_member = matches!(interner.get(member), Type::Constant(Scalar::Nil))
            || index.class_name_of(interner, member) == Some("NilClass");
        if is_nil_member {
            return None;
        }
        let class_name = index.class_name_of(interner, member)?;
        if unenumerable_instance_receiver(index, class_name) {
            return None;
        }
        if !index.knows_class(class_name) {
            return None;
        }
        if typer.source().project_declares_method(typer.file_key(), class_name, method)
            || index.class_has_method(class_name, method)
            || typer
                .source()
                .project_declares_method_through_ancestors(typer.file_key(), class_name, method)
        {
            return None;
        }
        arm_classes.push(class_name);
    }
    if arm_classes
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>()
        .len()
        < 2
    {
        return None;
    }
    let receiver_render = render_receiver(interner, index, typer.source(), recv_ty);
    let message = format!("undefined method `{method}' for {receiver_render}");
    let severity = catalog(CALL_UNDEFINED_METHOD)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Error);
    Some(Diagnostic {
        rule_id: CALL_UNDEFINED_METHOD,
        start_offset: message_span.0,
        end_offset: message_span.1,
        message,
        severity,
        source_family: "builtin",
        receiver_type: Some(receiver_render),
        method_name: Some(method.to_string()),
    })
}

/// Apply `call.undefined-method` to a call whose bare-local receiver was
/// class-narrowed by the flow pass (census mechanism 1) — the `is_a?`/
/// `case-when` counterpart of [`check_call`], firing from the precomputed
/// [`rigor_infer::Typer::class_narrowing_snapshots`] map.
///
/// The FP-delicate flow reasoning (which guard shapes narrow, truthy-edge-only,
/// invalidation, block-scope discipline) lives in the snapshot pass; here we
/// apply the residual gates, each `None` FP-safe:
/// 1. NOT a safe-nav call (belt-and-braces — the pass also skips them).
/// 2. The call node is in the snapshot map with a class name `C`.
/// 3. The receiver still types `Dynamic`/`Top` at the use site (the narrowing
///    only ever REPLACES a Dynamic/Top carrier — `narrow_class_other`
///    semantics; any concrete carrier means the ordinary [`check_call`] path
///    owns the site and has already declined).
/// 4. The witnessing tail mirrors [`check_call`]'s QUALIFIED path over
///    `Nominal[C]`: `C` resolves on one of the three accepted surfaces (a
///    genuine top-level RBS class, a project-`sig/` class, or the bundled
///    qualified registry), the method is certainly ABSENT on its ISOLATED
///    qualified surface, and the project does not reopen `C` with the method
///    (`project_declares_method`). `C` arrives already resolved to a qualified
///    key — see `Typer::resolve_constant_as_written`.
#[allow(clippy::too_many_arguments)]
fn check_narrowed_call(
    call_id: rigor_parse::NodeId,
    ast: &LoweredAst,
    receiver: rigor_parse::NodeId,
    method: &str,
    message_span: (usize, usize),
    safe_nav: bool,
    env: &rigor_infer::TypeEnv,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
    snapshots: &std::collections::HashMap<rigor_parse::NodeId, String>,
) -> Option<Diagnostic> {
    // (1) Safe-nav dispatch is out of the narrowed envelope.
    if safe_nav {
        return None;
    }
    // (2) The flow pass must have narrowed this exact call's receiver.
    let class_name = snapshots.get(&call_id)?.as_str();
    // (3) Dynamic/Top-only: any concrete carrier at the use site declines.
    let rty = typer.type_of(ast, receiver, env, interner);
    if !matches!(interner.get(rty), Type::Dynamic(_) | Type::Top) {
        return None;
    }
    // (4) Witness absence over the RESOLVED surface, exactly as `check_call`'s
    // qualified tail. S2 (2026-08-08) replaced the old pair of gates —
    // `knows_toplevel_class` AND `CoreIndex::class_id` — with ONE resolution
    // path. Each was an independent blocker:
    //   * `knows_toplevel_class` refuses EVERY namespaced name (the defect-2
    //     rule, which stays in force for every other consumer — this slice adds
    //     a path, it does not invert the gate);
    //   * `class_id` interns over `CORE_CLASSES`, a NINE-element array, so the
    //     witness fired for those nine names only. `Time`/`Range`/`Struct`/
    //     `Pathname` guards are reference-firing and were rigor-rs-silent
    //     (probes u1/u2/u4/u5) despite passing `knows_toplevel_class`.
    // The name arrives already resolved to a qualified key
    // (`Typer::resolve_constant_as_written`, at mint time where the use site's
    // lexical prefix is available), so all three spellings of a class — bare,
    // qualified, `::`-absolute — reach the same surface and the same rendering.
    //
    // Upstream #739/#742 (pin `v0.3.8`) — the guard NAMES the receiver's class,
    // and when that class is an RBS module or the generic `Class` / `Module` the
    // surface is not enumerable. This is the path the whole F-C family actually
    // fires from: `return unless v.is_a?(Digest::Instance)` types `v` through the
    // narrowing snapshot, not through a carrier `check_call` can see. `class_name`
    // arrives already resolved to a QUALIFIED key
    // (`Typer::resolve_constant_as_written`), which is exactly the spelling
    // `is_qualified_module` wants.
    //
    // Note what this does NOT touch: an `Enumerable` guard on a value already
    // typed `Array` keeps the `Array` bound (subclass ordering), so `class_name`
    // there is `"Array"` and the witness still fires on both engines — row x11,
    // the control an "any module in the guard" test would have silenced.
    if unenumerable_instance_receiver(index, class_name) {
        return None;
    }
    // Accepted surfaces: the existing top-level one, the project's own `sig/`
    // (nested included — probes q2/q3/q8), and the bundled qualified registry.
    // Everything else DECLINES, which is free: an unresolvable name (p2/p2b)
    // and an in-source-only project class (ADR-0033 provenance, p5/q1) are both
    // reference-silent.
    if !index.knows_toplevel_class(class_name)
        && !index.is_qualified_project_sig_class(class_name)
        && !index.knows_qualified_class(class_name)
    {
        return None;
    }
    // The ISOLATED qualified surface, as ADR-0042 Slice 3 established for
    // `check_call`: a project class named `Status` must not inherit stdlib
    // `Process::Status`'s methods. For a top-level name the qualified key IS the
    // short key, so this is the same surface the old `class_has_method` read,
    // minus the short-key merge.
    if index.qualified_class_has_method(class_name, method) {
        return None;
    }
    // A project reopen MERGES with the RBS surface rather than replacing it
    // (probe q6): the reopened method silences, the still-absent one still
    // fires. Keyed as written, so `Proj::Thing` matches.
    if typer.source().project_declares_method(typer.file_key(), class_name, method) {
        return None;
    }
    // Render the narrowed receiver as the reference does — the FULL resolved
    // path ("undefined method `frobnicate_zzz' for URI::HTTP", probes §1/§3),
    // which for a core name is the same plain class name `Nominal[C]` rendered
    // before ("… for Hash"). No `ClassId` is needed: `render_receiver` on a
    // `Nominal` yields exactly its class name.
    let receiver_render = class_name.to_string();
    let message = format!("undefined method `{method}' for {receiver_render}");
    let severity = catalog(CALL_UNDEFINED_METHOD)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Error);
    Some(Diagnostic {
        rule_id: CALL_UNDEFINED_METHOD,
        start_offset: message_span.0,
        end_offset: message_span.1,
        message,
        severity,
        source_family: "builtin",
        receiver_type: Some(receiver_render),
        method_name: Some(method.to_string()),
    })
}

/// Apply `call.undefined-method` to a call whose bare-local receiver carries a
/// COLLECTION shape the flow pass threaded — the mutation/local-binding
/// counterpart of [`check_call`], firing from the precomputed
/// [`rigor_infer::Typer::collection_shape_snapshots`] map (spec
/// docs/notes/20260807-collection-shape-slice-spec.md, stage 1).
///
/// All the FP-delicate flow reasoning (which seeds mint a carrier, which
/// mutators keep the nominal, how branch joins and block bodies decline) lives in
/// the snapshot pass. Here we apply the residual gates, each `None` FP-safe, in
/// exact parallel with [`check_narrowed_call`]:
/// 1. NOT a safe-nav call (belt-and-braces — the pass also skips them).
/// 2. The call node is in the snapshot map with a class name `C` ("Array"/"Hash").
/// 3. The receiver still types `Dynamic`/`Top` at the use site. This is what
///    keeps the rule to its intended job: at TOP level [`ScopedEnv`] already
///    binds the local, so [`check_call`] owns the site and has already decided
///    it; only a use inside a `def` body (empty scoped env ⇒ Dynamic) reaches
///    here, which is exactly the coverage gap the slice closes.
/// 4. The witnessing tail mirrors [`check_call`]'s core path over `Nominal[C]`.
///
/// [`ScopedEnv`]: crate::ScopedEnv
#[allow(clippy::too_many_arguments)]
fn check_collection_call(
    call_id: rigor_parse::NodeId,
    ast: &LoweredAst,
    receiver: rigor_parse::NodeId,
    method: &str,
    message_span: (usize, usize),
    safe_nav: bool,
    env: &rigor_infer::TypeEnv,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
    snapshots: &std::collections::HashMap<rigor_parse::NodeId, &'static str>,
) -> Option<Diagnostic> {
    // (1) Safe-nav dispatch is out of the envelope.
    if safe_nav {
        return None;
    }
    // (2) The flow pass must have typed this exact call's receiver.
    let class_name = *snapshots.get(&call_id)?;
    // (3) Dynamic/Top-only: any concrete carrier at the use site declines.
    let rty = typer.type_of(ast, receiver, env, interner);
    if !matches!(interner.get(rty), Type::Dynamic(_) | Type::Top) {
        return None;
    }
    // (4) Witness absence over the core surface, exactly as `check_call`'s tail.
    if !index.knows_toplevel_class(class_name) {
        return None;
    }
    if index.class_has_method(class_name, method) {
        return None;
    }
    if typer.source().project_declares_method(typer.file_key(), class_name, method) {
        return None;
    }
    let class = index.class_id(class_name)?;
    let recv_ty = interner.intern(Type::Nominal { class, args: vec![] });
    let receiver_render = render_receiver(interner, index, typer.source(), recv_ty);
    let message = format!("undefined method `{method}' for {receiver_render}");
    let severity = catalog(CALL_UNDEFINED_METHOD)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Error);
    Some(Diagnostic {
        rule_id: CALL_UNDEFINED_METHOD,
        start_offset: message_span.0,
        end_offset: message_span.1,
        message,
        severity,
        source_family: "builtin",
        receiver_type: Some(receiver_render),
        method_name: Some(method.to_string()),
    })
}

/// Apply `call.wrong-arity` to a single call with a receiver.
///
/// Zero-false-positive gate (ADR-0023), mirroring the reference's conservative
/// envelope: emit *only* when
///   - the receiver types to a concrete class the [`CoreIndex`] models,
///   - that class is known to DEFINE the method (so this is genuinely an arity
///     violation, not an undefined method — that's the other rule's job),
///   - [`rigor_index::method_arity`] returns a known `(min, max)` envelope, AND
///   - the positional-argument count is definitely outside `[min, max]`.
///
/// A variadic method (`max == None`) only triggers on `args < min`. Any
/// Dynamic / unknown receiver, unmodeled method, or unmodeled arity => silent.
// too_many_arguments: a rule-check fn threading the full typing context (ast, receiver,
// args, span, env, typer, interner, index); bundling into a struct would obscure the call sites.
#[allow(clippy::too_many_arguments)]
fn check_wrong_arity(
    ast: &LoweredAst,
    receiver: rigor_parse::NodeId,
    method: &str,
    args: &[rigor_parse::NodeId],
    args_plain_positional: bool,
    has_block: bool,
    message_span: (usize, usize),
    env: &rigor_infer::TypeEnv,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
) -> Option<Diagnostic> {
    // The reference's `plain_positional_call?` gate (`check_rules.rb:1510`,
    // `simple_positional?`): a call carrying ANY non-positional argument
    // shape — a `*splat`, a bare keyword-hash `a: 1`, or forwarded `...` —
    // is never arity-checked, because `args.len()` is then not the runtime
    // positional count. A `&blk` block-pass does NOT disqualify (the
    // reference reads only `call_node.arguments`, and the oracle fires on
    // `first(1, 2, &)`); `has_block` below handles the block shapes the
    // port cannot yet arity-check.
    if !args_plain_positional {
        return None;
    }

    // The reference arity-checks a block-bearing call too: its
    // `compute_arity_envelope` is a min/max collapse over EVERY overload and
    // the oracle fires that collapsed message on a block call (measured:
    // `[1, 2].map(1) { |z| z }` -> `given 1, expected 0`). We store the same
    // collapsed envelope but stay silent on any block-bearing call: firing a
    // collapse that includes block-less overloads against a call the runtime
    // will dispatch on the block overload has not been verified FP-safe, so
    // until per-overload arity is stored this is a conservative coverage gap
    // (a missed witness, never an extra one). Block-form RETURN typing IS
    // modeled (see `Typer::type_block_call`), so chained undefined-method on
    // a block result is still witnessed — only the block-call's own arity is
    // deferred.
    if has_block {
        return None;
    }

    let recv_ty = typer.type_of(ast, receiver, env, interner);

    // Resolve the receiver's class; `None` => Dynamic/unknown => silent.
    let class_name = index.class_name_of(interner, recv_ty)?;
    if !index.knows_class(class_name) {
        return None;
    }
    // Only check arity for a method the class actually defines — otherwise the
    // undefined-method rule owns this call site (no double-emit).
    if !index.class_has_method(class_name, method) {
        return None;
    }

    // A known arity envelope is required — never guess on an unmodeled method.
    let (min, max) = index.method_arity(class_name, method)?;

    let given = args.len();
    let too_few = given < min;
    let too_many = max.is_some_and(|m| given > m);
    if !(too_few || too_many) {
        return None;
    }

    // Render the expected envelope the reference's way: a bare count when the
    // arity is fixed (`min == max`), else `min..max`. A variadic upper bound is
    // not reachable here (too_few only), but render it defensively as `min..`.
    let expected = match max {
        Some(m) if m == min => min.to_string(),
        Some(m) => format!("{min}..{m}"),
        None => format!("{min}.."),
    };
    let message = format!(
        "wrong number of arguments to `{method}' on {class_name} (given {given}, expected {expected})"
    );

    let severity = catalog(CALL_WRONG_ARITY)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Error);

    Some(Diagnostic {
        rule_id: CALL_WRONG_ARITY,
        start_offset: message_span.0,
        end_offset: message_span.1,
        message,
        severity,
        source_family: "builtin",
        receiver_type: Some(class_name.to_string()),
        method_name: Some(method.to_string()),
    })
}

// ---------------------------------------------------------------------------
// call.argument-type-mismatch (ADR-64) — the reference `argument_type_diagnostic`
// (`check_rules.rb:1943`). Ported at the CLASS-name level: rigor-rs has no
// `Inference::Acceptance` engine, so acceptance is decided via the ATM substrate
// (`param_admits_nil` / `param_accepts_arg_class`, both over `class_ordering`)
// exactly as the reference's Slice-2 twins were designed to substitute.
// ---------------------------------------------------------------------------

/// Ruby's universal-equality methods (`Object#==` / `#eql?` / …) accept any
/// object by contract and RETURN false on a type mismatch rather than raise, so
/// a tight RBS param over-specifies — skip them wholesale (reference
/// `UNIVERSAL_EQUALITY_METHODS`, `check_rules.rb:1925`).
fn is_universal_equality_method(method: &str) -> bool {
    matches!(method, "==" | "!=" | "eql?" | "equal?" | "<=>")
}

/// The binary arithmetic / bit / ordering operators that dispatch through Ruby's
/// `coerce` protocol (`5 + Money.new` is valid via `Money#coerce`), so a non-nil
/// argument to them is NOT statically refutable — the MULTI-overload non-nil
/// channel excludes them (reference `COERCE_DISPATCH_METHODS`,
/// `check_rules.rb:1940`). `nil` never coerces, so the nil channel stays in force.
fn is_coerce_dispatch_method(method: &str) -> bool {
    matches!(
        method,
        "+" | "-" | "*" | "/" | "%" | "**" | "&" | "|" | "^" | "<<" | ">>" | "<" | ">" | "<=" | ">="
    )
}

/// An overload is eligible for argument checking iff it has none of the shapes
/// the substrate keeps only as presence flags — rest positionals, any keyword,
/// or trailing positionals (reference `argument_check_eligible?`,
/// `check_rules.rb:2303`).
fn argument_check_eligible(ov: &OverloadSignature) -> bool {
    !ov.has_rest_positionals
        && !ov.has_required_keywords
        && !ov.has_optional_keywords
        && !ov.has_rest_keywords
        && !ov.has_trailing_positionals
}

/// Whether a parameter type is FAITHFULLY translatable to a concrete class check
/// with NO interface degradation — the gate the single-overload non-nil channel
/// needs so it matches the reference's `translate_param_type` → `Acceptance`
/// (which degrades a `type` alias / `interface` param to `untyped` ⇒ skip). A
/// `ClassInstance` (`String`) is faithful; a union of faithful members is; a
/// bare `Alias` (`string`) / `Interface` (`_ToStr`) / `Other` is NOT (the
/// translator would hand the acceptance engine a `Dynamic`, which never refutes,
/// so `"abc".center("s")` — param `int` — stays silent). `Optional` is treated
/// as non-faithful (the substrate's `param_accepts_arg_class` always admits it,
/// so it never fires either way — declining explicitly keeps this honest).
fn is_faithful_param(t: &RetainedParamType) -> bool {
    match t {
        RetainedParamType::ClassInstance(_) => true,
        RetainedParamType::Union(members) => members.iter().all(is_faithful_param),
        RetainedParamType::Alias(_)
        | RetainedParamType::Interface(_)
        | RetainedParamType::Optional(_)
        | RetainedParamType::Other(_) => false,
    }
}

/// The written-form label of a parameter type (reference
/// `param.type.to_s.delete_prefix("::")`), used verbatim in the diagnostic
/// message (presentation only; the harness keys on `(rule, line, column)`).
fn render_retained_param(t: &RetainedParamType) -> String {
    fn strip(n: &str) -> String {
        n.strip_prefix("::").unwrap_or(n).to_string()
    }
    match t {
        RetainedParamType::ClassInstance(n)
        | RetainedParamType::Alias(n)
        | RetainedParamType::Interface(n) => strip(n),
        RetainedParamType::Union(members) => members
            .iter()
            .map(render_retained_param)
            .collect::<Vec<_>>()
            .join(" | "),
        RetainedParamType::Optional(inner) => format!("{}?", render_retained_param(inner)),
        RetainedParamType::Other(s) => strip(s),
    }
}

/// The per-overload written-form label for a multi-overload mismatch: each
/// overload's param at the index rendered, uniq'd in first-seen order, `" | "`
/// joined (reference `overload_param_expected_label`, `check_rules.rb:2213`).
fn expected_label_multi(params: &[&RetainedParamType]) -> String {
    let mut seen: Vec<String> = Vec::new();
    for p in params {
        let label = render_retained_param(p);
        if !seen.contains(&label) {
            seen.push(label);
        }
    }
    seen.join(" | ")
}

/// Whether the argument type is a PURE `nil` (reference `nil_member?` applied to
/// the whole arg type: a `Constant nil` / `Nominal NilClass`, NOT a `T | nil`
/// union). A union-with-nil takes the non-nil translated-acceptance channel.
fn arg_is_pure_nil(
    interner: &Interner,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
    arg_ty: rigor_types::TypeId,
) -> bool {
    concrete_class_name(interner, index, source, arg_ty).as_deref() == Some("NilClass")
}

fn arg_is_dynamic_or_top(interner: &Interner, arg_ty: rigor_types::TypeId) -> bool {
    matches!(interner.get(arg_ty), Type::Dynamic(_) | Type::Top)
}

/// The single concrete RBS-known class an argument types to for the MULTI-overload
/// non-nil channel, or `None` (reference `single_concrete_arg_class?`,
/// `check_rules.rb:2120`): a union arg (deferred), a class/module object
/// (`Singleton`, special acceptance surface), or a non-RBS project class (its
/// duck-typed conversion protocol is invisible) all decline.
fn single_concrete_arg_class(
    interner: &Interner,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
    arg_ty: rigor_types::TypeId,
) -> Option<String> {
    if matches!(interner.get(arg_ty), Type::Union(_) | Type::Singleton(_)) {
        return None;
    }
    let class_name = concrete_class_name(interner, index, source, arg_ty)?;
    if !index.knows_class(&class_name) {
        return None;
    }
    Some(class_name)
}

/// Whether a FAITHFUL parameter provably rejects the (non-nil) argument type —
/// the single-overload non-nil channel's class-level acceptance. Fires iff SOME
/// member of the argument (its union arms, or the arg itself) types to a concrete
/// class the param provably rejects (`class_ordering == Disjoint`, surfaced by
/// `!param_accepts_arg_class` on a faithful param). A `Dynamic`/`Top` member — or
/// a member with no concrete class — is a gradual `maybe`, never a proven
/// rejection, so `d | 42` still fires (on `42`) while `d | e` (both dynamic)
/// stays silent, matching the reference's union acceptance (`.no?` iff ANY member
/// is definitely rejected).
fn faithful_param_rejects_arg(
    interner: &Interner,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
    arg_ty: rigor_types::TypeId,
    param: &RetainedParamType,
) -> bool {
    let members: Vec<rigor_types::TypeId> = match interner.get(arg_ty) {
        Type::Union(ms) => ms.clone(),
        _ => vec![arg_ty],
    };
    members.iter().any(|&m| {
        if arg_is_dynamic_or_top(interner, m) {
            return false;
        }
        match concrete_class_name(interner, index, source, m) {
            Some(class_name) => !index.param_accepts_arg_class(param, &class_name),
            None => false,
        }
    })
}

/// One resolved argument-type mismatch: the argument node to anchor on, the
/// rendered `expected` label, and the argument's `TypeId` for the `got` render.
struct AtmMismatch {
    arg: NodeId,
    /// The declared RBS parameter name (single-overload channel only — the
    /// multi-overload channel matches the reference's `name: nil`, no prefix).
    param_name: Option<&'static str>,
    expected: String,
    actual: rigor_types::TypeId,
}

/// The single-overload channel (reference `first_argument_mismatch` /
/// `single_argument_mismatch`): per positional arg with a matching param, a pure
/// `nil` arg the param rejects (alias-aware `param_admits_nil`) OR a non-nil arg
/// a FAITHFUL param provably rejects.
#[allow(clippy::too_many_arguments)]
fn single_overload_mismatch(
    ov: &OverloadSignature,
    args: &[NodeId],
    ast: &LoweredAst,
    env: &rigor_infer::TypeEnv,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
) -> Option<AtmMismatch> {
    if !argument_check_eligible(ov) {
        return None;
    }
    let params: Vec<&RetainedParamType> = ov
        .required_positionals
        .iter()
        .chain(ov.optional_positionals.iter())
        .collect();
    let names: Vec<Option<&'static str>> = ov
        .required_positional_names
        .iter()
        .chain(ov.optional_positional_names.iter())
        .copied()
        .collect();

    for (i, &arg) in args.iter().enumerate() {
        let Some(param) = params.get(i) else {
            continue; // arity mismatch is the wrong-arity rule's concern.
        };
        let param_name = names.get(i).copied().flatten();
        let arg_ty = typer.type_of(ast, arg, env, interner);

        if arg_is_pure_nil(interner, index, source, arg_ty) {
            if index.param_admits_nil(param) {
                continue;
            }
            return Some(AtmMismatch {
                arg,
                param_name,
                expected: render_retained_param(param),
                actual: arg_ty,
            });
        }

        if arg_is_dynamic_or_top(interner, arg_ty) {
            continue;
        }
        if !is_faithful_param(param) {
            continue;
        }
        if faithful_param_rejects_arg(interner, index, source, arg_ty, param) {
            return Some(AtmMismatch {
                arg,
                param_name,
                expected: render_retained_param(param),
                actual: arg_ty,
            });
        }
    }
    None
}

/// The multi-overload channel (reference `multi_overload_argument_mismatch`,
/// `check_rules.rb:2003`): ALL overloads eligible; per positional index with a
/// param on EVERY overload, a pure `nil` arg NO overload admits (nil channel), or
/// — on a non-coerce method — a single-concrete-class arg NO overload accepts
/// (non-nil channel).
#[allow(clippy::too_many_arguments)]
fn multi_overload_mismatch(
    overloads: &[OverloadSignature],
    method: &str,
    args: &[NodeId],
    ast: &LoweredAst,
    env: &rigor_infer::TypeEnv,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
) -> Option<AtmMismatch> {
    if !overloads.iter().all(argument_check_eligible) {
        return None;
    }
    let coerce = is_coerce_dispatch_method(method);

    for (i, &arg) in args.iter().enumerate() {
        // The param at index `i` on EVERY overload; `None` if any overload lacks
        // one (arity divergence — the wrong-arity rule's concern).
        let params: Option<Vec<&RetainedParamType>> = overloads
            .iter()
            .map(|ov| {
                ov.required_positionals
                    .iter()
                    .chain(ov.optional_positionals.iter())
                    .nth(i)
            })
            .collect();
        let Some(params) = params else {
            continue;
        };

        let arg_ty = typer.type_of(ast, arg, env, interner);

        if arg_is_pure_nil(interner, index, source, arg_ty) {
            if params.iter().any(|p| index.param_admits_nil(p)) {
                continue;
            }
            return Some(AtmMismatch {
                arg,
                param_name: None, // multi-overload: the reference sets name: nil
                expected: expected_label_multi(&params),
                actual: arg_ty,
            });
        } else if !coerce {
            let Some(class_name) = single_concrete_arg_class(interner, index, source, arg_ty)
            else {
                continue;
            };
            if params
                .iter()
                .any(|p| index.param_accepts_arg_class(p, &class_name))
            {
                continue;
            }
            return Some(AtmMismatch {
                arg,
                param_name: None, // multi-overload: the reference sets name: nil
                expected: expected_label_multi(&params),
                actual: arg_ty,
            });
        }
    }
    None
}

/// `call.argument-type-mismatch` for a single receiver-bearing call. Reference
/// `argument_type_diagnostic` (`check_rules.rb:1943`), ported at the class-name
/// level. Gates (zero-FP envelope):
/// - skip the universal-equality methods and any non-plain-positional call;
/// - the receiver must resolve to a concrete class the RBS index models (an
///   instance) OR a `Singleton` class object, and the method must carry retained
///   RBS overloads (so undefined-method / non-RBS methods are the other rules'
///   concern — this never double-fires with undefined-method);
/// - unlike undefined-method / wrong-arity, this does NOT skip when the project
///   also `def`s the method: the RBS sig is the authoritative parameter contract
///   (reference `check_rules.rb:1955`).
#[allow(clippy::too_many_arguments)]
fn check_argument_type_mismatch(
    ast: &LoweredAst,
    receiver: rigor_parse::NodeId,
    method: &str,
    args: &[rigor_parse::NodeId],
    args_all_plain: bool,
    env: &rigor_infer::TypeEnv,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
) -> Option<Diagnostic> {
    if is_universal_equality_method(method) {
        return None;
    }
    if !args_all_plain {
        return None;
    }
    if args.is_empty() {
        return None;
    }

    let source = typer.source();
    let recv_ty = typer.type_of(ast, receiver, env, interner);

    // Resolve `(class_name, overloads)` for INSTANCE or SINGLETON (class-method)
    // dispatch. The overloads are cloned so no `index` borrow lingers across the
    // per-arg `param_admits_nil` / `param_accepts_arg_class` consultations.
    let (class_name, overloads): (String, Vec<OverloadSignature>) =
        if let Type::Singleton(class) = interner.get(recv_ty) {
            let class = *class;
            let name = source.class_name_for_id(class)?;
            let ov = index.singleton_method_overloads(name, method)?;
            (name.to_string(), ov.to_vec())
        } else {
            let name = concrete_class_name(interner, index, source, recv_ty)?;
            if !index.knows_class(&name) {
                return None;
            }
            let ov = index.method_overloads(&name, method)?;
            (name, ov.to_vec())
        };
    if overloads.is_empty() {
        return None;
    }

    let mismatch = if overloads.len() == 1 {
        single_overload_mismatch(&overloads[0], args, ast, env, typer, interner, index, source)
    } else {
        multi_overload_mismatch(&overloads, method, args, ast, env, typer, interner, index, source)
    }?;

    let (start, end) = ast.get(mismatch.arg).span();
    let actual = render_receiver(interner, index, source, mismatch.actual);
    // Reference `build_argument_type_diagnostic` (`check_rules.rb:2322`): a
    // single-overload mismatch names the parameter (``parameter `str' of `m' on
    // C``); the multi-overload channel (name nil) renders the bare method label.
    let method_label = format!("`{method}' on {class_name}");
    let parameter_label = match mismatch.param_name {
        Some(name) => format!("parameter `{name}' of {method_label}"),
        None => method_label,
    };
    let message = format!(
        "argument type mismatch at {parameter_label}: expected {}, got {actual}",
        mismatch.expected
    );
    let severity = catalog(CALL_ARGUMENT_TYPE_MISMATCH)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Error);

    Some(Diagnostic {
        rule_id: CALL_ARGUMENT_TYPE_MISMATCH,
        start_offset: start,
        end_offset: end,
        message,
        severity,
        source_family: "builtin",
        receiver_type: Some(class_name),
        method_name: Some(method.to_string()),
    })
}

/// Apply `flow.always-raises` to a single call with a receiver — a provable
/// Integer `ZeroDivisionError` (the reference's `integer_zero_division?`).
///
/// Zero-false-positive gate (ADR-0023), mirroring the reference exactly. Fire
/// iff ALL hold:
///   1. the method is one of [`INTEGER_RAISING_OPERATORS`] (`/ % div modulo
///      divmod`),
///   2. NO block is attached (a block changes dispatch — decline),
///   3. exactly ONE positional argument is present (the divisor),
///   4. the receiver types to a provably Integer-rooted type — a
///      `Constant[Integer]`, an `IntegerRange`, or `Nominal[Integer]` with no
///      type args (the reference's `integer_rooted_for_diagnostic?`), AND
///   5. that one argument types to a constant Integer `0`
///      (`Constant[Int(0)]`).
///
/// Any other case DECLINES (returns `None`): a Float receiver (`5.0 / 0` —
/// Float division by zero is `Infinity`, not an error), a Float / non-zero /
/// non-constant divisor (`5 / 0.0`, `5 / 2`, `x / y`), a Dynamic/unknown
/// receiver, a block-bearing call, or a multi-arg call. This is the error-
/// severity zero-FP keystone: an FP here would be an ERROR on correct code.
// too_many_arguments: a rule-check fn threading the full typing context (ast, receiver,
// args, span, env, typer, interner, index); bundling into a struct would obscure the call sites.
#[allow(clippy::too_many_arguments)]
fn check_always_raises(
    ast: &LoweredAst,
    receiver: rigor_parse::NodeId,
    method: &str,
    args: &[rigor_parse::NodeId],
    has_block: bool,
    message_span: (usize, usize),
    env: &rigor_infer::TypeEnv,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
) -> Option<Diagnostic> {
    // (1) op set, (2) no block, (3) exactly one positional arg.
    if !INTEGER_RAISING_OPERATORS.contains(&method) {
        return None;
    }
    if has_block {
        return None;
    }
    let [arg] = args else {
        return None; // not exactly one positional arg ⇒ decline.
    };

    // (4) receiver provably Integer-rooted — mirrors the reference's
    // `integer_rooted_for_diagnostic?` (Constant<Integer> | IntegerRange |
    // Nominal[Integer] with no type args). Any other carrier (Float, Dynamic,
    // unknown, a generic Integer subtype application) ⇒ decline.
    let recv_ty = typer.type_of(ast, receiver, env, interner);
    if !is_integer_rooted(interner, index, recv_ty) {
        return None;
    }

    // (5) the divisor types to a constant Integer zero — `Constant[Int(0)]`.
    // A Float `0.0`, a non-zero constant, or any non-constant ⇒ decline.
    let arg_ty = typer.type_of(ast, *arg, env, interner);
    if !matches!(interner.get(arg_ty), Type::Constant(Scalar::Int(0))) {
        return None;
    }

    let message =
        format!("always raises ZeroDivisionError: `{method}' by zero on Integer receiver");
    let severity = catalog(FLOW_ALWAYS_RAISES)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Error);

    Some(Diagnostic {
        rule_id: FLOW_ALWAYS_RAISES,
        start_offset: message_span.0,
        end_offset: message_span.1,
        message,
        severity,
        source_family: "builtin",
        // Not a dispatch-typo rule; the receiver render / method fields are
        // carried for parity with the other call-family diagnostics.
        receiver_type: Some("Integer".to_string()),
        method_name: Some(method.to_string()),
    })
}

/// Whether `ty` is provably Integer-rooted for `flow.always-raises` — the
/// reference's `integer_rooted_for_diagnostic?`: a `Constant` pinned to an
/// Integer value, any `IntegerRange`, or `Nominal[Integer]` with NO type args.
/// Everything else (Float, Dynamic, unknown, applied generics) is NOT
/// Integer-rooted ⇒ the caller declines.
fn is_integer_rooted(interner: &Interner, index: &CoreIndex, ty: rigor_types::TypeId) -> bool {
    match interner.get(ty) {
        // A value-pinned Integer literal (`Constant[Int(5)]`).
        Type::Constant(Scalar::Int(_)) => true,
        // Any bounded Integer range is Integer-rooted (the reference fires on
        // `Type::IntegerRange` unconditionally).
        Type::IntegerRange { .. } => true,
        // `Nominal[Integer]` with NO type args — resolve the class name through
        // the core index (the same surface `class_name_of` uses), so this stays
        // robust to the class id's interning.
        Type::Nominal { class, args } => {
            args.is_empty() && index.class_name_for_id(*class) == Some("Integer")
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// call.raise-non-exception (v0.3.0) — reference `raise_non_exception_diagnostic`
// ---------------------------------------------------------------------------

/// The method names that dispatch to `Kernel#raise` (reference
/// `RAISE_METHOD_NAMES`).
const RAISE_METHOD_NAMES: &[&str] = &["raise", "fail"];

/// Instance types whose nominal class subsumes exception values / class objects,
/// so a "disjoint from Exception" ordering proves nothing about the runtime
/// value (reference `RAISE_UNEXACT_INSTANCE_CLASSES`). Applied ONLY to the
/// instance path — the exact singleton path fires on `raise Object` / `raise
/// Class`.
const RAISE_UNEXACT_INSTANCE_CLASSES: &[&str] = &["Class", "Module", "Object", "BasicObject"];

/// The trinary verdict of the raise-operand check (reference
/// `raise_operand_verdict`): only [`RaiseVerdict::Illegal`] fires.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RaiseVerdict {
    Legal,
    Illegal,
    Unknown,
}

/// Emit `call.raise-non-exception` for every implicit-self `raise`/`fail` whose
/// first positional operand is provably not a legal raise operand. Its OWN walk
/// over `receiver: None` calls (the main call walk is receiver-Some only), NOT
/// toplevel-restricted (fires inside method bodies). A faithful port of the
/// reference `raise_non_exception_diagnostic`.
fn raise_non_exception_diagnostics(
    ast: &LoweredAst,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
    typer: &Typer,
    env: &ScopedEnv,
    interner: &mut Interner,
    out: &mut Vec<Diagnostic>,
) {
    // Collect the candidate calls up front (an owned snapshot) so the immutable
    // AST borrow does not clash with the `&mut interner` the operand typing needs.
    let candidates: Vec<(NodeId, String, NodeId, (usize, usize))> = ast
        .iter()
        .filter_map(|(id, node)| match node {
            Node::Call {
                receiver: None,
                method,
                args,
                block_body,
                message_span,
                first_arg_nonplain,
                ..
            } if RAISE_METHOD_NAMES.contains(&method.as_str())
                && block_body.is_empty()
                && !*first_arg_nonplain =>
            {
                // The first positional argument (bare `raise` / `fail` has none).
                args.first().map(|&arg| (id, method.clone(), arg, *message_span))
            }
            _ => None,
        })
        .collect();

    for (call_id, method, arg, message_span) in candidates {
        // Redefinition gate — a reachable project-side `raise`/`fail`.
        if raise_redefined_in_scope(ast, source, call_id, &method) {
            continue;
        }
        let operand_ty = typer.type_of(ast, arg, env.at(message_span), interner);
        if raise_operand_verdict(interner, index, source, operand_ty) != RaiseVerdict::Illegal {
            continue;
        }
        let rendered = render_receiver(interner, index, source, operand_ty);
        let message = format!(
            "`{method}' operand types as {rendered}, which is not an Exception class, \
             an Exception instance, a String, or an object defining `#exception' \u{2014} \
             this raises TypeError at runtime"
        );
        let severity = catalog(CALL_RAISE_NON_EXCEPTION)
            .map(|e| e.default_severity)
            .unwrap_or(Severity::Error);
        out.push(Diagnostic {
            rule_id: CALL_RAISE_NON_EXCEPTION,
            start_offset: message_span.0,
            end_offset: message_span.1,
            message,
            severity,
            source_family: "builtin",
            // JSON carries `method_name` but no `receiver_type` for this rule.
            receiver_type: None,
            method_name: Some(method),
        });
    }
}

/// The trinary raise-operand verdict (reference `raise_operand_verdict`): a Union
/// recurses per member (all-illegal ⇒ illegal, all-legal ⇒ legal, any mixed ⇒
/// unknown); a `Singleton` takes the exact class path; everything else takes the
/// instance path.
fn raise_operand_verdict(
    interner: &Interner,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
    ty: rigor_types::TypeId,
) -> RaiseVerdict {
    match interner.get(ty) {
        Type::Union(members) => {
            let verdicts: Vec<RaiseVerdict> = members
                .clone()
                .iter()
                .map(|&m| raise_operand_verdict(interner, index, source, m))
                .collect();
            if verdicts.iter().all(|&v| v == RaiseVerdict::Illegal) {
                RaiseVerdict::Illegal
            } else if verdicts.iter().all(|&v| v == RaiseVerdict::Legal) {
                RaiseVerdict::Legal
            } else {
                RaiseVerdict::Unknown
            }
        }
        Type::Singleton(class) => {
            let class = *class;
            let Some(name) = resolve_class_name(index, source, class) else {
                return RaiseVerdict::Unknown;
            };
            raise_class_operand_verdict(index, source, &name)
        }
        _ => raise_instance_operand_verdict(interner, index, source, ty),
    }
}

/// The exact class-object (`Type::Singleton`) verdict (reference
/// `raise_class_operand_verdict`): unknown for a project-discovered class or a
/// non-RBS-known class; else the ordering vs `Exception` decides — `:equal` /
/// `:subclass` legal, `:superclass` OR `:disjoint` illegal unless the singleton
/// defines `#exception` (the duck), `:unknown` silent. NO module exclusion here:
/// `raise Comparable` / `raise Class` / `raise Object` all fire.
fn raise_class_operand_verdict(
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
    class_name: &str,
) -> RaiseVerdict {
    // The most important gate — any project-discovered class bails unconditionally
    // (its RBS-declared ancestry may omit the real superclass; the typer already
    // declines to singleton-type a project class, this is belt-and-braces).
    if source.knows_class(class_name) {
        return RaiseVerdict::Unknown;
    }
    if !index.knows_class(class_name) {
        return RaiseVerdict::Unknown;
    }
    match index.class_ordering(class_name, "Exception") {
        rigor_index::ClassOrdering::Equal | rigor_index::ClassOrdering::Subclass => {
            RaiseVerdict::Legal
        }
        rigor_index::ClassOrdering::Superclass | rigor_index::ClassOrdering::Disjoint => {
            if index.class_has_singleton_method(class_name, "exception") {
                RaiseVerdict::Legal
            } else {
                RaiseVerdict::Illegal
            }
        }
        rigor_index::ClassOrdering::Unknown => RaiseVerdict::Unknown,
    }
}

/// The instance-operand verdict (reference `raise_instance_operand_verdict`):
/// legal when the class is String-family or an Exception descendant; illegal only
/// when the class is fully known, exact enough (not `Class`/`Module`/`Object`/
/// `BasicObject`, not a module), not project-discovered, provably `:disjoint`
/// from both String and Exception, and defines no instance `#exception`.
/// `:superclass` stays UNKNOWN (asymmetric with the singleton path) — a value
/// typed `Object` may well BE an Exception at runtime.
fn raise_instance_operand_verdict(
    interner: &Interner,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
    ty: rigor_types::TypeId,
) -> RaiseVerdict {
    let Some(class_name) = concrete_class_name(interner, index, source, ty) else {
        return RaiseVerdict::Unknown;
    };
    if RAISE_UNEXACT_INSTANCE_CLASSES.contains(&class_name.as_str()) {
        return RaiseVerdict::Unknown;
    }
    if source.knows_class(&class_name) {
        return RaiseVerdict::Unknown;
    }
    if !index.knows_class(&class_name) {
        return RaiseVerdict::Unknown;
    }
    if index.is_module(&class_name) {
        return RaiseVerdict::Unknown;
    }
    match index.class_ordering(&class_name, "String") {
        rigor_index::ClassOrdering::Equal | rigor_index::ClassOrdering::Subclass => {
            return RaiseVerdict::Legal;
        }
        _ => {}
    }
    match index.class_ordering(&class_name, "Exception") {
        rigor_index::ClassOrdering::Equal | rigor_index::ClassOrdering::Subclass => {
            RaiseVerdict::Legal
        }
        rigor_index::ClassOrdering::Disjoint => {
            if index.class_has_method(&class_name, "exception") {
                RaiseVerdict::Legal
            } else {
                RaiseVerdict::Illegal
            }
        }
        // `:superclass` (asymmetric with the singleton path) and `:unknown` stay
        // silent.
        rigor_index::ClassOrdering::Superclass | rigor_index::ClassOrdering::Unknown => {
            RaiseVerdict::Unknown
        }
    }
}

/// The concrete single-class name a NON-singleton operand type dispatches to
/// (reference `concrete_class_name`): `Nominal` its class, `Tuple`→Array,
/// `HashShape`→Hash, `Constant` its value's class, `IntegerRange`→Integer,
/// `Refined`/`Difference` through their base. Everything else (Dynamic / Top /
/// Bottom / unresolvable) is `None` ⇒ the caller declines.
fn concrete_class_name(
    interner: &Interner,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
    ty: rigor_types::TypeId,
) -> Option<String> {
    match interner.get(ty) {
        Type::Nominal { class, .. } => resolve_class_name(index, source, *class),
        Type::Tuple(_) => Some("Array".to_string()),
        Type::HashShape(_) => Some("Hash".to_string()),
        Type::Constant(scalar) => Some(constant_class_name(scalar).to_string()),
        Type::IntegerRange { .. } => Some("Integer".to_string()),
        Type::Refined { base, .. } | Type::Difference { base, .. } => {
            concrete_class_name(interner, index, source, *base)
        }
        _ => None,
    }
}

/// The Ruby core class name of a value-pinned scalar (reference
/// `constant_class_name` / `CONSTANT_CLASSES`).
fn constant_class_name(scalar: &Scalar) -> &'static str {
    match scalar {
        Scalar::Int(_) => "Integer",
        Scalar::Str(_) => "String",
        Scalar::Sym(_) => "Symbol",
        Scalar::Bool(true) => "TrueClass",
        Scalar::Bool(false) => "FalseClass",
        Scalar::Nil => "NilClass",
        Scalar::Float(_) => "Float",
    }
}

/// Resolve a [`rigor_types::ClassId`] to its class name through the core RBS
/// index then the project `sig/` / source registry (same order as
/// [`render_receiver`]).
fn resolve_class_name(
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
    class: rigor_types::ClassId,
) -> Option<String> {
    index
        .class_name_for_id(class)
        .map(str::to_string)
        .or_else(|| source.class_name_for_id(class).map(str::to_string))
}

/// Whether a project-side definition of `raise`/`fail` could shadow Kernel's at
/// this call site (reference `raise_redefined_in_scope?`): a toplevel `def` or an
/// in-source Object/Kernel/BasicObject reopen (both already folded into
/// [`rigor_infer::SourceIndex::is_toplevel_def`]), OR a `def` on the innermost
/// enclosing class — instance OR singleton side (implicit self dispatches to
/// either depending on context; being silent for both is the cheap conservative
/// answer).
fn raise_redefined_in_scope(
    ast: &LoweredAst,
    source: &rigor_infer::SourceIndex,
    call_id: NodeId,
    name: &str,
) -> bool {
    // Covers the toplevel `def raise` and the Object/Kernel/BasicObject reopen
    // (`toplevel_defs` folds both — see `SourceIndex::build_project` pass 1c).
    if source.is_toplevel_def(Some(ast.file_key()), name) {
        return true;
    }
    let call_span = ast.get(call_id).span();
    // The INNERMOST enclosing class/module (smallest span containing the call);
    // its `self` is what a redefined `raise` would resolve against.
    let enclosing = ast
        .iter()
        .filter(|(_, n)| matches!(n, Node::ClassDef { .. } | Node::ModuleDef { .. }))
        .filter(|(_, n)| {
            let s = n.span();
            s.0 <= call_span.0 && call_span.1 <= s.1
        })
        .min_by_key(|(_, n)| {
            let s = n.span();
            s.1 - s.0
        });
    let Some((_, class_node)) = enclosing else {
        return false;
    };
    let body = match class_node {
        Node::ClassDef { body, .. } | Node::ModuleDef { body, .. } => body,
        _ => return false,
    };
    // A DIRECT `def raise` (instance) or `def self.raise` (singleton) in that
    // class body redefines it.
    body.iter().any(|&child| {
        matches!(
            ast.get(child),
            Node::Definition { name: Some(n), .. } if n == name
        ) || matches!(
            ast.get(child),
            Node::Definition { singleton_name: Some(n), .. } if n == name
        )
    })
}

/// Apply `call.possible-nil-receiver` to a single call, firing from the
/// precomputed ADR-0038 Slice-1 snapshot map.
///
/// The FP-delicate flow reasoning (which receiver is certainly `C | nil` and
/// unguarded) lives in [`rigor_infer::Typer::nilable_receiver_snapshots`], which
/// threads the nilability fact straight-line through the program INCLUDING block
/// bodies (the treemaps cluster). Here we only apply the two RBS gates the arm
/// still needs, in order (every `None` is FP-safe):
/// 1. NOT a safe-nav call (`x&.foo` short-circuits on nil ⇒ not a bug). The
///    snapshot pass also skips safe-nav uses; this is a belt-and-braces re-check.
/// 2. The call node is in `snapshots` with a non-nil core arm `C` (the pass
///    proved a certain `C | nil`, unguarded receiver).
/// 3. `method` is ABSENT on `NilClass` (else the call is sound on the nil arm —
///    `to_s`/`to_a`/`inspect`/`nil?`/… live on NilClass and must not fire).
/// 4. `method` is PRESENT on `C` (the non-nil arm defines it — otherwise this is
///    `call.undefined-method`'s job, one diagnostic per call site).
fn check_nil_receiver(
    call_id: rigor_parse::NodeId,
    method: &str,
    message_span: (usize, usize),
    safe_nav: bool,
    snapshots: &std::collections::HashMap<rigor_parse::NodeId, &'static str>,
    index: &CoreIndex,
) -> Option<Diagnostic> {
    // (1) Safe-nav calls short-circuit on nil at runtime ⇒ never a bug.
    if safe_nav {
        return None;
    }
    // (2) The flow pass must have proved a certain `C | nil`, unguarded receiver.
    let core_arm = *snapshots.get(&call_id)?;
    // (3) The method must be ABSENT on NilClass (else sound on the nil arm).
    if index.class_has_method("NilClass", method) {
        return None;
    }
    // (4) The method must be PRESENT on the non-nil arm `C` (else this is
    // `call.undefined-method`'s call, not ours — one diagnostic per site).
    if !index.class_has_method(core_arm, method) {
        return None;
    }
    // Fire. Message is byte-exact with the reference's
    // `build_nil_receiver_diagnostic`: ``possible nil receiver: `m' is undefined
    // on NilClass``. Severity resolves to the catalog default (`error` under
    // balanced — matching the reference's severity_profile).
    let message = format!("possible nil receiver: `{method}' is undefined on NilClass");
    let severity = catalog(CALL_POSSIBLE_NIL_RECEIVER)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Error);
    Some(Diagnostic {
        rule_id: CALL_POSSIBLE_NIL_RECEIVER,
        start_offset: message_span.0,
        end_offset: message_span.1,
        message,
        severity,
        source_family: "builtin",
        receiver_type: None,
        method_name: Some(method.to_string()),
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// flow.dead-assignment (ADR-0030) — pure AST/structural, no typer/index
// ---------------------------------------------------------------------------
//
// Faithful port of `DeadAssignmentCollector` (the reference firing logic) +
// `build_dead_assignment_diagnostic` (the message/severity/name-loc). For one
// method body:
//   1. Gather READ names `R`: every `LocalVariableRead.name`, PLUS every
//      `LocalVariableOpWrite.name` (an op-write reads-then-writes its target —
//      reference `reading_assignment?`), anywhere in the body subtree INCLUDING
//      blocks and string interpolation. Reads do NOT stop at nested defs for the
//      reference (`gather_read_names` has no def barrier) — but a write does, and
//      since we only ever fire on a write found OUTSIDE a nested def, and a name
//      read only inside a nested def cannot suppress an OUTER write that the
//      nested def can't see... we mirror the reference precisely: reads are
//      gathered with NO def barrier (so an inner-def read of an outer local
//      counts as a read — closure capture), writes ARE gathered with a def
//      barrier.
//   2. Gather WRITE candidates `W`: every plain `LocalVariableWrite`, WITHOUT
//      descending into a nested `Definition`/`ClassDef`/`ModuleDef`. Op-writes
//      and multi-writes (lowered to `Other`) are never candidates.
//   3. Trailing statement: the last node of the body list, descending through a
//      `BeginRescue` wrapper's last statement (the reference's
//      `trailing_statement`, which unwraps `StatementsNode`/`BeginNode`).
//   4. Fire iff the write is NOT the trailing statement, its name does NOT start
//      with `_`, and its name is NOT in `R`.

/// Collect every `flow.dead-assignment` diagnostic for one named method body.
///
/// ## Why reads/writes are gathered by SPAN, not structural recursion
///
/// The reference's `gather_read_names`/`gather_write_nodes` recurse the real
/// Prism tree via `compact_child_nodes` — a complete parent->child link. The
/// rigor-rs owned arena is a *lossy* lowering: several Prism nodes (a `return`,
/// `super`, `yield`, a `*splat` arg, …) lower to `Node::Other` and DISCARD their
/// lowered children, orphaning any `LocalVariableRead` underneath. A structural
/// child-walk would miss those reads and FALSELY flag a write that the reference
/// sees as read (a confirmed FP class: `return [entries, policy]`,
/// `super(head: frozen_head)`, `[*rest.map { … }]`).
///
/// The faithful, orphan-proof equivalent: every read/write node STILL lands in
/// the flat arena (lowering is total — only the *link* is lost, not the node),
/// and its byte span lies within the enclosing `def`'s span. So we scan the arena
/// for reads/writes whose span is contained in this def's span. This is exactly
/// the reference's "any read anywhere in the def subtree" set, because the def
/// span delimits precisely that subtree.
///
/// * Reads have NO def barrier in the reference (a read of an outer local inside
///   a nested `def` is a closure capture and counts) — span-containment naturally
///   includes nested-def reads, matching that.
/// * Writes DO have a def barrier (a nested def's writes are its own unit) — so a
///   write is a candidate here only if it is NOT inside any nested
///   def/class/module span that itself sits within this def.
fn dead_assignments_in_def(
    ast: &LoweredAst,
    def_id: rigor_parse::NodeId,
    def_name: &str,
    body: &[rigor_parse::NodeId],
    def_span: rigor_parse::Span,
    param_span: Option<rigor_parse::Span>,
    out: &mut Vec<Diagnostic>,
) {
    // Spans of nested definition units WITHIN this def (the write barrier). A
    // nested def/class/module is one whose span is strictly inside `def_span`
    // (i.e. not this def itself). A write inside any of these belongs to that
    // inner unit, not this one.
    let nested_spans: Vec<rigor_parse::Span> = ast
        .iter()
        .filter_map(|(id, n)| {
            if id == def_id {
                return None;
            }
            match n {
                Node::Definition { span, .. }
                | Node::ClassDef { span, .. }
                | Node::ModuleDef { span, .. }
                    if span_within(*span, def_span) =>
                {
                    Some(*span)
                }
                _ => None,
            }
        })
        .collect();

    // (1) read names — every read/op-write target whose span is within this def
    // (no def barrier). Orphan-proof: the node is in the arena regardless of link.
    let mut reads: HashSet<String> = HashSet::new();
    // (2) write candidates — plain LocalVariableWrites within this def but NOT
    // inside a nested unit.
    let mut writes: Vec<rigor_parse::NodeId> = Vec::new();
    for (id, n) in ast.iter() {
        match n {
            Node::LocalVariableRead { name, span } if span_within(*span, def_span) => {
                reads.insert(name.clone());
            }
            Node::LocalVariableOpWrite { name, span, .. } if span_within(*span, def_span) => {
                // An op-write READS its target (reference `reading_assignment?`).
                reads.insert(name.clone());
            }
            // The PARAMETER LIST is excluded: the reference gathers writes from
            // `def_node.body` only, so a write in a default value (`def f(a, b =
            // (not_set = true))`) is not a candidate there. Parameter defaults
            // are lowered into the arena for the call rules, which is what puts
            // them inside `def_span` at all. Reads are NOT excluded — keeping the
            // extra read names only suppresses more, which stays inside the
            // reference's witness set.
            Node::LocalVariableWrite { span, .. }
                if span_within(*span, def_span)
                    && !param_span.is_some_and(|ps| span_within(*span, ps))
                    && !nested_spans.iter().any(|ns| span_within(*span, *ns)) =>
            {
                writes.push(id);
            }
            _ => {}
        }
    }

    // (3) trailing statement (implicit return — its write is intentional).
    let trailing = trailing_statement(ast, body);

    let severity = catalog(FLOW_DEAD_ASSIGNMENT)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Warning);

    // Emit in source order (writes were collected in arena/source order already).
    for wid in writes {
        let Node::LocalVariableWrite {
            name, name_span, ..
        } = ast.get(wid)
        else {
            continue;
        };
        // (4) the gate.
        if Some(wid) == trailing {
            continue;
        }
        if name.starts_with('_') {
            continue;
        }
        if reads.contains(name) {
            continue;
        }
        out.push(Diagnostic {
            rule_id: FLOW_DEAD_ASSIGNMENT,
            start_offset: name_span.0,
            end_offset: name_span.1,
            message: format!("local `{name}' assigned in `{def_name}' but never read"),
            severity,
            source_family: "builtin",
            receiver_type: None,
            method_name: None,
        });
    }
}

/// Whether `inner` is contained within `outer` (`outer.start <= inner.start` and
/// `inner.end <= outer.end`). Half-open byte spans; equal spans count as within.
fn span_within(inner: rigor_parse::Span, outer: rigor_parse::Span) -> bool {
    outer.0 <= inner.0 && inner.1 <= outer.1
}

/// The file's top-level local env, plus the method-body spans it must NOT reach
/// into.
///
/// The rules walk types every use site against ONE flat env keyed by name, built
/// from the file's TOP-LEVEL writes (`build_toplevel_env` does not descend into
/// `def` bodies). A Ruby method body is an independent local scope, so a name
/// inside a `def` is that def's parameter or its own write — never the top-level
/// local of the same name. Reading the flat env there types the wrong value:
/// rigor-survey `Ruby/data_structures/hash_table/anagram_checker.rb` closes a
/// `s = 'a'` / `t = 'ab'` driver section and then reopens `def is_anagram(s, t)`,
/// and the parameters were typed as those two driver strings — two
/// `call.wrong-arity` and two `call.undefined-method` false positives.
///
/// [`Self::at`] therefore hands a use site inside a method body an EMPTY env.
/// That is a strict loss of information and so cannot add a diagnostic; the
/// bindings it withholds were all wrong. Method-body locals are not typed by
/// this walk at all today (they are absent from the flat env), so nothing that
/// used to fire correctly stops firing.
///
/// The top-level env widens every local a nested construct rebinds
/// (`Typer::build_toplevel_check_env`, rigor-rs#133): the flat binder cannot see
/// a rebind inside an `if`, a loop or a block, nor one on a `next` / `break`
/// path. Widening only ever declines a rule that needs a concrete receiver — but
/// the class-narrowing and collection-shape rules fire ONLY on a `Dynamic`
/// carrier, so for them a widened local would OPEN the gate the stale concrete
/// type closed. They read the unwidened env through [`Self::gate_at`], exactly
/// as before.
struct ScopedEnv {
    top: rigor_infer::TypeEnv,
    gate_top: rigor_infer::TypeEnv,
    empty: rigor_infer::TypeEnv,
    method_bodies: Vec<rigor_parse::Span>,
}

impl ScopedEnv {
    fn build(typer: &Typer, ast: &LoweredAst, interner: &mut Interner) -> Self {
        ScopedEnv {
            top: typer.build_toplevel_check_env(ast, interner),
            gate_top: typer.build_toplevel_env(ast, interner),
            empty: rigor_infer::TypeEnv::new(),
            method_bodies: rigor_infer::method_body_spans(ast),
        }
    }

    /// The env a use site at `span` may read: the top-level env at file scope (or
    /// inside a block, which DOES capture the enclosing locals), an empty env
    /// inside any method body.
    fn at(&self, span: rigor_parse::Span) -> &rigor_infer::TypeEnv {
        if self.in_method_body(span) {
            &self.empty
        } else {
            &self.top
        }
    }

    /// [`Self::at`] for the `Dynamic`-only gates of the class-narrowing and
    /// collection-shape rules: the unwidened top-level env.
    fn gate_at(&self, span: rigor_parse::Span) -> &rigor_infer::TypeEnv {
        if self.in_method_body(span) {
            &self.empty
        } else {
            &self.gate_top
        }
    }

    fn in_method_body(&self, span: rigor_parse::Span) -> bool {
        self.method_bodies.iter().any(|d| span_within(span, *d))
    }
}

/// The trailing statement of a method body: the last id in `body`, descending
/// through a `BeginRescue` / `Statements` wrapper's last statement (mirrors the
/// reference's `trailing_statement`, which unwraps `StatementsNode`/`BeginNode`).
/// `None` for an empty body. A write that IS the trailing statement is an
/// implicit return and is skipped.
fn trailing_statement(ast: &LoweredAst, body: &[rigor_parse::NodeId]) -> Option<rigor_parse::NodeId> {
    let &last = body.last()?;
    descend_trailing(ast, last)
}

fn descend_trailing(ast: &LoweredAst, id: rigor_parse::NodeId) -> Option<rigor_parse::NodeId> {
    match ast.get(id) {
        // A `begin ... end` — its trailing node is the last statement of the
        // protected/rescue/else region, NOT the ensure tail: an `ensure` clause's
        // value is discarded (the reference treats the protected-body tail as the
        // implicit return even when an `ensure` follows it in `body`, where the
        // lowering appends the ensure statements).
        Node::BeginRescue {
            body, ensure_body, ..
        } => match body.iter().rev().find(|id| !ensure_body.contains(id)) {
            Some(&inner) => descend_trailing(ast, inner),
            None => Some(id),
        },
        // The lowered Statements wrapper — its last statement is the real
        // trailing node.
        Node::Statements { body, .. } => match body.last() {
            Some(&inner) => descend_trailing(ast, inner),
            None => Some(id),
        },
        // An explicit `return E` is NOT descended: the reference FIRES
        // `flow.dead-assignment` on `return (x = 5)` (the local binding is
        // pointless even though its value is returned — oracle-probed
        // 2026-07-10), so a write inside a return must NOT get the
        // implicit-return trailing-write skip.
        _ => Some(id),
    }
}

// ---------------------------------------------------------------------------
// `def.ivar-write-mismatch` — faithful port of `IvarWriteCollector` +
// `ivar_mismatch_diagnostics_for` + `ivar_class_for`.
// ---------------------------------------------------------------------------

/// One collected `@x = value` write: the rvalue node to type, the `@x` name-token
/// span the diagnostic anchors on, and the write's byte span (for the rescue-scope
/// lookup of increment a).
struct IvarWrite {
    value: rigor_parse::NodeId,
    name_span: rigor_parse::Span,
    span: rigor_parse::Span,
}

/// A resolved rescue binding in effect over a clause body (increment a): within
/// `clause_span`, a read of `bound_name` types to `exception_class`.
struct RescueBinding {
    clause_span: rigor_parse::Span,
    bound_name: String,
    exception_class: String,
}

/// Collect the resolvable rescue bindings: a single-class `rescue C => e` (whose
/// `C` names a core- or project-known class) binds `e` to `C`; a bare `rescue => e`
/// binds `e` to `StandardError`. A multi-class `rescue A, B => e` binds a union
/// the reference cannot name to a single concrete class, so it is NOT recorded
/// (the write stays silent) — probed against the oracle. An unresolvable exception
/// constant is likewise skipped (a coverage gap, FP-safe).
fn collect_rescue_bindings(
    ast: &LoweredAst,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
) -> Vec<RescueBinding> {
    let mut out = Vec::new();
    for (_, n) in ast.iter() {
        let Node::BeginRescue { clauses, .. } = n else {
            continue;
        };
        for clause in clauses {
            let Some(bound_name) = &clause.bound_name else {
                continue;
            };
            let exception_class = match clause.exceptions.as_slice() {
                // Bare `rescue => e` catches `StandardError`.
                [] => {
                    if index.knows_class("StandardError") {
                        Some("StandardError".to_string())
                    } else {
                        None
                    }
                }
                // A single named class — resolvable via core RBS or the project
                // source registry (`rescue MyError => e` where `MyError` is a
                // discovered project class fires too, probed).
                [only] => match ast.get(*only) {
                    Node::ConstantRead { name, .. }
                        if !name.is_empty()
                            && (index.knows_class(name) || source.knows_class(name)) =>
                    {
                        Some(name.clone())
                    }
                    _ => None,
                },
                // Multi-class arm ⇒ a union with no single concrete class ⇒ silent.
                _ => None,
            };
            if let Some(exception_class) = exception_class {
                out.push(RescueBinding {
                    clause_span: clause.span,
                    bound_name: bound_name.clone(),
                    exception_class,
                });
            }
        }
    }
    out
}

/// The concrete class NAME of one ivar write's rvalue — the `ivar_class_for`
/// analog. Increment (a): a read of a rescue-bound variable inside the clause body
/// resolves to the exception class directly (bypassing the `TypeId` layer, since
/// exception classes are outside the 9-class `Nominal` id space). Otherwise the
/// rvalue is typed through the shared typer against an EMPTY local env (literals
/// and the `Integer()`/`Float()`/`String()` folds are env-independent; any other
/// local read declines to `Dynamic` ⇒ `None`, a coverage gap that can only silence
/// the rule, never mis-fire it) and mapped via `class_name_of`, with
/// `TrueClass`/`FalseClass` folded to `"bool"`.
fn ivar_write_class(
    ast: &LoweredAst,
    write: &IvarWrite,
    typer: &Typer,
    index: &CoreIndex,
    interner: &mut Interner,
    rescue_bindings: &[RescueBinding],
) -> Option<String> {
    if let Node::LocalVariableRead { name, .. } = ast.get(write.value) {
        for binding in rescue_bindings {
            if binding.bound_name == *name && span_within(write.span, binding.clause_span) {
                return Some(binding.exception_class.clone());
            }
        }
    }
    let env = rigor_infer::TypeEnv::new();
    let ty = typer.type_of(ast, write.value, &env, interner);
    match index.class_name_of(interner, ty) {
        Some("TrueClass") | Some("FalseClass") => Some("bool".to_string()),
        Some(name) => Some(name.to_string()),
        None => None,
    }
}

/// Emit every `def.ivar-write-mismatch` diagnostic for one file. Walks each
/// ClassDef/ModuleDef reachable through class/module bodies (a class nested in a
/// `def` is absent from `qualified_class_names`, matching the reference walk that
/// returns at the first `def`), collects its DIRECT instance-`def` bodies' plain
/// `@x = value` writes (barriers at nested def/class/module; singleton `def self.x`
/// and non-instance defs skipped), groups by (qualified class, ivar) in source
/// order, then applies the reference firing logic.
fn ivar_write_mismatch_diagnostics(
    ast: &LoweredAst,
    interner: &mut Interner,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
    typer: &Typer,
    out: &mut Vec<Diagnostic>,
) {
    let qualified = qualified_class_names(ast);

    // Every def/class/module span — the write barriers.
    let barrier_spans: Vec<rigor_parse::Span> = ast
        .iter()
        .filter_map(|(_, n)| match n {
            Node::Definition { span, .. }
            | Node::ClassDef { span, .. }
            | Node::ModuleDef { span, .. } => Some(*span),
            _ => None,
        })
        .collect();

    let rescue_bindings = collect_rescue_bindings(ast, index, source);

    // Gather writes grouped by (qualified class, ivar), preserving first-seen
    // (source) order.
    let mut order: Vec<(String, String)> = Vec::new();
    let mut groups: std::collections::HashMap<(String, String), Vec<IvarWrite>> =
        std::collections::HashMap::new();

    for (class_id, node) in ast.iter() {
        let body = match node {
            Node::ClassDef { body, .. } | Node::ModuleDef { body, .. } => body,
            _ => continue,
        };
        let Some(class_name) = qualified.get(&class_id) else {
            continue; // un-namable / nested-in-def ⇒ never collected.
        };
        for &child_id in body {
            let Node::Definition {
                name: Some(_),
                span: def_span,
                ..
            } = ast.get(child_id)
            else {
                continue; // singleton / non-instance def ⇒ barrier, skip.
            };
            let def_span = *def_span;
            for (_, wn) in ast.iter() {
                let Node::InstanceVariableWrite {
                    name,
                    value,
                    name_span,
                    span,
                } = wn
                else {
                    continue;
                };
                if !span_within(*span, def_span) {
                    continue;
                }
                // Exclude a write inside a nested def/class/module within this def.
                let barriered = barrier_spans.iter().any(|b| {
                    *b != def_span && span_within(*b, def_span) && span_within(*span, *b)
                });
                if barriered {
                    continue;
                }
                let key = (class_name.clone(), name.clone());
                if !groups.contains_key(&key) {
                    order.push(key.clone());
                }
                groups.entry(key).or_default().push(IvarWrite {
                    value: *value,
                    name_span: *name_span,
                    span: *span,
                });
            }
        }
    }

    let severity = catalog(DEF_IVAR_WRITE_MISMATCH)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Warning);

    for (class_name, ivar_name) in &order {
        let writes = &groups[&(class_name.clone(), ivar_name.clone())];
        if writes.len() < 2 {
            continue;
        }
        // The class string of every write (mapped `ivar_class_for`).
        let mut classes: Vec<Option<String>> = Vec::with_capacity(writes.len());
        for w in writes {
            classes.push(ivar_write_class(ast, w, typer, index, interner, &rescue_bindings));
        }

        // Canonical = first write whose class is not "NilClass" (leading `@x = nil`
        // placeholders skipped). If that write's class is unresolvable (`None`),
        // the WHOLE group is silent.
        let Some(canonical) = classes
            .iter()
            .position(|c| c.as_deref() != Some("NilClass"))
        else {
            continue;
        };
        let Some(first_class) = classes[canonical].clone() else {
            continue;
        };

        for i in (canonical + 1)..writes.len() {
            let Some(other_class) = &classes[i] else {
                continue;
            };
            if other_class == "NilClass" || *other_class == first_class {
                continue; // clear-to-nil idiom / same class ⇒ silent.
            }
            let w = &writes[i];
            out.push(Diagnostic {
                rule_id: DEF_IVAR_WRITE_MISMATCH,
                start_offset: w.name_span.0,
                end_offset: w.name_span.1,
                message: format!(
                    "instance variable `{ivar_name}' on {class_name} was previously \
                     assigned {first_class}; this write assigns {other_class}"
                ),
                severity,
                source_family: "builtin",
                receiver_type: None,
                method_name: None,
            });
        }
    }
}

/// Render the receiver for the diagnostic message: the bare literal value for a
/// value-pinned `Constant`, else the resolved class name.
/// Render a receiver for a diagnostic's `message` / `receiver_type` field in the
/// reference's spelling, via the shared `describe_named` display layer: a
/// `Constant` renders its value (`"Hello"`, `3`), a `Tuple` value-pinned
/// (`[1, 2, 3]`), a `Nominal` its class name — resolving class ids through the
/// core RBS index then the project `sig/` registry. Presentation, not contract
/// (ADR-0030); the harness keys diagnostics on `(rule, line, column)`, so the
/// spelling never affects the zero-FP invariant.
fn render_receiver(
    interner: &Interner,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
    ty: rigor_types::TypeId,
) -> String {
    let resolve = |class: rigor_types::ClassId| -> Option<String> {
        index
            .class_name_for_id(class)
            .map(str::to_string)
            .or_else(|| source.class_name_for_id(class).map(str::to_string))
    };
    rigor_types::describe_named(interner, ty, &resolve)
}

/// Render a scalar literal as it appears in the reference's message: strings
/// quoted (`"Hello"`), symbols colon-prefixed (`:foo`), everything else by its
/// natural literal spelling.
fn render_scalar(scalar: &Scalar) -> String {
    match scalar {
        Scalar::Str(s) => format!("{s:?}"),
        Scalar::Sym(s) => format!(":{s}"),
        Scalar::Int(n) => n.to_string(),
        Scalar::Float(f) => f.to_string(),
        Scalar::Bool(b) => b.to_string(),
        Scalar::Nil => "nil".to_string(),
    }
}

// ---------------------------------------------------------------------------
// flow.duplicate-hash-key (v0.3.0) — reference `DuplicateHashKeyCollector`
// ---------------------------------------------------------------------------

/// Emit `flow.duplicate-hash-key` for every LATER occurrence of a repeated
/// value-pinned literal key within one Hash literal (braced or bare kwargs). Walks
/// each `HashLit`'s precomputed `dup_keys` (source order); a `seen` map keyed by
/// the collision tag records the FIRST occurrence, and each later hit fires
/// pointing at the repeat, naming the first's line. The `seen` entry is NOT
/// updated on a hit, so with N≥2 duplicates every later occurrence references the
/// SAME original first occurrence (reference semantics). Each literal is its own
/// scope — nested literals never cross-compare (they are distinct arena nodes).
fn duplicate_hash_key_diagnostics(ast: &LoweredAst, out: &mut Vec<Diagnostic>) {
    for (_id, node) in ast.iter() {
        let Node::HashLit { dup_keys, .. } = node else {
            continue;
        };
        if dup_keys.len() < 2 {
            continue;
        }
        let mut seen: HashMap<&HashKeyTag, u32> = HashMap::new();
        for key in dup_keys {
            match seen.get(&key.tag) {
                Some(&first_line) => out.push(Diagnostic {
                    rule_id: FLOW_DUPLICATE_HASH_KEY,
                    start_offset: key.anchor.0,
                    end_offset: key.anchor.1,
                    message: format!(
                        "duplicate hash key `{}' in the same literal; this entry \
                         overwrites the value first set at line {first_line}",
                        key.label
                    ),
                    severity: Severity::Warning,
                    source_family: "builtin",
                    receiver_type: None,
                    method_name: None,
                }),
                None => {
                    seen.insert(&key.tag, key.line);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// flow.return-in-ensure (v0.3.0) — reference `ReturnInEnsureCollector`
// ---------------------------------------------------------------------------

/// Receiver-less calls whose attached BLOCK opens a new return frame: a `return`
/// inside their block exits the lambda / defined method, not the method whose
/// `ensure` is scanned. `proc` is deliberately ABSENT — `return` inside a `Proc`
/// block returns from the enclosing method, so it stays in scope. Reference
/// `FRAME_BARRIER_CALL_NAMES`.
const FRAME_BARRIER_CALL_NAMES: &[&str] = &["lambda", "define_method"];

/// Emit `flow.return-in-ensure` for every explicit `return` lexically inside an
/// `ensure` clause body. Dispatches on every `BeginRescue` carrying a non-empty
/// `ensure_body` and gathers returns from it with a frame-aware envelope.
fn return_in_ensure_diagnostics(ast: &LoweredAst, out: &mut Vec<Diagnostic>) {
    for (_id, node) in ast.iter() {
        let Node::BeginRescue { ensure_body, .. } = node else {
            continue;
        };
        for &stmt in ensure_body {
            gather_returns_in_ensure(ast, stmt, out);
        }
    }
}

/// Recursively collect offending `return`s under `id`, stopping at frame
/// barriers. Port of the reference `gather_returns` + `gather_returns_around_barrier_block`.
fn gather_returns_in_ensure(ast: &LoweredAst, id: NodeId, out: &mut Vec<Diagnostic>) {
    match ast.get(id) {
        Node::Return { values, span } => {
            out.push(Diagnostic {
                rule_id: FLOW_RETURN_IN_ENSURE,
                start_offset: span.0,
                end_offset: span.1,
                message: "`return' inside `ensure' discards the method's in-flight \
                          return value and swallows any in-flight exception"
                    .to_string(),
                severity: Severity::Warning,
                source_family: "builtin",
                receiver_type: None,
                method_name: None,
            });
            // The reference falls through to descend the return's children.
            for &v in values {
                gather_returns_in_ensure(ast, v, out);
            }
        }
        // A nested `def` / lambda opens a new return frame — a `return` below it
        // exits that inner frame, not the one whose `ensure` we scan.
        Node::Definition { .. } | Node::Lambda { .. } => {}
        // A nested `begin/ensure`: descend the protected/rescue/else statements
        // but NOT its own `ensure` clause — that inner ensure is scanned when its
        // OWN `BeginRescue` is dispatched, so descending here would double-count.
        // The ensure statements also live (duplicated) in `body`, so exclude them.
        Node::BeginRescue { body, ensure_body, .. } => {
            for &child in body {
                if !ensure_body.contains(&child) {
                    gather_returns_in_ensure(ast, child, out);
                }
            }
        }
        // A receiver-less `lambda`/`define_method` call with a block is a barrier:
        // its receiver + args stay in the current frame (and are descended), only
        // the block opens a new one. Every other call (incl. `proc`, plain blocks)
        // is fully descended.
        Node::Call { receiver, method, args, block_body, .. } => {
            let is_barrier = receiver.is_none()
                && FRAME_BARRIER_CALL_NAMES.contains(&method.as_str())
                && !block_body.is_empty();
            if let Some(r) = receiver {
                gather_returns_in_ensure(ast, *r, out);
            }
            for &a in args {
                gather_returns_in_ensure(ast, a, out);
            }
            if !is_barrier {
                for &b in block_body {
                    gather_returns_in_ensure(ast, b, out);
                }
            }
        }
        other => {
            for child in node_children(other) {
                gather_returns_in_ensure(ast, child, out);
            }
        }
    }
}

/// The child node ids of a node (for the generic descent in the return-in-ensure
/// walk). Covers every variant carrying child ids; the barrier/special variants
/// (`Call`/`BeginRescue`/`Return`/`Definition`/`Lambda`) are handled by the caller
/// and never routed here.
fn node_children(node: &Node) -> Vec<NodeId> {
    let mut out = Vec::new();
    match node {
        Node::Program { body, .. }
        | Node::Statements { body, .. }
        | Node::ClassDef { body, .. }
        | Node::ModuleDef { body, .. }
        | Node::Definition { body, .. }
        | Node::Lambda { body, .. }
        | Node::BeginRescue { body, .. } => out.extend(body.iter().copied()),
        Node::LocalVariableWrite { value, .. }
        | Node::LocalVariableOpWrite { value, .. }
        | Node::VariableWrite { value, .. }
        | Node::InstanceVariableWrite { value, .. }
        | Node::ConstantWrite { value, .. } => out.push(*value),
        // A multi-write descends into its RHS *and* the expressions embedded in
        // non-local targets (`obj.attr, b = (return 1), 2` — the `return` can
        // hide in either). Correct descent regardless of whether a probe
        // currently reaches it.
        Node::MultiWrite { value, target_exprs, .. } => {
            out.push(*value);
            out.extend(target_exprs.iter().copied());
        }
        Node::InterpolatedString { parts, .. } | Node::InterpolatedSymbol { parts, .. } => {
            out.extend(parts.iter().copied())
        }
        Node::Call { receiver, args, block_body, .. } => {
            if let Some(r) = receiver {
                out.push(*r);
            }
            out.extend(args.iter().copied());
            out.extend(block_body.iter().copied());
        }
        Node::If { predicate, then_body, else_body, .. } => {
            out.push(*predicate);
            out.extend(then_body.iter().copied());
            out.extend(else_body.iter().copied());
        }
        Node::Case { predicate, branches, else_body, .. } => {
            if let Some(p) = predicate {
                out.push(*p);
            }
            out.extend(branches.iter().copied());
            out.extend(else_body.iter().copied());
        }
        // A `when` clause's children are its conditions then its body — the
        // same id set (and order) the pre-split `BeginRescue` carrier held
        // concatenated in `body`.
        Node::When { conditions, body, .. } => {
            out.extend(conditions.iter().copied());
            out.extend(body.iter().copied());
        }
        Node::Loop { predicate, body, .. } => {
            if let Some(p) = predicate {
                out.push(*p);
            }
            out.extend(body.iter().copied());
        }
        Node::Logical { left, right, .. } => {
            out.push(*left);
            out.push(*right);
        }
        Node::ArrayLit { elements, .. } | Node::HashLit { elements, .. } => {
            out.extend(elements.iter().copied());
        }
        Node::Return { values, .. } => out.extend(values.iter().copied()),
        _ => {}
    }
    out
}

// ---------------------------------------------------------------------------
// static.value-use.void (ADR-100; bleeding-edge `use-of-void-value`)
// ---------------------------------------------------------------------------

/// Collect `static.value-use.void` diagnostics: a call whose author-declared
/// RBS return is `void`, sitting in VALUE context (reference
/// `VoidValueUseCollector`, upstream 8c87f68e). Value context is read top-down
/// from the consumer node — an assignment's RHS (local / ivar / constant
/// writes; the reference also covers cvar / gvar / const-path writes, which
/// rigor-parse lowers opaquely — under-emit), a call's explicit receiver, and
/// a call's positional arguments. A bare-statement void call stays silent.
///
/// The CALLER gates this on the `use-of-void-value` bleeding-edge feature
/// being active — the observable equivalent of the reference's authored
/// `:warning` / profile-`:off` severity resolution. Runs BEFORE
/// `filter_suppressed`, so `# rigor:disable static.value-use.void` works.
#[must_use]
pub fn void_value_use_diagnostics(
    ast: &LoweredAst,
    interner: &mut Interner,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
) -> Vec<Diagnostic> {
    let scopes = rigor_infer::lexical_scopes(ast);
    let typer = Typer::with_source(index, source).with_lexical_scopes(&scopes);
    // Same local-scope discipline as the main walk: a use site inside a method
    // body does not read the file's top-level locals.
    let env = ScopedEnv::build(&typer, ast, interner);
    let mut out = Vec::new();
    for (_id, node) in ast.iter() {
        let span = node.span();
        let scoped = env.at(span);
        match node {
            Node::LocalVariableWrite { value, .. }
            | Node::InstanceVariableWrite { value, .. }
            | Node::ConstantWrite { value, .. } => {
                check_void_value_use(ast, *value, scoped, &typer, interner, index, &mut out);
            }
            Node::Call { receiver, args, .. } => {
                if let Some(recv) = receiver {
                    check_void_value_use(ast, *recv, scoped, &typer, interner, index, &mut out);
                }
                for &arg in args {
                    check_void_value_use(ast, arg, scoped, &typer, interner, index, &mut out);
                }
            }
            _ => {}
        }
    }
    out
}

/// Fire when `value_id` is a receiver-bearing call whose method's declared RBS
/// return is `void` on the DIRECT-dispatch path — the receiver types to a
/// resolvable class (core Nominal, source-range Nominal with an RBS-known
/// name, or a class object for the singleton spelling). Everything else is
/// silent: a literal, a variable read, a non-void call, an unresolvable
/// receiver (never guess).
#[allow(clippy::too_many_arguments)]
fn check_void_value_use(
    ast: &LoweredAst,
    value_id: NodeId,
    env: &rigor_infer::TypeEnv,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
    out: &mut Vec<Diagnostic>,
) {
    let Node::Call { receiver: Some(recv), method, span, .. } = ast.get(value_id) else {
        return;
    };
    let (recv, method, span) = (*recv, method.clone(), *span);
    let recv_ty = typer.type_of(ast, recv, env, interner);
    // Resolve the receiver to (class name, dispatch kind).
    let (class_name, is_singleton) = if let Type::Singleton(class) = interner.get(recv_ty) {
        match typer.source().class_name_for_id(*class) {
            Some(n) => (n.to_string(), true),
            None => return,
        }
    } else if let Some(n) = index.class_name_of(interner, recv_ty) {
        (n.to_string(), false)
    } else if let Some(n) = typer.source().class_name_for_id_of(interner, recv_ty) {
        (n.to_string(), false)
    } else {
        return;
    };
    let is_void = if is_singleton {
        index.singleton_method_is_void(&class_name, &method)
    } else {
        index.method_return_is_void(&class_name, &method)
    };
    if !is_void {
        return;
    }
    // The reference's `VoidOrigin#label`: `Class#method` / `Class.method`.
    let separator = if is_singleton { "." } else { "#" };
    let label = format!("{class_name}{separator}{method}");
    out.push(Diagnostic {
        rule_id: STATIC_VALUE_USE_VOID,
        start_offset: span.0,
        end_offset: span.1,
        message: format!(
            "value use of `void': `{label}' declares `-> void', so its return \
             recovers to `top' and should not be used as a value"
        ),
        severity: Severity::Warning,
        source_family: "builtin",
        receiver_type: None,
        method_name: Some(method),
    });
}

// ---------------------------------------------------------------------------
// suppression.unknown-rule / suppression.empty (v0.3.0)
// ---------------------------------------------------------------------------

/// Produce the `suppression.unknown-rule` / `suppression.empty` surveillance
/// diagnostics for a file's comments (reference `suppression_marker_diagnostics`).
/// MUST be emitted into the same diagnostic list BEFORE `filter_suppressed`, so a
/// marker can suppress its own complaint (`# rigor:disable suppression.unknown-rule`).
/// `comments` is the `(line, start_offset, text)` list from `comment_lines`; every
/// diagnostic anchors at the comment's `#` (`start_offset`), which the CLI resolves
/// to `(line, start_column+1)` exactly like the reference.
#[must_use]
pub fn suppression_marker_diagnostics(comments: &[(usize, usize, String)]) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (_line, offset, text) in comments {
        if let Some(rules) = match_directive(text, "rigor:disable-file") {
            validate_suppression_tokens(rules, "rigor:disable-file", *offset, &mut out);
        } else if let Some(rules) = match_directive(text, "rigor:disable") {
            validate_suppression_tokens(rules, "rigor:disable", *offset, &mut out);
        } else {
            diagnose_bare_suppression_marker(text, *offset, &mut out);
        }
    }
    out
}

/// Validate a matched marker's rule tokens: an empty token list fires
/// `suppression.empty`; each token that is not a known identifier fires
/// `suppression.unknown-rule` (reference `validate_suppression_tokens`).
fn validate_suppression_tokens(raw: &str, marker: &str, offset: usize, out: &mut Vec<Diagnostic>) {
    let tokens: Vec<&str> = raw.split([' ', '\t', ',']).filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() {
        out.push(empty_suppression_diagnostic(marker, offset));
        return;
    }
    for token in tokens {
        if !known_suppression_token(token) {
            out.push(unknown_suppression_rule_diagnostic(marker, token, offset));
        }
    }
}

/// A comment carrying a marker word but not the token-bearing suppression
/// grammar. First the BARE `# rigor:disable[-file]` form: an all-whitespace/comma
/// remainder is a genuinely empty marker (`suppression.empty`); anything else
/// (documentation prose) is left alone. Otherwise the out-of-grammar
/// `suppression.unknown-marker` fallback (`# rigor:disable-next-line`,
/// `# rigor:enable`, ...). Mirrors the reference's `diagnose_bare_suppression_marker`
/// (BARE_SUPPRESSION_MARKER) chaining into `diagnose_unknown_suppression_marker`
/// (UNKNOWN_SUPPRESSION_MARKER). The reference applies each pattern as a whole-
/// string search — BARE has priority — so bare is scanned across every `#` before
/// the unknown fallback is tried at all. The anchor is the comment start (offset),
/// matching the reference's `comment.location.start_column + 1`.
fn diagnose_bare_suppression_marker(text: &str, offset: usize, out: &mut Vec<Diagnostic>) {
    // Pass 1: BARE_SUPPRESSION_MARKER = /#\s*rigor:disable(-file)?(?![\w-])(?<rest>.*)/.
    // The `(-file)?` is greedy but backtracks under the `(?![\w-])` lookahead:
    // `disable-files` / `disable-file-x` fail BARE and drop to the unknown pass.
    for after_rigor in rigor_marker_tails(text) {
        let Some(after_disable) = after_rigor.strip_prefix("disable") else {
            continue;
        };
        if let Some((is_file, rest)) = match_bare_disable(after_disable) {
            if rest.chars().all(is_suppression_ws_or_comma) {
                let marker = if is_file { "rigor:disable-file" } else { "rigor:disable" };
                out.push(empty_suppression_diagnostic(marker, offset));
            }
            return;
        }
    }
    // Pass 2: UNKNOWN_SUPPRESSION_MARKER — an out-of-grammar `rigor:` marker word.
    for after_rigor in rigor_marker_tails(text) {
        if let Some((marker, rest)) = parse_unknown_marker(after_rigor) {
            // Fire only when the remainder is empty/commas or rule-list-shaped, so
            // prose mentioning the spelling (`<rule>`, backticks) stays a comment.
            if rest.chars().all(is_suppression_ws_or_comma) || is_rule_list_shaped(rest) {
                out.push(unknown_suppression_marker_diagnostic(marker, offset));
            }
            return;
        }
    }
}

/// True for an ASCII `[\w-]` byte (word char or hyphen), matching Ruby's default
/// ASCII `\w`. Non-ASCII lead bytes are not word chars, so `(?![\w-])` holds.
fn is_word_or_hyphen_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// True when `s` begins with a `[\w-]` char — the negated `(?![\w-])` lookahead
/// is the complement. An empty string (end-of-input) has no such char, so the
/// lookahead holds there.
fn starts_word_or_hyphen(s: &str) -> bool {
    s.bytes().next().is_some_and(is_word_or_hyphen_byte)
}

/// Byte length of the leading `[\w-]+` run (all ASCII).
fn word_or_hyphen_run_len(s: &str) -> usize {
    s.bytes().take_while(|b| is_word_or_hyphen_byte(*b)).count()
}

/// `[\s,]` — Ruby `\s` (`[ \t\r\n\f\v]`) plus comma.
fn is_suppression_ws_or_comma(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n' | '\u{0c}' | '\u{0b}' | ',')
}

/// Match the BARE `disable(-file)?(?![\w-])` marker against the text after
/// `rigor:disable`, replicating the regex's `(-file)?` backtracking. Returns
/// `(is_file, rest)` on a match, `None` when the marker word runs into a `[\w-]`
/// char (`disable-files`, `disablexyz`) and so is not the bare grammar.
fn match_bare_disable(after_disable: &str) -> Option<(bool, &str)> {
    // Greedy `-file` first, valid only if `(?![\w-])` holds right after it.
    if let Some(after_file) = after_disable.strip_prefix("-file") {
        if !starts_word_or_hyphen(after_file) {
            return Some((true, after_file));
        }
    }
    // Backtrack: no `-file`, then `(?![\w-])` right after `disable`.
    (!starts_word_or_hyphen(after_disable)).then_some((false, after_disable))
}

/// Match `UNKNOWN_SUPPRESSION_MARKER`'s `marker` group against the text after
/// `rigor:` — `disable-<suffix>` (suffix other than `file`) or `enable[-<suffix>]`,
/// each terminated by `(?![\w-])`. Returns `(marker, rest)`.
fn parse_unknown_marker(after_rigor: &str) -> Option<(&str, &str)> {
    // Alt A: `disable-(?!file(?![\w-]))[\w-]+`.
    if let Some(rem) = after_rigor.strip_prefix("disable-") {
        // Negative lookahead: reject a bare `disable-file` (handled by BARE).
        if let Some(after_file) = rem.strip_prefix("file") {
            if !starts_word_or_hyphen(after_file) {
                return None;
            }
        }
        let suffix = word_or_hyphen_run_len(rem);
        if suffix == 0 {
            return None; // `disable-` with no `[\w-]` suffix.
        }
        let end = "disable-".len() + suffix;
        return Some((&after_rigor[..end], &after_rigor[end..]));
    }
    // Alt B: `enable(?:-[\w-]+)?(?![\w-])`.
    if let Some(rem) = after_rigor.strip_prefix("enable") {
        let end = if let Some(after_dash) = rem.strip_prefix('-') {
            let suffix = word_or_hyphen_run_len(after_dash);
            if suffix == 0 {
                return None; // `enable-` then non-`[\w-]`: `(?![\w-])` sees `-`.
            }
            "enable".len() + 1 + suffix
        } else if starts_word_or_hyphen(rem) {
            return None; // `enablexyz` — `(?![\w-])` fails.
        } else {
            "enable".len()
        };
        return Some((&after_rigor[..end], &after_rigor[end..]));
    }
    None
}

/// `\A\s+[\w.,\s-]+\z` — a leading run of whitespace followed by a rule-list-
/// shaped remainder (word / `.` / `,` / whitespace / `-`). Reduces to: the first
/// char is whitespace and every char is in the class, with length ≥ 2.
fn is_rule_list_shaped(rest: &str) -> bool {
    let is_class = |c: char| {
        c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ',' | '-') || is_suppression_ws_or_comma(c)
    };
    let mut chars = rest.chars();
    match chars.next() {
        Some(c) if is_suppression_ws_or_comma(c) => {}
        _ => return false,
    }
    let mut count = 1;
    for c in chars {
        if !is_class(c) {
            return false;
        }
        count += 1;
    }
    count >= 2
}

/// Iterate the tail after each `#\s*rigor:` occurrence in a comment, replicating
/// the `#\s*rigor:` prefix shared by the bare / unknown marker patterns.
fn rigor_marker_tails(text: &str) -> impl Iterator<Item = &str> {
    let bytes = text.as_bytes();
    (0..bytes.len()).filter_map(move |i| {
        if bytes[i] != b'#' {
            return None;
        }
        let mut j = i + 1;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        text[j..].strip_prefix("rigor:")
    })
}

fn unknown_suppression_rule_diagnostic(marker: &str, token: &str, offset: usize) -> Diagnostic {
    Diagnostic {
        rule_id: SUPPRESSION_UNKNOWN_RULE,
        start_offset: offset,
        end_offset: offset,
        message: format!(
            "unknown rule `{token}` in `# {marker}` — the token matches no known rule, \
             alias, or family, so this suppression has no effect. Likely a typo; \
             `rigor explain <rule>` lists the canonical ids."
        ),
        severity: Severity::Warning,
        source_family: "builtin",
        receiver_type: None,
        method_name: None,
    }
}

fn unknown_suppression_marker_diagnostic(marker: &str, offset: usize) -> Diagnostic {
    Diagnostic {
        rule_id: SUPPRESSION_UNKNOWN_MARKER,
        start_offset: offset,
        end_offset: offset,
        message: format!(
            "unrecognised suppression marker `rigor:{marker}` — Rigor's markers are \
             `# rigor:disable <rules>` (suppresses on its own line) and \
             `# rigor:disable-file <rules>`, so this comment suppresses nothing."
        ),
        severity: Severity::Warning,
        source_family: "builtin",
        receiver_type: None,
        method_name: None,
    }
}

fn empty_suppression_diagnostic(marker: &str, offset: usize) -> Diagnostic {
    Diagnostic {
        rule_id: SUPPRESSION_EMPTY,
        start_offset: offset,
        end_offset: offset,
        message: format!(
            "`# {marker}` lists no rules, so this suppression has no effect. Name the \
             rules to suppress (`# {marker} call.undefined-method`) or use `# {marker} all`."
        ),
        severity: Severity::Warning,
        source_family: "builtin",
        receiver_type: None,
        method_name: None,
    }
}

// ---------------------------------------------------------------------------
// In-source diagnostic suppression (reference `filter_suppressed`)
// ---------------------------------------------------------------------------

/// The sentinel rule id of the synthetic internal-error diagnostic emitted on a
/// per-file panic (ADR-0016). Such diagnostics carry no real rule and MUST NEVER
/// be suppressed — they represent failures the user cannot silence away (matches
/// the reference's `rule == nil` guard in `filter_suppressed`).
const INTERNAL_ERROR_RULE: &str = "internal-error";

/// Family-wildcard tokens (`call`, `flow`, …). A token in this set expands to
/// every canonical rule whose id starts with `<token>.` (reference
/// `RULE_FAMILIES`). Only `call` can match an implemented rule today; the rest
/// are carried for forward-compat with the reference's catalogue.
const RULE_FAMILIES: &[&str] = &["call", "flow", "assert", "dump", "def", "suppression", "static"];

/// The canonical rule ids rigor-rs can actually emit. Family expansion and the
/// `disable all` wildcard are checked against this set, so a `call` family token
/// only ever expands to these three (the reference expands against its full
/// `ALL_RULES`, but the extra ids it would add match no rigor-rs diagnostic).
const IMPLEMENTED_RULES: &[&str] = &[
    CALL_UNDEFINED_METHOD,
    CALL_WRONG_ARITY,
    CALL_ARGUMENT_TYPE_MISMATCH,
    CALL_POSSIBLE_NIL_RECEIVER,
    FLOW_DEAD_ASSIGNMENT,
    DEF_OVERRIDE_VISIBILITY_REDUCED,
    FLOW_ALWAYS_RAISES,
    FLOW_UNREACHABLE_BRANCH,
    FLOW_ALWAYS_TRUTHY_CONDITION,
    FLOW_DUPLICATE_HASH_KEY,
    FLOW_RETURN_IN_ENSURE,
    CALL_RAISE_NON_EXCEPTION,
    FLOW_SHADOWED_RESCUE_CLAUSE,
    SUPPRESSION_UNKNOWN_RULE,
    SUPPRESSION_EMPTY,
    SUPPRESSION_UNKNOWN_MARKER,
    DEF_IVAR_WRITE_MISMATCH,
    STATIC_VALUE_USE_VOID,
];

/// The canonical rule ids rigor-rs can actually emit — the implemented coverage
/// scope, a SOUND SUBSET of the reference's catalogue (ADR-0008). Reported by
/// `rigor doctor` so users know which rules are live.
pub fn implemented_rules() -> &'static [&'static str] {
    IMPLEMENTED_RULES
}

/// The reference's FULL `ALL_RULES` canonical catalogue (all 19 built-in ids,
/// `check_rules.rb` lines 58–76). Deliberately BROADER than [`IMPLEMENTED_RULES`]:
/// the config audit ([`is_inert_builtin_token`]) uses it to decide whether a
/// `disable:`/`severity_overrides:` token names a real rule, so it must never
/// flag an id the reference recognizes — even one rigor-rs does not yet emit.
const ALL_CANONICAL_RULES: &[&str] = &[
    "call.undefined-method",
    "call.self-undefined-method",
    "call.unresolved-toplevel",
    "call.wrong-arity",
    "call.argument-type-mismatch",
    "call.possible-nil-receiver",
    "call.raise-non-exception",
    "dump.type",
    "assert.type-mismatch",
    "flow.always-raises",
    "flow.unreachable-branch",
    "def.return-type-mismatch",
    "def.method-visibility-mismatch",
    "def.override-visibility-reduced",
    "def.override-return-widened",
    "def.override-param-narrowed",
    "def.ivar-write-mismatch",
    "flow.dead-assignment",
    "flow.always-truthy-condition",
    "flow.unreachable-clause",
    // v0.3.0 ids. `flow.duplicate-hash-key` / `flow.return-in-ensure` /
    // `call.raise-non-exception` / `flow.shadowed-rescue-clause` /
    // `suppression.unknown-rule` / `suppression.empty` /
    // `suppression.unknown-marker` are all implemented.
    "flow.duplicate-hash-key",
    "flow.return-in-ensure",
    "flow.shadowed-rescue-clause",
    "suppression.unknown-rule",
    "suppression.empty",
    "suppression.unknown-marker",
    "static.value-use.void",
];

/// True when `token` looks like a built-in-family rule id but matches none — its
/// first `.`-segment is a built-in family (`call`/`flow`/`assert`/`dump`/`def`)
/// yet it is neither the bare family wildcard nor a known canonical id, so it is
/// a likely typo whose `disable:`/`severity_overrides:` entry has no effect.
///
/// A faithful port of `ConfigAudit#inert_builtin_token?`. A token whose family is
/// NOT built-in (a plugin / `rbs_extended.*` rule, or a bare legacy alias like
/// `undefined-method`) is deliberately never flagged — it may resolve at run
/// time, so under-warning is the FP-safe choice. Validated against the full
/// reference [`ALL_CANONICAL_RULES`], not the narrower [`IMPLEMENTED_RULES`].
#[must_use]
pub fn is_inert_builtin_token(token: &str) -> bool {
    let family = token.split('.').next().unwrap_or(token);
    if !RULE_FAMILIES.contains(&family) {
        return false;
    }
    if token == family {
        return false;
    }
    !ALL_CANONICAL_RULES.contains(&token)
}

/// Maps a legacy short alias to its canonical id (reference `LEGACY_RULE_ALIASES`).
/// Only the three implemented ids can ever match a real diagnostic; the remaining
/// aliases are included for forward-compat (they expand to ids no rigor-rs
/// diagnostic carries, so they are inert).
fn legacy_alias(token: &str) -> Option<&'static str> {
    match token {
        "undefined-method" => Some(CALL_UNDEFINED_METHOD),
        "self-undefined-method" => Some("call.self-undefined-method"),
        "wrong-arity" => Some(CALL_WRONG_ARITY),
        "argument-type-mismatch" => Some("call.argument-type-mismatch"),
        "possible-nil-receiver" => Some(CALL_POSSIBLE_NIL_RECEIVER),
        "dump-type" => Some("dump.type"),
        "assert-type" => Some("assert.type-mismatch"),
        "always-raises" => Some("flow.always-raises"),
        "unreachable-branch" => Some("flow.unreachable-branch"),
        "method-visibility-mismatch" => Some("def.method-visibility-mismatch"),
        "ivar-write-mismatch" => Some("def.ivar-write-mismatch"),
        "dead-assignment" => Some("flow.dead-assignment"),
        "always-truthy-condition" => Some("flow.always-truthy-condition"),
        "unreachable-clause" => Some("flow.unreachable-clause"),
        "raise-non-exception" => Some("call.raise-non-exception"),
        "duplicate-hash-key" => Some(FLOW_DUPLICATE_HASH_KEY),
        "return-in-ensure" => Some(FLOW_RETURN_IN_ENSURE),
        "shadowed-rescue-clause" => Some("flow.shadowed-rescue-clause"),
        _ => None,
    }
}

/// Families of diagnostics the engine emits OUTSIDE the check-rule catalogue
/// (aggregator/reporter-level: `rbs_extended.*`, `dynamic.*`, `rbs.*`,
/// `pre-eval.*`), plus the `plugin.` prefix reserved for plugin-produced ids. A
/// suppression token whose first `.`-segment is one of these is treated as KNOWN
/// (under-warning is the FP-safe direction — these ids load dynamically / live in
/// the engine-heavy runner and cannot be enumerated here). Reference
/// `NON_CHECK_DIAGNOSTIC_FAMILIES`.
const NON_CHECK_DIAGNOSTIC_FAMILIES: &[&str] =
    &["rbs_extended", "dynamic", "rbs", "pre-eval", "plugin"];

/// Bare (dot-less) diagnostic ids the engine emits outside the catalogue. A token
/// equal to one of these is KNOWN even without a family prefix. Reference
/// `NON_CHECK_DIAGNOSTIC_IDS`.
const NON_CHECK_DIAGNOSTIC_IDS: &[&str] = &[
    "configuration-error",
    "load-error",
    "pool-degraded",
    "runtime-error",
    "source-rbs-synthesis-failed",
];

/// True when a suppression token resolves to a diagnostic identifier some producer
/// can emit: the `all` wildcard, a canonical check-rule id (the FULL
/// [`ALL_CANONICAL_RULES`], not just the emitted subset), a legacy alias, a family
/// wildcard, a bare non-catalogue engine id, or a dotted id under a known
/// non-check family (`plugin.*` is always known). A faithful port of the
/// reference's `known_suppression_token?`. Used by `suppression.unknown-rule`.
#[must_use]
pub fn known_suppression_token(token: &str) -> bool {
    if token == "all" {
        return true;
    }
    if ALL_CANONICAL_RULES.contains(&token)
        || legacy_alias(token).is_some()
        || RULE_FAMILIES.contains(&token)
        || NON_CHECK_DIAGNOSTIC_IDS.contains(&token)
    {
        return true;
    }
    // A dotted id whose family is a known non-check family (`plugin.foo`, …).
    matches!(token.split_once('.'), Some((family, _)) if NON_CHECK_DIAGNOSTIC_FAMILIES.contains(&family))
}

/// A parsed suppression set: a flag for the `all` wildcard plus the explicit
/// canonical rule ids. Mirrors the reference's `Set` that may contain the
/// `"all"` sentinel alongside real ids.
///
/// This is the single source of truth for rule-token expansion (legacy aliases,
/// the `call`/`flow`/… family wildcards, canonical ids, and the `all` wildcard).
/// It backs BOTH in-source `# rigor:disable` suppression and the `.rigor.yml`
/// `disable:` config key, so the two stay in lockstep.
#[derive(Default, Clone)]
pub struct SuppressSet {
    all: bool,
    rules: HashSet<String>,
}

impl SuppressSet {
    /// Build a set from a list of user-supplied rule tokens (e.g. a config
    /// `disable:` list), expanding each through the same logic as inline
    /// `# rigor:disable` directives. The internal-error sentinel can never be
    /// matched here — even an explicit `internal-error`/`all` token leaves it
    /// reportable (enforced by [`SuppressSet::suppresses`]).
    #[must_use]
    pub fn from_tokens<S: AsRef<str>>(tokens: &[S]) -> Self {
        let mut set = Self::default();
        for token in tokens {
            set.absorb_token(token.as_ref());
        }
        set
    }

    /// Whether this set matches `rule` (so the diagnostic should be dropped). The
    /// `internal-error` sentinel is NEVER matched, regardless of `all` or an
    /// explicit token — it represents a failure the user cannot silence (reference
    /// `rule == nil` guard).
    #[must_use]
    pub fn suppresses(&self, rule: &str) -> bool {
        if rule == INTERNAL_ERROR_RULE {
            return false;
        }
        self.all || self.rules.contains(rule)
    }

    fn is_empty(&self) -> bool {
        !self.all && self.rules.is_empty()
    }

    /// Expand one user token into this set (reference `expand_token` +
    /// `absorb_suppression_tokens`).
    fn absorb_token(&mut self, token: &str) {
        if token == "all" {
            self.all = true;
        } else if let Some(canonical) = legacy_alias(token) {
            self.rules.insert(canonical.to_string());
        } else if RULE_FAMILIES.contains(&token) {
            let prefix = format!("{token}.");
            for rule in IMPLEMENTED_RULES {
                if rule.starts_with(&prefix) {
                    self.rules.insert((*rule).to_string());
                }
            }
        } else {
            // Canonical id → itself; unknown token → passes through verbatim
            // (matches no real diagnostic ⇒ a no-op). Both paths just insert
            // the token, matching the reference's `expand_token` fallthrough.
            self.rules.insert(token.to_string());
        }
    }
}

/// Drop the diagnostics suppressed by the file's inline `# rigor:disable` /
/// `# rigor:disable-file` comments, mirroring the reference's `filter_suppressed`
/// (honored regardless of any config file). Each input is `(line, diagnostic)`
/// where `line` is the diagnostic's 1-based source line; `comments` is the
/// `(line, text)` list from [`rigor_parse::comment_lines`].
///
/// A diagnostic is dropped iff its `rule_id` is in the file-suppression set (or
/// that set contains `all`), OR its `rule_id` is in its line's suppression set
/// (or that line's set contains `all`). The internal-error sentinel is never
/// dropped.
#[must_use]
pub fn filter_suppressed(
    diagnostics: Vec<(usize, Diagnostic)>,
    comments: &[(usize, usize, String)],
) -> Vec<(usize, Diagnostic)> {
    let (line_suppressions, file_suppressions) = parse_suppression_comments(comments);

    diagnostics
        .into_iter()
        .filter(|(line, diag)| {
            // Never suppress the internal-error sentinel (reference: `rule.nil?`).
            if diag.rule_id == INTERNAL_ERROR_RULE {
                return true;
            }
            if file_suppressions.suppresses(diag.rule_id) {
                return false;
            }
            if let Some(set) = line_suppressions.get(line) {
                if set.suppresses(diag.rule_id) {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// Parse the comment list into `(line_suppressions, file_suppressions)`.
/// File-level directives (`# rigor:disable-file ...`) apply to every line; the
/// `-file` form is checked FIRST so a `disable-file` comment is not also read as
/// a line-level `disable` (the reference's `(?!-file)` negative lookahead).
fn parse_suppression_comments(
    comments: &[(usize, usize, String)],
) -> (HashMap<usize, SuppressSet>, SuppressSet) {
    let mut line_suppressions: HashMap<usize, SuppressSet> = HashMap::new();
    let mut file_suppressions = SuppressSet::default();

    for (line, _offset, text) in comments {
        if let Some(rules) = match_directive(text, "rigor:disable-file") {
            absorb_tokens(rules, &mut file_suppressions);
        } else if let Some(rules) = match_directive(text, "rigor:disable") {
            absorb_tokens(rules, line_suppressions.entry(*line).or_default());
        }
    }

    (line_suppressions, file_suppressions)
}

/// Find `#` `<ws>*` `<keyword>` `<ws>+` in `text` and return the rule-token tail
/// (everything after the keyword's trailing whitespace). Hand-rolled equivalent
/// of the reference's `/#\s*<keyword>\s+(?<rules>[\w.,\s-]+)/` (the `regex` crate
/// is a cached dep, but the patterns are simple enough to scan directly and avoid
/// pulling it into this crate). Returns `None` when the directive is absent or
/// has no whitespace-separated tail.
///
/// For the `rigor:disable` keyword the caller has already tried `disable-file`
/// first, which is how the reference's `(?!-file)` lookahead is honored: a
/// `disable-file` comment matches the `-file` branch and never reaches here.
fn match_directive<'a>(text: &'a str, keyword: &str) -> Option<&'a str> {
    let hash = text.find('#')?;
    let mut rest = &text[hash + 1..];
    // `#\s*`
    rest = rest.trim_start_matches([' ', '\t']);
    let after_kw = rest.strip_prefix(keyword)?;
    // `\s+` — at least one whitespace must follow the keyword.
    let trimmed = after_kw.trim_start_matches([' ', '\t']);
    if trimmed.len() == after_kw.len() {
        return None;
    }
    Some(trimmed)
}

/// Split the rule-token tail on whitespace/commas and absorb each token,
/// matching the reference's `raw.split(/[\s,]+/)`. The reference's `[\w.,\s-]+`
/// capture stops at the first character outside that class; tokens here are split
/// on the same delimiters, and any token is absorbed verbatim, so a trailing
/// non-rule word is simply an unknown token (a no-op).
fn absorb_tokens(tail: &str, target: &mut SuppressSet) {
    for token in tail.split([' ', '\t', ',']) {
        if !token.is_empty() {
            target.absorb_token(token);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;

#[cfg(test)]
mod void_value_use_tests;

#[cfg(test)]
mod rbs_tuple_witness_tests;
