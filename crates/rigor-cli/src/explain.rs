//! `rigor explain <rule>` (ADR-0015) — the read-only rule-catalogue probe.
//!
//! Mirrors the reference's `Rigor::CLI::ExplainCommand` + `Analysis::RuleCatalog`:
//! it prints the per-rule metadata (summary, severity-by-profile, fires-when /
//! does-not-fire-when, suppression, docs URL) for one canonical rule id, a legacy
//! alias, or a family wildcard (`call`/`flow`/`assert`/`dump`/`def`). With no
//! argument it lists every rule's id + one-line summary.
//!
//! The catalogue here is a static table mirroring the reference's
//! `RuleCatalog::ENTRIES` *content* verbatim — `explain` exposes existing
//! capability (the rule metadata) and does not touch analysis, so faithful
//! parity is a copy of the reference's authored strings. The command is
//! read-only: no parser, no analyzer, no I/O beyond the rendered catalogue.

use std::process::ExitCode;

/// One catalogue entry — the metadata `rigor explain` renders for a rule.
/// Field set mirrors the reference's `RuleCatalog::Entry`.
struct Entry {
    /// Canonical rule id (`call.undefined-method`).
    id: &'static str,
    /// Single-line headline.
    summary: &'static str,
    /// Conditions that trigger the rule (rendered under "Fires when:").
    fires_when: &'static [&'static str],
    /// Cases the rule intentionally skips ("Does not fire when:").
    does_not_fire_when: &'static [&'static str],
    /// Short note on how to suppress.
    suppression: &'static str,
    /// The severity the rule emits with (`error`/`warning`/`info`).
    severity_authored: &'static str,
    /// `(profile, severity)` for each of lenient / balanced / strict.
    severity_by_profile: [(&'static str, &'static str); 3],
    /// Confidence tier (`high`/`medium`/`low`), or `None` for an informational
    /// helper (`dump.type`).
    evidence_tier: Option<&'static str>,
    /// First version the rule shipped in.
    since: &'static str,
}

impl Entry {
    /// The published diagnostics-catalogue URL anchored at this rule (mirrors
    /// `RuleCatalog.documentation_url`: dots in the id become dashes).
    fn documentation_url(&self) -> String {
        format!("{DOCUMENTATION_BASE}#rule-{}", self.id.replace('.', "-"))
    }

    /// Legacy aliases that resolve to this canonical id (the reverse of
    /// `LEGACY_RULE_ALIASES`).
    fn aliases(&self) -> Vec<&'static str> {
        LEGACY_RULE_ALIASES
            .iter()
            .filter(|(_, canonical)| *canonical == self.id)
            .map(|(legacy, _)| *legacy)
            .collect()
    }
}

/// Stable documentation home for a built-in rule (mirrors the reference's
/// `RuleCatalog::DOCUMENTATION_BASE`).
const DOCUMENTATION_BASE: &str =
    "https://github.com/rigortype/rigor/blob/main/docs/manual/04-diagnostics.md";

/// Family wildcard tokens — a bare `<family>` resolves to every rule under
/// `<family>.` (reference `RULE_FAMILIES`).
const RULE_FAMILIES: &[&str] = &["call", "flow", "assert", "dump", "def", "suppression"];

/// Legacy unprefixed rule ids → canonical id (reference `LEGACY_RULE_ALIASES`).
const LEGACY_RULE_ALIASES: &[(&str, &str)] = &[
    ("undefined-method", "call.undefined-method"),
    ("self-undefined-method", "call.self-undefined-method"),
    ("wrong-arity", "call.wrong-arity"),
    ("argument-type-mismatch", "call.argument-type-mismatch"),
    ("possible-nil-receiver", "call.possible-nil-receiver"),
    ("raise-non-exception", "call.raise-non-exception"),
    ("dump-type", "dump.type"),
    ("assert-type", "assert.type-mismatch"),
    ("always-raises", "flow.always-raises"),
    ("unreachable-branch", "flow.unreachable-branch"),
    ("method-visibility-mismatch", "def.method-visibility-mismatch"),
    ("ivar-write-mismatch", "def.ivar-write-mismatch"),
    ("dead-assignment", "flow.dead-assignment"),
    ("always-truthy-condition", "flow.always-truthy-condition"),
    ("unreachable-clause", "flow.unreachable-clause"),
    ("duplicate-hash-key", "flow.duplicate-hash-key"),
    ("return-in-ensure", "flow.return-in-ensure"),
    ("shadowed-rescue-clause", "flow.shadowed-rescue-clause"),
];

/// The full rule catalogue — content mirrors the reference's
/// `RuleCatalog::ENTRIES` verbatim (every authored string), so `explain`
/// prints the same per-rule reference the docs site publishes.
const ENTRIES: &[Entry] = &[
    Entry {
        id: "call.undefined-method",
        summary: "Method does not exist on the receiver's statically-known class.",
        fires_when: &[
            "The call is `receiver.method(...)` with an explicit receiver.",
            "The receiver type resolves to `Type::Nominal` / `Singleton` / `Constant` / `Tuple` / `HashShape`.",
            "The receiver class is RBS-known (declared in the loaded environment).",
            "The user has not declared the method via `def` or recognised `define_method`.",
            "Neither the receiver class nor an ancestor's RBS sig declares the method.",
        ],
        does_not_fire_when: &[
            "Implicit-self calls (no receiver) — too noisy without per-method RBS for every helper.",
            "Receiver is `Dynamic[T]` / `Top` / `Union` — by definition the method set isn't enumerable.",
            "Receiver class is in the loader but its RBS definition cannot be built (constant aliases).",
        ],
        suppression: "`# rigor:disable call.undefined-method` on the call line, or `disable: [\"call.undefined-method\"]` in `.rigor.yml`.",
        severity_authored: "error",
        severity_by_profile: [("lenient", "error"), ("balanced", "error"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.0.1",
    },
    Entry {
        id: "call.self-undefined-method",
        summary: "Implicit-self call resolves to no method on a confidently-closed class.",
        fires_when: &[
            "The call is an implicit-self call (no explicit receiver) inside a class body.",
            "The engine's own resolution (RBS dispatch + the user-class ancestor walk) found nothing.",
            "The enclosing class is a STANDALONE project class: no superclass and no `include`/`prepend`.",
            "It defines no `method_missing` and no dynamic `attr_*(*splat)` accessor.",
            "It is not a plugin-declared open receiver (ADR-26).",
        ],
        does_not_fire_when: &[
            "The enclosing scope is a `module` (a mixin contract — methods may come from includers).",
            "The class has a superclass or mixes in a module (surface extends beyond this file — a later slice).",
            "`self` is `Dynamic` / top-level (the gradual guarantee), or the method exists via any project signal.",
            "Off in every shipped profile pending the external corpus FP gate — opt in via `severity_overrides:`.",
        ],
        suppression: "`# rigor:disable call.self-undefined-method`, or enable/disable via `severity_overrides: { call.self-undefined-method: warning }` in `.rigor.yml`.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "off"), ("balanced", "off"), ("strict", "off")],
        evidence_tier: Some("low"),
        since: "0.1.17",
    },
    Entry {
        id: "call.unresolved-toplevel",
        summary: "Top-level implicit-self call resolves against no def, pre_eval: patch, or Kernel method.",
        fires_when: &[
            "The call is an implicit-self call (no receiver) at top level (outside any class / module body).",
            "Its name resolves against no same-file top-level `def`.",
            "No ADR-17 `pre_eval:` monkey-patch on `Object` / `Kernel` declares it.",
            "It is not a standard `Kernel` / `Object` private method (`puts`, `require`, `loop`, …).",
        ],
        does_not_fire_when: &[
            "The call has an explicit receiver, or sits inside a `def` / `class` / `module` body (ADR-24 WD3 stays lenient there).",
            "A project file defines the name via a top-level `def` or an Object/Kernel monkey-patch listed in `.rigor.yml`'s `pre_eval:` (ADR-17).",
            "The name is a Kernel/Object method visible in the loaded RBS environment.",
        ],
        suppression: "`# rigor:disable call.unresolved-toplevel` on the call line, or list the defining file in `.rigor.yml`'s `pre_eval:` so the analyzer sees the top-level `def` / patch.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "off"), ("balanced", "warning"), ("strict", "error")],
        evidence_tier: Some("low"),
        since: "0.1.14",
    },
    Entry {
        id: "call.wrong-arity",
        summary: "Call's positional argument count is outside the declared overloads' envelope.",
        fires_when: &[
            "Call is `receiver.method(args...)` with explicit receiver + plain positional args.",
            "Receiver class is RBS-known and the method has a definition.",
            "Actual positional count is below the min or above the max across all overloads.",
        ],
        does_not_fire_when: &[
            "Call uses `*splat`, keyword arguments, block-pass, or forwarded arguments.",
            "Method declares required keyword arguments (caller must pass kwargs the rule doesn't model).",
            "Method has a `*rest` positional parameter (max arity is unbounded).",
        ],
        suppression: "`# rigor:disable call.wrong-arity`.",
        severity_authored: "error",
        severity_by_profile: [("lenient", "error"), ("balanced", "error"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.0.1",
    },
    Entry {
        id: "call.argument-type-mismatch",
        summary: "Call passes an argument whose type the parameter cannot accept.",
        fires_when: &[
            "The parameter type rejects the argument under `accepts(arg, mode: :gradual)`.",
            "Single-overload: no overload accepts the arg class (ADR-64 non-nil channel).",
            "Multi-overload: every overload rejects a pure-`nil` arg (ADR-64 nil channel) or every overload rejects a single concrete non-nil arg class (non-nil channel).",
            "Both sides have a non-Dynamic concrete type.",
        ],
        does_not_fire_when: &[
            "Either the parameter or the argument is `Dynamic[T]`.",
            "The call is a coerce-dispatch operator (`+`, `-`, `*`, `/`, `<`, `>`, …) — excluded because the `coerce` protocol makes acceptance undecidable.",
            "Method has `*rest_positionals`, required keywords, or trailing positionals.",
            "The argument type is a union (not a single concrete class).",
        ],
        suppression: "`# rigor:disable call.argument-type-mismatch`.",
        severity_authored: "error",
        severity_by_profile: [("lenient", "warning"), ("balanced", "error"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.0.2",
    },
    Entry {
        id: "call.possible-nil-receiver",
        summary: "Receiver may be nil and the method is not defined on NilClass.",
        fires_when: &[
            "Receiver type is `Type::Union` containing `Constant<nil>` (or `nil` from the RBS Optional).",
            "The non-nil branch has the method, but `NilClass` does not.",
            "Call is not safe-navigation (`x&.method`).",
        ],
        does_not_fire_when: &[
            "Method exists on every member of the union (including NilClass).",
            "Receiver was narrowed via `return if x.nil?` / similar early-return guard.",
            "Call uses safe-navigation (`x&.method`).",
        ],
        suppression: "`# rigor:disable call.possible-nil-receiver`.",
        severity_authored: "error",
        severity_by_profile: [("lenient", "warning"), ("balanced", "error"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.0.2",
    },
    Entry {
        id: "dump.type",
        summary: "`dump_type(expr)` from Rigor::Testing — informational type print.",
        fires_when: &[
            "Top-level / DSL-block call to `dump_type(expr)` after `include Rigor::Testing`.",
        ],
        does_not_fire_when: &[
            "Outside a context that includes Rigor::Testing.",
            "Argument is not a single expression.",
        ],
        suppression: "Remove the `dump_type` call (it's a debug helper, not a real diagnostic).",
        severity_authored: "info",
        severity_by_profile: [("lenient", "info"), ("balanced", "info"), ("strict", "error")],
        evidence_tier: None,
        since: "0.0.1",
    },
    Entry {
        id: "assert.type-mismatch",
        summary: "`assert_type(\"<expected>\", expr)` from Rigor::Testing — type-equality check.",
        fires_when: &[
            "Inferred type's display does not match the asserted string.",
            "Useful in fixture self-assertions (every `spec/integration/fixtures/*.rb` uses it).",
        ],
        does_not_fire_when: &["Inferred type matches the assertion exactly."],
        suppression: "Update the assertion to the actual inferred type, or correct the source.",
        severity_authored: "error",
        severity_by_profile: [("lenient", "error"), ("balanced", "error"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.0.1",
    },
    Entry {
        id: "flow.always-raises",
        summary: "Call provably raises (today: Integer division-by-zero).",
        fires_when: &[
            "Receiver is `Integer` / `IntegerRange` / `Constant<Integer>`.",
            "Operator is `/` / `%` / `div` / `modulo` / `divmod`.",
            "Argument is a `Constant<Integer>` whose value is exactly zero.",
        ],
        does_not_fire_when: &[
            "Receiver is Float / Rational (those return Infinity / NaN, not an exception).",
            "Argument is a Union containing zero (\"may raise\" not \"always raises\").",
        ],
        suppression: "`# rigor:disable flow.always-raises`.",
        severity_authored: "error",
        severity_by_profile: [("lenient", "warning"), ("balanced", "error"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.0.3",
    },
    Entry {
        id: "flow.unreachable-branch",
        summary: "An if / unless / ternary's literal predicate makes one branch dead.",
        fires_when: &[
            "Predicate is a syntactic literal: `true` / `false` / `nil` / Integer / Float / String / Symbol / Regexp.",
            "The corresponding dead branch carries a non-empty body.",
        ],
        does_not_fire_when: &[
            "Predicate is an inferred-constant expression (not a literal). The literal-only envelope avoids false positives from Rigor's incomplete loop / mutation / RBS-strictness modelling.",
            "The dead branch is empty (no useful location to point at).",
        ],
        suppression: "`# rigor:disable unreachable-branch` on the dead-branch line (the diagnostic points at the dead branch, not the predicate, so the suppression goes there).",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "info"), ("balanced", "warning"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.1.2",
    },
    Entry {
        id: "flow.always-truthy-condition",
        summary: "An if / unless / ternary predicate's inferred type folds to a constant.",
        fires_when: &[
            "Predicate's inferred type is `Type::Constant<true | false | nil | ...>`.",
            "Predicate is NOT a syntactic literal (the literal-only `flow.unreachable-branch` rule covers those).",
        ],
        does_not_fire_when: &[
            "Predicate sits inside a `WhileNode` / `UntilNode` / `ForNode` / `BlockNode` ancestor — Rigor's mutation tracking through loop bodies is incomplete enough that an inferred `Constant<bool>` can be a false positive.",
            "Predicate is a defensive `.nil?` / `.empty?` / `.zero?` / `.any?` / `.none?` / `.all?` / `.respond_to?` call — these typically fire when the user is being more cautious than the RBS strict-on-returns sig admits.",
            "Predicate folds to a non-Constant type (Union / Nominal / Dynamic / etc.).",
        ],
        suppression: "`# rigor:disable always-truthy-condition` on the predicate line.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "info"), ("balanced", "warning"), ("strict", "error")],
        evidence_tier: Some("medium"),
        since: "0.1.2",
    },
    Entry {
        id: "flow.unreachable-clause",
        summary: "A `case` / `when` clause the flow engine's narrowing proves can never match.",
        fires_when: &[
            "The subject is a `case <local>` (`LocalVariableReadNode`), the only shape the engine narrows.",
            "Every `when` condition is a class / module constant (`when String` / `when MyClass`).",
            "The clause's narrowed body subject is `Type::Bot` — disjoint from the subject (`when String` over an `Integer`) or already exhausted by an earlier clause (prior-exhaustion).",
        ],
        does_not_fire_when: &[
            "The subject's type at case entry is `Dynamic` (disjointness is never provable under gradual `Dynamic`, preserving the gradual guarantee) or already `Bot` (dead code, not a clause error).",
            "A `when` condition is not a class / module constant — `when nil`, ranges, regexps, and arbitrary expressions are out of the WD1 scope.",
            "The clause sits inside a `WhileNode` / `UntilNode` / `ForNode` / `BlockNode` (mutation tracking through those is incomplete), or its body is empty (no useful location).",
        ],
        suppression: "`# rigor:disable unreachable-clause` on the dead-clause body line.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "info"), ("balanced", "info"), ("strict", "warning")],
        evidence_tier: Some("medium"),
        since: "0.1.17",
    },
    Entry {
        id: "flow.dead-assignment",
        summary: "Local variable assigned in a method body but never read.",
        fires_when: &[
            "Plain `LocalVariableWriteNode` (not `+=` / `||=` / multi-assign) inside a `DefNode` body.",
            "The target name does not appear as a `LocalVariableReadNode` anywhere in the same body, including nested blocks / lambdas.",
            "The write is not the last statement of the body (Ruby's implicit return).",
        ],
        does_not_fire_when: &[
            "Top-level / class-body assignments (their reachability spans the file's introspection / require surface).",
            "The target name starts with `_` (Ruby convention for intentionally-unused).",
            "The write is a destructure (`a, b = foo`) or operator-write (`x += 1` / `x ||= 1`).",
            "The write is the last statement of the method body (assignments return their rvalue).",
        ],
        suppression: "`# rigor:disable dead-assignment` on the offending line, or rename the local to `_<name>`.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "info"), ("balanced", "warning"), ("strict", "error")],
        evidence_tier: Some("medium"),
        since: "0.1.2",
    },
    Entry {
        id: "def.return-type-mismatch",
        summary: "Method body's last-expression type is incompatible with the declared return type.",
        fires_when: &[
            "Method has a `def` body the engine can re-type.",
            "Method's RBS sig declares a non-`untyped` return type.",
            "Body's inferred return type does not flow into the declared type under gradual acceptance.",
            "When the RBS sig carries `%a{rigor:v1:return: <refinement>}` (v0.1.2), the refinement carrier — `non-empty-string`, `positive-int`, etc. — replaces the bare RBS class for the comparison, so a body the bare class would accept may still fail the refinement.",
        ],
        does_not_fire_when: &[
            "Method's declared return is `untyped` / `void`.",
            "Body's last expression is `Dynamic[T]` (the engine cannot rule out the declared type).",
        ],
        suppression: "`# rigor:disable def.return-type-mismatch`.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "warning"), ("balanced", "warning"), ("strict", "error")],
        evidence_tier: Some("medium"),
        since: "0.1.0",
    },
    Entry {
        id: "def.method-visibility-mismatch",
        summary: "Explicit-receiver call to a method declared `private` in source.",
        fires_when: &[
            "Call is `receiver.method(...)` with explicit non-self receiver.",
            "Receiver type resolves to `Type::Nominal[X]`.",
            "X is a user-defined class whose source carries the method under `private`.",
        ],
        does_not_fire_when: &[
            "Implicit-self call (no receiver) — always allowed for private.",
            "Receiver is `self` (Ruby 2.7+ permits `self.private_method`).",
            "Receiver class is RBS-known but not user-source-defined (RBS-side visibility is deferred).",
            "Method is `:protected` (subclass tracking is deferred).",
        ],
        suppression: "`# rigor:disable method-visibility-mismatch`.",
        severity_authored: "error",
        severity_by_profile: [("lenient", "warning"), ("balanced", "error"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.1.2",
    },
    Entry {
        id: "def.override-visibility-reduced",
        summary: "Instance-method override reduces the visibility it inherits from an ancestor.",
        fires_when: &[
            "An instance `def` shadows a same-name instance method defined by a project-discovered ancestor (included/prepended module or superclass, cross-file).",
            "The override's source-discovered visibility is strictly more restrictive than the ancestor's (public → protected/private, or protected → private).",
            "Both visibilities are statically observable from project source.",
        ],
        does_not_fire_when: &[
            "Override raises or preserves visibility (only reductions break substitutability).",
            "The shadowed method lives on an RBS-known / third-party ancestor (RBS models only public/private; RBS-parent visibility is a deferred follow-on).",
            "`def self.foo` singleton methods (visibility is instance-side only).",
            "The `private def foo; end` wrap-around form (not yet tracked by the visibility walker).",
        ],
        suppression: "`# rigor:disable def.override-visibility-reduced` on the override.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "off"), ("balanced", "warning"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.1.15",
    },
    Entry {
        id: "def.override-return-widened",
        summary: "Instance-method override widens the return type it inherits from an ancestor.",
        fires_when: &[
            "An instance `def` with an authored RBS signature overrides a same-name method whose RBS signature is declared by a project-discovered ancestor (module or superclass).",
            "The override's declared return is not acceptable where the ancestor's declared return is expected (`parent_return.accepts(override_return)` is `:no`) — a covariance violation.",
        ],
        does_not_fire_when: &[
            "Either side lacks an authored RBS signature (WD1 both-sides-authored gate).",
            "The override narrows or preserves the return (covariant-safe).",
            "The ancestor's return is `untyped` / `self` / an unbound generic (degrades to `Dynamic[Top]`, which accepts everything — FP-safe).",
            "The subtype relationship between the two return types is not resolvable from loaded Ruby classes / their ancestors (a user-only class hierarchy degrades to `:maybe` and stays silent — the check has reach over core / stdlib / loadable-gem hierarchies).",
            "`def self.foo` singleton methods (instance-side only in v1).",
            "The shadowed method lives only on an RBS-known / third-party ancestor not in the project-discovered chain (user-source ancestor scope in v1).",
        ],
        suppression: "`# rigor:disable def.override-return-widened` on the override.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "off"), ("balanced", "warning"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.1.15",
    },
    Entry {
        id: "def.override-param-narrowed",
        summary: "Instance-method override narrows a parameter type it inherits from an ancestor.",
        fires_when: &[
            "An instance `def` with an authored RBS signature overrides a same-name method whose RBS signature is declared by a project-discovered ancestor (module or superclass).",
            "At some matching positional parameter index, the override's slot cannot accept the ancestor's parameter type (`override_param.accepts(parent_param)` is `:no`) — a contravariance violation (the override narrowed the parameter).",
        ],
        does_not_fire_when: &[
            "Either side lacks an authored RBS signature (WD1 both-sides-authored gate).",
            "The override widens or preserves the parameter (contravariant-safe).",
            "Either side is overloaded (more than one method type — arm mapping is ambiguous).",
            "The ancestor's parameter is `untyped` / an unbound generic / an interface (degrades to `Dynamic[Top]`, which is passable to anything — FP-safe).",
            "The subtype relationship between the two parameter types is not resolvable from loaded Ruby classes / their ancestors (a user-only class hierarchy degrades to `:maybe` and stays silent — the check has reach over core / stdlib / loadable-gem hierarchies).",
            "Arity / keyword-requiredness divergence (out of scope for v1 — positional types only).",
            "`def self.foo` singleton methods (instance-side only in v1).",
            "The shadowed method lives only on an RBS-known / third-party ancestor (user-source ancestor scope in v1).",
        ],
        suppression: "`# rigor:disable def.override-param-narrowed` on the override.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "off"), ("balanced", "warning"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.1.15",
    },
    Entry {
        id: "def.ivar-write-mismatch",
        summary: "Same instance variable assigned a different concrete class within one class.",
        fires_when: &[
            "Two or more `@var = ...` writes occur in instance methods of the same class.",
            "First write's rvalue resolves to a concrete class (Nominal / Singleton / Constant / Tuple → \"Array\" / HashShape → \"Hash\").",
            "A later write's rvalue resolves to a different concrete class.",
        ],
        does_not_fire_when: &[
            "Later write is `nil` — the `@cache = nil` clear-idiom is allowlisted.",
            "Either side is Union / Dynamic / IntegerRange / a shape-varied carrier.",
            "Writes live in different classes that happen to share an ivar name.",
            "Writes are in `def self.foo` (singleton) bodies — those track separately.",
        ],
        suppression: "`# rigor:disable ivar-write-mismatch` on the offending write.",
        severity_authored: "error",
        severity_by_profile: [("lenient", "warning"), ("balanced", "warning"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.1.2",
    },
    Entry {
        id: "flow.duplicate-hash-key",
        summary: "Duplicate literal key within a single Hash literal (the last entry wins silently at runtime).",
        fires_when: &[
            "Two entries of one Hash literal — braced (`{ a: 1, a: 2 }`) or bare keyword arguments \
             (`m(a: 1, a: 2)`) — carry the same value-pinned literal key: a symbol (the `key:` shorthand \
             and `:key =>` spell the same symbol), a plain non-interpolated string, an integer, a float, \
             or `true` / `false` / `nil`.",
            "A `**splat` between two identical literal keys does not rescue the pair — the later literal \
             entry still overwrites the earlier one regardless of what the splat contributes.",
        ],
        does_not_fire_when: &[
            "Either key is not value-pinned at parse time: interpolated strings / symbols, constants, \
             method calls, locals, and `**splat` entries are never compared.",
            "The keys live in different literal kinds — `:a` vs `\"a\"`, and `1` vs `1.0` (`1.eql?(1.0)` \
             is false, so Hash treats them as distinct keys) never collide.",
            "The repeated keys sit in different Hash literals (nested literals are each their own scope).",
        ],
        suppression: "`# rigor:disable duplicate-hash-key` on the later occurrence's line.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "info"), ("balanced", "warning"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.3.0",
    },
    Entry {
        id: "flow.return-in-ensure",
        summary: "Explicit `return` inside an `ensure` clause swallows in-flight exceptions.",
        fires_when: &[
            "An explicit `return` sits lexically inside the `ensure` clause of a `begin` / `def` / class \
             body (`ensure` always runs, so its `return` overrides the method's in-flight return value \
             AND silently discards any exception being raised).",
            "The `return` is inside a plain block (`each do ... return ... end`) within the ensure body — \
             a `return` there still exits the enclosing method.",
        ],
        does_not_fire_when: &[
            "The `return` is inside a nested `def`, a lambda (`->` / `lambda`), or a `define_method` \
             block within the ensure body — it exits that inner frame, not the one the `ensure` guards.",
            "The `ensure` body contains no explicit `return` (implicit last-expression values in `ensure` \
             are discarded harmlessly and do not swallow exceptions).",
        ],
        suppression: "`# rigor:disable flow.return-in-ensure` on the `return` line.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "info"), ("balanced", "warning"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.3.0",
    },
    Entry {
        id: "suppression.unknown-rule",
        summary: "A `# rigor:disable[-file]` comment names a rule that does not exist.",
        fires_when: &[
            "A `# rigor:disable` / `# rigor:disable-file` marker carries a token that is not a canonical \
             rule id, a legacy alias, `all`, or a family wildcard (`call` / `flow` / ...).",
            "The token is also not a known non-catalogue engine diagnostic (`rbs_extended.*`, `dynamic.*`, \
             `rbs.*`, `pre-eval.*`, or a bare engine id such as `load-error`).",
            "Typically a typo — `call.undefined-metod` — leaving the suppression silently ineffective.",
        ],
        does_not_fire_when: &[
            "The token resolves (canonical id, legacy alias, `all`, family wildcard, known engine id).",
            "The token starts with `plugin.` — plugins load dynamically, so their rule vocabulary cannot \
             be enumerated statically and under-warning is the FP-safe direction.",
            "The comment merely mentions the marker followed by non-token text (documentation prose \
             like \"`# rigor:disable <rule>` comments\") — that is not parsed as a suppression either.",
        ],
        suppression: "Fix or remove the dead token; `# rigor:disable suppression.unknown-rule` on the \
                      same line, or `disable: [\"suppression.unknown-rule\"]` in `.rigor.yml`.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "warning"), ("balanced", "warning"), ("strict", "warning")],
        evidence_tier: Some("high"),
        since: "0.3.0",
    },
    Entry {
        id: "suppression.empty",
        summary: "A `# rigor:disable[-file]` comment lists no rules.",
        fires_when: &[
            "A comment is exactly the bare marker (`# rigor:disable` / `# rigor:disable-file`) with \
             nothing but whitespace or commas after it.",
            "Such a marker suppresses nothing — the author almost certainly meant to name rules or `all`.",
        ],
        does_not_fire_when: &[
            "At least one token follows the marker (each token is then checked by \
             `suppression.unknown-rule` instead).",
            "Non-token text follows the marker (documentation prose mentioning the syntax).",
        ],
        suppression: "Complete the marker (`# rigor:disable <rule>` / `all`) or delete it; \
                      `disable: [\"suppression.empty\"]` in `.rigor.yml`.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "warning"), ("balanced", "warning"), ("strict", "warning")],
        evidence_tier: Some("high"),
        since: "0.3.0",
    },
];

/// Resolve a token to its catalogue entries (reference `RuleCatalog.resolve`):
///
/// - canonical id → 1-element vec,
/// - legacy alias → 1-element vec (resolved to canonical),
/// - family token (`call`) → every entry under that family, id-sorted,
/// - unknown token → empty vec.
fn resolve(token: &str) -> Vec<&'static Entry> {
    if let Some(e) = ENTRIES.iter().find(|e| e.id == token) {
        return vec![e];
    }
    if let Some((_, canonical)) = LEGACY_RULE_ALIASES.iter().find(|(legacy, _)| *legacy == token) {
        return ENTRIES.iter().filter(|e| e.id == *canonical).collect();
    }
    if RULE_FAMILIES.contains(&token) {
        let prefix = format!("{token}.");
        let mut hits: Vec<&Entry> = ENTRIES.iter().filter(|e| e.id.starts_with(&prefix)).collect();
        hits.sort_by(|a, b| a.id.cmp(b.id));
        return hits;
    }
    Vec::new()
}

/// Every entry, id-sorted (reference `RuleCatalog.all`).
fn all() -> Vec<&'static Entry> {
    let mut v: Vec<&Entry> = ENTRIES.iter().collect();
    v.sort_by(|a, b| a.id.cmp(b.id));
    v
}

// ---------------------------------------------------------------------------
// Catalogue surface reused by `rigor docs` (§11).
//
// `docs` is a thin view over the SAME catalogue `explain` renders: rigor-rs has
// no bundled manual pages (the reference's `docs/manual/*.md`), so its
// documented-content corpus IS the rule catalogue. These helpers let `docs.rs`
// list the catalogue and render one rule's documentation without duplicating
// the `ENTRIES` table or its rendering.
// ---------------------------------------------------------------------------

/// `(id, summary)` for every catalogue rule, id-sorted — the data `rigor docs`
/// (no argument) lists. Mirrors the index `explain` prints, exposed as data so
/// `docs` can frame it with its own header.
pub fn catalogue_index() -> Vec<(&'static str, &'static str)> {
    all().iter().map(|e| (e.id, e.summary)).collect()
}

/// Render one rule's documentation to stdout, by canonical id, legacy alias, or
/// family token (the SAME resolution `explain` uses). Returns `false` (printing
/// nothing) for an unknown token so the caller can emit its own error + exit
/// code. The body is exactly `explain`'s text rendering — the rule documentation
/// rigor-rs ships.
pub fn render_rule_doc(token: &str) -> bool {
    let entries = resolve(token);
    if entries.is_empty() {
        return false;
    }
    render_entries(&entries, "text");
    true
}

/// Structured rule-catalogue JSON for the MCP `explain` tool (§12). `None` query
/// → an id-sorted index (`{id, summary}` per rule). `Some(token)` → the full
/// metadata for every matching entry (canonical id / legacy alias / family token,
/// the SAME resolution `explain` uses), or `Err` when nothing resolves. Reuses
/// the single `ENTRIES` table — no duplication of catalogue content.
pub(crate) fn explain_json(query: Option<&str>) -> Result<serde_json::Value, String> {
    use serde_json::json;
    let entries = match query {
        None => all(),
        Some(token) => {
            let hits = resolve(token);
            if hits.is_empty() {
                return Err(format!("unknown rule, alias, or family: {token}"));
            }
            hits
        }
    };
    let full = query.is_some(); // index view (id+summary) vs full metadata.
    let rules: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            if !full {
                return json!({ "id": e.id, "summary": e.summary });
            }
            json!({
                "id": e.id,
                "summary": e.summary,
                "fires_when": e.fires_when,
                "does_not_fire_when": e.does_not_fire_when,
                "suppression": e.suppression,
                "severity_authored": e.severity_authored,
                "severity_by_profile": e.severity_by_profile
                    .iter()
                    .map(|(p, s)| json!({ "profile": p, "severity": s }))
                    .collect::<Vec<_>>(),
                "evidence_tier": e.evidence_tier,
                "since": e.since,
                "documentation_url": e.documentation_url(),
                "aliases": e.aliases(),
            })
        })
        .collect();
    Ok(json!({ "rules": rules, "count": rules.len() }))
}

/// `rigor explain [--format text|json] [<rule>]` — print rule metadata.
/// Exit 0 on success, 64 on an unknown rule or a usage error.
pub fn cmd_explain(args: &[String]) -> ExitCode {
    let mut format = "text";
    let mut token: Option<&str> = None;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--format" => match it.next().map(String::as_str) {
                Some(f @ ("text" | "json")) => format = f,
                other => {
                    eprintln!("rigor explain: --format expects `text` or `json`, got {other:?}");
                    return ExitCode::from(64);
                }
            },
            other => {
                if token.is_some() {
                    eprintln!("rigor explain: unexpected argument `{other}`");
                    return ExitCode::from(64);
                }
                token = Some(other);
            }
        }
    }

    match token {
        None => {
            render_index(format);
            ExitCode::SUCCESS
        }
        Some(tok) => {
            let entries = resolve(tok);
            if entries.is_empty() {
                eprintln!("Unknown rule: {tok}");
                eprintln!("Run `rigor explain` with no arguments to list every rule.");
                return ExitCode::from(64);
            }
            render_entries(&entries, format);
            ExitCode::SUCCESS
        }
    }
}

/// The no-argument listing: each rule's id + one-line summary.
fn render_index(format: &str) {
    if format == "json" {
        println!("{}", json_array(&all()));
        return;
    }
    println!("Available rules:");
    println!();
    for entry in all() {
        // `ljust(33)`, like the reference's index column.
        println!("  {:<33} {}", entry.id, entry.summary);
    }
    println!();
    println!("Run `rigor explain <rule>` for the full description.");
    println!("Family wildcards (`call`, `flow`, `assert`, `dump`, `def`) print every rule under that prefix.");
}

/// Render the resolved entries (one for an id, many for a family).
fn render_entries(entries: &[&Entry], format: &str) {
    if format == "json" {
        println!("{}", json_array(entries));
        return;
    }
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            println!();
        }
        render_entry_text(entry);
    }
}

/// The full text rendering of one rule (mirrors the reference's
/// `render_entry_text`).
fn render_entry_text(entry: &Entry) {
    println!("{}", entry.id);
    println!("{}", "=".repeat(entry.id.len()));
    println!();
    println!("{}", entry.summary);
    println!();

    let aliases = entry.aliases();
    if !aliases.is_empty() {
        println!("Legacy aliases: {}", aliases.join(", "));
        println!();
    }

    println!("Authored severity: :{}", entry.severity_authored);
    let profile_table: Vec<String> = entry
        .severity_by_profile
        .iter()
        .map(|(profile, sev)| format!("{profile} → :{sev}"))
        .collect();
    println!("Severity by profile: {}", profile_table.join(", "));
    println!(
        "Evidence tier: {}",
        entry.evidence_tier.unwrap_or("n/a (informational)")
    );
    println!();

    render_section("Fires when:", entry.fires_when);
    render_section("Does not fire when:", entry.does_not_fire_when);
    println!("Suppression: {}", entry.suppression);
    println!("Documentation: {}", entry.documentation_url());
    println!("Since: rigor {}", entry.since);
}

/// Print a bulleted section, or nothing when empty (reference `render_section`).
fn render_section(heading: &str, items: &[&str]) {
    if items.is_empty() {
        return;
    }
    println!("{heading}");
    for item in items {
        println!("  - {item}");
    }
    println!();
}

// ---------------------------------------------------------------------------
// JSON rendering
// ---------------------------------------------------------------------------

/// Pretty-print a list of entries as the reference's JSON array. The key order
/// mirrors the reference's `Entry#to_h` exactly (id, aliases, summary,
/// fires_when, does_not_fire_when, suppression, severity_authored,
/// severity_by_profile, documentation_url, since, and evidence_tier last when
/// present). serde_json's `Map` would alphabetize the keys, so the document is
/// hand-built (2-space indent, matching `JSON.pretty_generate`) to keep byte
/// parity with the reference. JSON string escaping is delegated to
/// `serde_json::to_string` on a `Value::String` so quotes / unicode are correct.
fn json_array(entries: &[&Entry]) -> String {
    if entries.is_empty() {
        return "[]".to_string();
    }
    let objs: Vec<String> = entries.iter().map(|e| entry_to_json(e)).collect();
    format!("[\n{}\n]", objs.join(",\n"))
}

/// Render one entry as a 4-space-indented JSON object (its keys at 6 spaces),
/// matching `JSON.pretty_generate`'s nesting under the top-level array.
fn entry_to_json(entry: &Entry) -> String {
    // Each `(key, rendered-value)` in the reference's insertion order. Nested
    // containers are rendered at the key's indent (4 spaces): their elements sit
    // at 6 and their closing bracket back at 4, matching `JSON.pretty_generate`.
    let mut fields: Vec<(&str, String)> = vec![
        ("id", jstr(entry.id)),
        ("aliases", jstr_array(&entry.aliases(), 4)),
        ("summary", jstr(entry.summary)),
        ("fires_when", jstr_array(entry.fires_when, 4)),
        ("does_not_fire_when", jstr_array(entry.does_not_fire_when, 4)),
        ("suppression", jstr(entry.suppression)),
        ("severity_authored", jstr(entry.severity_authored)),
        ("severity_by_profile", severity_object(entry, 4)),
        ("documentation_url", jstr(&entry.documentation_url())),
        ("since", jstr(entry.since)),
    ];
    if let Some(tier) = entry.evidence_tier {
        fields.push(("evidence_tier", jstr(tier)));
    }
    let lines: Vec<String> = fields
        .iter()
        .map(|(k, v)| format!("    {}: {}", jstr(k), v))
        .collect();
    format!("  {{\n{}\n  }}", lines.join(",\n"))
}

/// The `severity_by_profile` nested object, rendered at `indent` spaces.
fn severity_object(entry: &Entry, indent: usize) -> String {
    let pad = " ".repeat(indent + 2);
    let lines: Vec<String> = entry
        .severity_by_profile
        .iter()
        .map(|(k, v)| format!("{pad}{}: {}", jstr(k), jstr(v)))
        .collect();
    format!("{{\n{}\n{}}}", lines.join(",\n"), " ".repeat(indent))
}

/// Render a `&[&str]` as a pretty JSON array at `indent` spaces. An empty array
/// renders as `[]` (matching `JSON.pretty_generate`).
fn jstr_array(items: &[&str], indent: usize) -> String {
    if items.is_empty() {
        return "[]".to_string();
    }
    let pad = " ".repeat(indent + 2);
    let lines: Vec<String> = items.iter().map(|s| format!("{pad}{}", jstr(s))).collect();
    format!("[\n{}\n{}]", lines.join(",\n"), " ".repeat(indent))
}

/// A JSON-escaped string literal (delegated to serde_json so escaping is exact).
fn jstr(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_canonical_id() {
        let e = resolve("call.undefined-method");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].id, "call.undefined-method");
        assert_eq!(e[0].severity_authored, "error");
        assert_eq!(e[0].evidence_tier, Some("high"));
    }

    #[test]
    fn resolves_legacy_alias() {
        let e = resolve("undefined-method");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].id, "call.undefined-method");
    }

    #[test]
    fn resolves_family_wildcard_sorted() {
        let e = resolve("assert");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].id, "assert.type-mismatch");

        let calls = resolve("call");
        // Every `call.*` rule, id-sorted.
        let ids: Vec<&str> = calls.iter().map(|e| e.id).collect();
        assert_eq!(
            ids,
            vec![
                "call.argument-type-mismatch",
                "call.possible-nil-receiver",
                "call.self-undefined-method",
                "call.undefined-method",
                "call.unresolved-toplevel",
                "call.wrong-arity",
            ]
        );
    }

    #[test]
    fn unknown_rule_resolves_empty() {
        assert!(resolve("bogus.rule").is_empty());
    }

    #[test]
    fn documentation_url_anchors_on_dashed_id() {
        let e = resolve("call.undefined-method");
        assert_eq!(
            e[0].documentation_url(),
            "https://github.com/rigortype/rigor/blob/main/docs/manual/04-diagnostics.md#rule-call-undefined-method"
        );
    }

    #[test]
    fn all_is_id_sorted_and_complete() {
        let ids: Vec<&str> = all().iter().map(|e| e.id).collect();
        // 23 rules: the reference's ALL_RULES minus the two v0.3.0 ids rigor-rs
        // does not yet emit (`call.raise-non-exception`, `flow.shadowed-rescue-clause`),
        // which stay known suppression tokens without an explain entry.
        assert_eq!(ids.len(), 23);
        // Sorted ascending by id.
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn aliases_reverse_maps_the_legacy_table() {
        let e = resolve("def.method-visibility-mismatch");
        assert_eq!(e[0].aliases(), vec!["method-visibility-mismatch"]);
        // A rule with no legacy alias reports none.
        let nil = resolve("call.unresolved-toplevel");
        assert!(nil[0].aliases().is_empty());
    }

    #[test]
    fn json_contains_expected_fields() {
        let e = resolve("call.undefined-method");
        let s = json_array(&e);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        let obj = &v[0];
        assert_eq!(obj["id"], "call.undefined-method");
        assert_eq!(obj["aliases"][0], "undefined-method");
        assert_eq!(obj["severity_by_profile"]["balanced"], "error");
        assert_eq!(obj["evidence_tier"], "high");
        assert!(obj["documentation_url"]
            .as_str()
            .unwrap()
            .contains("rule-call-undefined-method"));
    }

    #[test]
    fn json_omits_evidence_tier_for_informational() {
        let e = resolve("dump.type");
        let s = json_array(&e);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert!(v[0].get("evidence_tier").is_none());
    }
}
