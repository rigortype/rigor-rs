//! Diagnostic rules + the structured `Diagnostic` type (ADR-0014: rule id,
//! severity, primary/secondary annotations, subdiagnostics). All rules run in a
//! single converged AST walk (ADR-0005), not one pass per rule. The tracer
//! bullet's first rule is `call.undefined-method`.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use rigor_index::CoreIndex;
use rigor_infer::Typer;
use rigor_parse::{LoweredAst, Node, NodeId};
use rigor_types::{Interner, Scalar, Type};

mod shadowed_rescue;
pub use shadowed_rescue::shadowed_rescue_diagnostics;

pub mod dead_version_guard;
mod call_toplevel;
mod void_value_use;
mod suppression_markers;
mod call_receiver;
mod call_arguments;
mod flow;
pub use dead_version_guard::{
    filter_dead_version_guard_arms, filter_dead_version_guard_arms_with, RubyRuntime,
};
pub(crate) use call_toplevel::*;
pub use void_value_use::void_value_use_diagnostics;
pub use suppression_markers::suppression_marker_diagnostics;
pub(crate) use call_receiver::*;
pub(crate) use call_arguments::*;
pub(crate) use flow::*;

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
