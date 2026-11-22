//! `rigor explain <rule>` (ADR-0015) — the read-only rule-catalogue probe.
//!
//! Mirrors the reference's `Rigor::CLI::ExplainCommand` + `Analysis::RuleCatalog`:
//! it prints the per-rule metadata (summary, severity-by-profile, fires-when /
//! does-not-fire-when, suppression, docs URL) for one canonical rule id, a legacy
//! alias, or a family wildcard (`call`/`flow`/`assert`/`dump`/`def`/`global`/
//! `suppression`/`static`/`effect`). A `sig.skipped.*` token resolves against the
//! second catalogue `explain` consults, `SigGen::SkipReasonCatalog`. With no
//! argument it lists every rule's id + one-line summary, then the skip reasons.
//!
//! The catalogue here is a static table mirroring the reference's
//! `RuleCatalog::ENTRIES` *content* verbatim — `explain` exposes existing
//! capability (the rule metadata) and does not touch analysis, so faithful
//! parity is a copy of the reference's authored strings. The command is
//! read-only: no parser, no analyzer, no I/O beyond the rendered catalogue.

use std::process::ExitCode;

use crate::optparse::Item;

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
    /// `LEGACY_RULE_ALIASES` in `rigor-rules`).
    fn aliases(&self) -> Vec<&'static str> {
        rigor_rules::legacy_aliases()
            .iter()
            .filter(|(_, canonical)| *canonical == self.id)
            .map(|(legacy, _)| *legacy)
            .collect()
    }
}

/// Stable documentation home for a built-in rule (mirrors the reference's
/// `RuleCatalog::DOCUMENTATION_BASE`).
///
/// The canonical docs HOST, deliberately not a `github.com/…/blob/<ref>/…` path
/// (reference ADR-65 amendment, upstream #438): a git ref inside a frozen public
/// contract is a mutable component, and the one baked in here — `main`, a branch
/// the rigor repository has never had — made every URL this project ever emitted
/// 404, on every `--format json` diagnostic and every `rigor explain`. A branch
/// name rots on a rename; a tag resolves only once pushed, so every build between
/// a version bump and its tag would 404 again. The published site carries no ref
/// at all and renders `docs/manual/04-diagnostics.md` verbatim, `<a id="rule-…">`
/// anchors included, so the fragment half of the contract is unchanged.
const DOCUMENTATION_BASE: &str = "https://rigor.typedduck.fail/manual/04-diagnostics/";

// The rule-token vocabulary — family wildcards, legacy aliases, the full
// canonical id set — lives in `rigor-rules` (`RULE_FAMILIES` /
// `LEGACY_RULE_ALIASES` / `ALL_CANONICAL_RULES`, `check_rules/rule_ids.rb`).
// `explain` reads those tables directly (rigor-rs#255): a second copy here had
// drifted three families (`global`, `static`, `effect`) and four `effect.*`
// entries behind the reference.

/// The full rule catalogue — content mirrors the reference's
/// `RuleCatalog::ENTRIES` verbatim (every authored string), so `explain`
/// prints the same per-rule reference the docs site publishes.
const ENTRIES: &[Entry] = &[
    Entry {
        id: "assert.type-mismatch",
        summary: "`assert_type(\"<expected>\", expr)` from Rigor::Testing — type-equality check.",
        fires_when: &[
            "Inferred type's display does not match the asserted string.",
            "Useful in fixture self-assertions (every `spec/integration/fixtures/*.rb` uses it).",
        ],
        does_not_fire_when: &[
            "Inferred type matches the assertion exactly.",
        ],
        suppression: "Update the assertion to the actual inferred type, or correct the source.",
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
        id: "call.deprecated-ruby2-keywords",
        summary: "A call to the ruby2_keywords family Ruby 4.1 deprecates, under an explicit target_ruby >= 4.1.",
        fires_when: &[
            "`.rigor.yml` sets `target_ruby` explicitly to \"4.1\" or later.",
            "The call reaches `Module#ruby2_keywords` (no receiver or `self` in a class / module body, a `class << self` body or a singleton method), top-level `ruby2_keywords` (at the top level, outside any block), or `Proc#ruby2_keywords` (a receiver typed `Proc`): removed in Ruby 4.4.",
            "The call reaches `Hash.ruby2_keywords_hash?` or `Hash.ruby2_keywords_hash` (on `Hash` or a subclass RBS knows): removed in Ruby 4.5.",
            "The same names through `send` / `__send__` with a literal Symbol.",
        ],
        does_not_fire_when: &[
            "`target_ruby` is absent, is the default, is below 4.1, or is \"latest\" (which names a parser, not a runtime).",
            "A project or `pre_eval:` file defines a method by the same name anywhere.",
            "The call sits in a block at the top level, in an `instance_eval` / `instance_exec` block, in an instance method of a class or module, or on an explicit receiver that is not `Proc` / `Hash`.",
            "The call sits in a version-guard arm that cannot run on the stated Ruby, under a guard on `RUBY_VERSION` that cannot be decided, or after a guard that returns or raises.",
        ],
        suppression: "`# rigor:disable call.deprecated-ruby2-keywords` on the call line. To fix it instead, rewrite the delegation the flag serves to `(...)` or `(*args, **kwargs, &block)` first, then drop the `ruby2_keywords` call: removing it alone breaks keyword pass-through.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "info"), ("balanced", "warning"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.4.2",
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
        id: "call.raise-non-exception",
        summary: "`raise` / `fail` operand provably cannot be raised (TypeError at runtime).",
        fires_when: &[
            "The call is an implicit-self `raise x` / `fail x` (Kernel#raise) with a plain first positional argument.",
            "The argument's inferred type is concrete and provably NOT a legal raise operand: not an Exception class, not an Exception instance, not a String, and its class defines no `#exception` (e.g. `raise 42`, `raise :sym`, `raise [1, 2]`, `raise nil` — an explicit nil argument is a TypeError, unlike bare `raise`).",
            "A Class-object operand fires only when the class is RBS-known and its ancestry provably excludes Exception (including a concretely-resolved bare Module).",
            "A union argument fires only when EVERY arm is independently illegal.",
        ],
        does_not_fire_when: &[
            "The argument is `Dynamic[T]` / untyped / unresolved, or a union with any legal or unknown arm.",
            "The call has an explicit receiver (`obj.raise(...)` is a user method, not Kernel#raise).",
            "The project redefines `raise` / `fail` anywhere the call could resolve (toplevel def, Object/Kernel monkey-patch, or a def on the enclosing class — in source or `pre_eval:`).",
            "The argument's class defines `#exception` (RBS-declared or project-defined) — the duck protocol `raise` consults at runtime.",
            "The instance operand's nominal class subsumes exceptions (`Object`, `Class`, `Module`, a module type) — the runtime value may still be a legal operand.",
            "The operand's class is declared in the analyzed source — a project `sig/` that omits the superclass would misread the ancestry as non-Exception, so source-declared classes stay silent.",
            "Bare `raise` (re-raises `$!`), or a splat / keyword / forwarded first argument.",
        ],
        suppression: "`# rigor:disable call.raise-non-exception`.",
        severity_authored: "error",
        severity_by_profile: [("lenient", "warning"), ("balanced", "error"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.3.0",
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
            "Receiver class answers beyond any signature: a plugin-declared open receiver (ADR-26), or one Rigor's bundled overlays name (`OpenStruct`, whose fields are per-instance singleton methods; `ActiveSupport::Duration` and `ActiveSupport::Concern` when their gem overlay loads). Both sides of the class are open, and an inherited Object / Kernel signature no longer decides `call.wrong-arity` or `call.argument-type-mismatch` on it.",
            "The program may answer the call another way (#1722): a `method_missing` on the receiver's class or an ancestor, in source or in RBS (#2792; an RBS `method_missing` on `BasicObject`, `Object` or `Kernel` does not count); a module mixed into either that defines the method (`Integer.include(Ext)` counts wherever it is written); singleton methods given to the receiver local (`s.extend(M)`, `define_singleton_method`, `class << s`); or, for a receiver typed by its class's name rather than a literal, a project subclass that defines the method.",
        ],
        suppression: "`# rigor:disable call.undefined-method` on the call line, or `disable: [\"call.undefined-method\"]` in `.rigor.yml`.",
        severity_authored: "error",
        severity_by_profile: [("lenient", "error"), ("balanced", "error"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.0.1",
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
        id: "def.ivar-write-mismatch",
        summary: "Same instance variable assigned a different concrete class within one class.",
        fires_when: &[
            "Two or more `@var = ...` writes occur in instance methods of the same class.",
            "First write's rvalue resolves to a concrete class (Nominal / Singleton / Constant / Tuple → \"Array\" / HashShape → \"Hash\").",
            "A later write's rvalue resolves to a different concrete class.",
        ],
        does_not_fire_when: &[
            "Later write is `nil` — the `@cache = nil` clear-idiom is allowlisted.",
            "The class's RBS declares the ivar (`@var: T`, or the slot an `attr_*` member implies, own or inherited) and that type accepts both the first write and the later one.",
            "Either side is Union / Dynamic / IntegerRange / a shape-varied carrier.",
            "Writes live in different classes that happen to share an ivar name.",
            "Writes are in a singleton body — `def self.foo` or a `class << self` def — those track separately.",
            "Writes are in a `def` inside an anonymous-class factory block (`Class.new do … end`, `Module.new`, `Struct.new`, `Data.define`): that body defines another class.",
            "The class descends from a plugin-declared per-invocation class (ADR-125; rigor-actionpack declares `ActionController::Base` / `ActionController::API`, rigor-actionmailer `ActionMailer::Base`), and the write and every conflicting write sit in different actions: public methods no callback macro or `helper_method` names. One action runs per instance.",
        ],
        suppression: "`# rigor:disable ivar-write-mismatch` on the offending write.",
        severity_authored: "error",
        severity_by_profile: [("lenient", "warning"), ("balanced", "warning"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.1.2",
    },
    Entry {
        id: "def.method-visibility-mismatch",
        summary: "Explicit-receiver call to a method declared `private` in source.",
        fires_when: &[
            "Call is `receiver.method(...)` with explicit non-self receiver.",
            "Receiver type resolves to `Type::Nominal[X]`, or to the class object `singleton(X)`.",
            "X is a user-defined class whose source carries the method under `private`; for a class object, the singleton method the call reaches is private (`private_class_method`, `class << self` with `private`, or a private method of a module X extends).",
        ],
        does_not_fire_when: &[
            "Implicit-self call (no receiver) — always allowed for private.",
            "Receiver is `self` (Ruby 2.7+ permits `self.private_method`).",
            "Receiver class is RBS-known but not user-source-defined (RBS-side visibility is deferred).",
            "Method is `:protected` (subclass tracking is deferred).",
            "A `method_missing` or `respond_to_missing?` on the receiver's chain, on the side the call reaches, may answer the call (Ruby sends a private call to `method_missing`).",
            "The visibility rests on a statement that may not run, or on files that disagree about it.",
            "On a class object: the receiver is not a constant (`self.class.x` may be a subclass that re-publicises the name), or the call is not inside a `def` (load-time code may run before the method is made private).",
        ],
        suppression: "`# rigor:disable method-visibility-mismatch`.",
        severity_authored: "error",
        severity_by_profile: [("lenient", "warning"), ("balanced", "error"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.1.2",
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
            "`initialize` and the `initialize_copy` / `_dup` / `_clone` hooks: `Class#new` / `dup` / `clone` reach them only on the receiver's own class, so no parent-typed caller passes the parent's arguments (ADR-35 WD10).",
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
        id: "def.override-return-widened",
        summary: "Instance-method override widens the return type it inherits from an ancestor.",
        fires_when: &[
            "An instance `def` with an authored RBS signature overrides a same-name method whose RBS signature is declared by a project-discovered ancestor (module or superclass).",
            "The override's declared return is not acceptable where the ancestor's declared return is expected (`parent_return.accepts(override_return)` is `:no`) — a covariance violation.",
        ],
        does_not_fire_when: &[
            "Either side lacks an authored RBS signature (WD1 both-sides-authored gate).",
            "The override narrows or preserves the return (covariant-safe).",
            "`initialize` and the `initialize_copy` / `_dup` / `_clone` hooks: `Class#new` / `dup` / `clone` reach them on the receiver's own class and discard their return, outside substitutability (ADR-35 WD10).",
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
        id: "def.override-visibility-reduced",
        summary: "Instance-method override reduces the visibility it inherits from an ancestor.",
        fires_when: &[
            "An instance `def` shadows a same-name instance method defined by a project-discovered ancestor (included/prepended module or superclass, cross-file).",
            "The override's source-discovered visibility is strictly more restrictive than the ancestor's (public → protected/private, or protected → private).",
            "Both visibilities are statically observable from project source.",
        ],
        does_not_fire_when: &[
            "Override raises or preserves visibility (only reductions break substitutability).",
            "The method is one Ruby makes private wherever it is defined (`initialize`, `initialize_copy` / `_dup` / `_clone`, `respond_to_missing?`), so its section is not its runtime visibility (ADR-35 WD10).",
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
        id: "effect.annotations-unchecked",
        summary: "Effect annotations are present, but no `effects:` block enables anything that reads them.",
        fires_when: &[
            "The project's own `signature_paths:` RBS carries a `%a{pure}` or `%a{rigor:v1:effect …}` annotation.",
            "`.rigor.yml` carries no `effects:` block, so effect collection never runs and nothing checks those annotations. One diagnostic per run, positioned at the first such annotation.",
        ],
        does_not_fire_when: &[
            "An `effects:` block is present — including `effects: {}` and `effects: {check: false}`, both of which are deliberate answers to the question this asks.",
            "No project-authored effect annotation exists.",
            "The annotation is written only as an rbs-inline `# @rbs %a{…}` comment and this run did not already have an RBS environment at hand: detecting it would mean building one on a surface that must stay free, so the residual stays quiet rather than paying for an `:info`.",
        ],
        suppression: "Add an `effects:` block (`effects: {}` enables collection), or `disable: [\"effect.annotations-unchecked\"]` in `.rigor.yml` to keep the annotations documentary.",
        severity_authored: "info",
        severity_by_profile: [("lenient", "info"), ("balanced", "info"), ("strict", "info")],
        evidence_tier: None,
        since: "0.3.4",
    },
    Entry {
        id: "effect.envelope-exceeded",
        summary: "A method performs an effect its declared envelope does not admit.",
        fires_when: &[
            "The project's `.rigor.yml` carries an `effects:` block and `effects.check` is on (it defaults to on when the block is present).",
            "The method's RBS carries an envelope — `%a{pure}` (the empty bound) or `%a{rigor:v1:effect <labels>}` — written on the method, or on its class / module and distributed to it.",
            "The method's PROVEN effect labels — its own body plus the transitive closure over the project methods it calls — include a label no member of the bound subsumes.",
        ],
        does_not_fire_when: &[
            "No `effects:` block is configured, or `effects.check: false` is set — an annotation alone never turns the check on.",
            "The exceeding label is `mutate.local`: mutating what the frame itself allocated and never let escape is tolerated by every envelope, `%a{pure}` included.",
            "The label is only suspected rather than proven — an unresolved or dynamic call taints the summary's exhaustiveness bit and contributes nothing to the proven lane, so \"possibly more\" never produces a finding.",
            "The envelope names a label the effect registry does not recognise (a typo, a retired spelling): the whole tag then reads as unbounded, which suppresses findings rather than inventing them.",
            "The envelope was written on a supertype rather than on this class — the inherited-bound (Liskov) reading is `effect.liskov-widened`.",
            "The envelope was written outside the project's own `signature_paths:` RBS (core, a gem's shipped RBS): only project-authored envelopes are checked. Such an envelope is still imported as a `≤` bound at calls INTO it, which produces no finding.",
        ],
        suppression: "`# rigor:disable effect.envelope-exceeded` on the Ruby `def` line (the diagnostic is positioned there, not on the `.rbs` line), or `disable: [\"effect.envelope-exceeded\"]` in `.rigor.yml`. Widening or removing the envelope is the real fix.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "warning"), ("balanced", "warning"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.3.4",
    },
    Entry {
        id: "effect.liskov-widened",
        summary: "An override performs or declares an effect the envelope it inherits does not admit.",
        fires_when: &[
            "The project's `.rigor.yml` carries an `effects:` block and `effects.check` is on (it defaults to on when the block is present).",
            "A method the project defines redefines a method of a SUPERCLASS, and that ancestor's definition carries an envelope — written on it, distributed from its class, or put there by an `effects.envelopes:` convention. The nearest enveloped ancestor wins.",
            "The override declares NO envelope of its own and its PROVEN effect labels — its body plus the transitive closure over the project methods it calls — include a label the inherited bound does not admit.",
            "Or the override declares its OWN envelope and that bound is wider than the inherited one. This half is proven-independent: two authored bounds, compared by subsumption.",
        ],
        does_not_fire_when: &[
            "No `effects:` block is configured, or `effects.check: false` is set.",
            "Nobody wrote an envelope on the overridden method. The check is both-sides-authored: an override alone can never produce it.",
            "The override is purer than the bound it inherits — that is the whole point of an upper bound, and a narrower envelope on an override is correct by construction.",
            "The relation is a module include rather than a subclass. An includer's own `def` sits AHEAD of the module's in Ruby's ancestry rather than under it, so the substitutability argument that licenses this check does not apply.",
            "The exceeding label is `mutate.local`, or the label is only suspected rather than proven: an unresolved or dynamic call taints exhaustiveness and contributes nothing to the proven lane.",
            "The ancestor's envelope names a label the registry does not recognise: the whole tag reads as unbounded, which suppresses findings rather than inventing them.",
        ],
        suppression: "`# rigor:disable effect.liskov-widened` on the override's Ruby `def` line, or `disable: [\"effect.liskov-widened\"]` in `.rigor.yml`. Widening the ancestor's envelope — or moving the effect out of the override — is the real fix.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "warning"), ("balanced", "warning"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.3.4",
    },
    Entry {
        id: "effect.unknown-label",
        summary: "An effect declaration names a label the registry does not recognise, so it bounds nothing.",
        fires_when: &[
            "The project's `.rigor.yml` carries an `effects:` block and `effects.check` is on — the same gate as `effect.envelope-exceeded`, because opting into envelope enforcement is what turns on the diagnostic that says an envelope stopped enforcing.",
            "A `%a{rigor:v1:effect <labels>}` envelope — in `.rbs`, or written as an rbs-inline `# @rbs %a{…}` comment — names a token the effect registry does not know after plugin load, which makes the WHOLE tag read as unbounded (⊤), or an `effects.tolerated:` entry names one, which then tolerates nothing.",
            "Label intent is evident from one of four signals: the spelling is within two edits of a known label, another member of the same list is known, the token carries two or more dot-separated segments, or the registry's retired table names it.",
        ],
        does_not_fire_when: &[
            "No `effects:` block is configured, or `effects.check: false` is set.",
            "The unrecognised token is a lone far-off word (`database`): a vocabulary is open by design, so a bare word nothing resembles is as likely to be a label this project has not registered as it is a typo. The tag still reads ⊤ — silence here is about the diagnostic, never about the reading.",
            "The tag is malformed rather than unrecognised (`io/db`, an empty list): a grammar violation is a different condition, reported through the `RBS::Extended` conflict channel.",
            "The label is registered — by the shared registry, by `effects.labels:`, or by a plugin that owns its root.",
        ],
        suppression: "`disable: [\"effect.unknown-label\"]` in `.rigor.yml`, or a baseline entry. A `# rigor:disable` comment works only where the diagnostic lands in a `.rb` file (an rbs-inline annotation); Rigor does not read suppression comments out of `.rbs` or `.rigor.yml`. Fixing the spelling is the real fix.",
        severity_authored: "info",
        severity_by_profile: [("lenient", "info"), ("balanced", "info"), ("strict", "warning")],
        evidence_tier: Some("high"),
        since: "0.3.4",
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
        id: "flow.duplicate-hash-key",
        summary: "Duplicate literal key within a single Hash literal (the last entry wins silently at runtime).",
        fires_when: &[
            "Two entries of one Hash literal — braced (`{ a: 1, a: 2 }`) or bare keyword arguments (`m(a: 1, a: 2)`) — carry the same value-pinned literal key: a symbol (the `key:` shorthand and `:key =>` spell the same symbol), a plain non-interpolated string, an integer, a float, or `true` / `false` / `nil`.",
            "A `**splat` between two identical literal keys does not rescue the pair — the later literal entry still overwrites the earlier one regardless of what the splat contributes.",
        ],
        does_not_fire_when: &[
            "Either key is not value-pinned at parse time: interpolated strings / symbols, constants, method calls, locals, and `**splat` entries are never compared.",
            "The keys live in different literal kinds — `:a` vs `\"a\"`, and `1` vs `1.0` (`1.eql?(1.0)` is false, so Hash treats them as distinct keys) never collide.",
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
            "An explicit `return` sits lexically inside the `ensure` clause of a `begin` / `def` / class body (`ensure` always runs, so its `return` overrides the method's in-flight return value AND silently discards any exception being raised).",
            "The `return` is inside a plain block (`each do ... return ... end`) within the ensure body — a `return` there still exits the enclosing method.",
        ],
        does_not_fire_when: &[
            "The `return` is inside a nested `def`, a lambda (`->` / `lambda`), or a `define_method` block within the ensure body — it exits that inner frame, not the one the `ensure` guards.",
            "The `ensure` body contains no explicit `return` (implicit last-expression values in `ensure` are discarded harmlessly and do not swallow exceptions).",
        ],
        suppression: "`# rigor:disable flow.return-in-ensure` on the `return` line.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "info"), ("balanced", "warning"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.3.0",
    },
    Entry {
        id: "flow.shadowed-rescue-clause",
        summary: "A `rescue` clause an earlier clause of the same chain already fully catches.",
        fires_when: &[
            "An earlier `rescue` clause in the same `begin` / `def` rescue chain names a superclass (or the same class) of EVERY exception class the later clause names — `rescue StandardError` followed by `rescue ArgumentError` leaves the ArgumentError arm dead.",
            "A bare `rescue` / `rescue => e` counts as the implicit `StandardError` on either side.",
            "Every exception reference in both clauses is a constant path resolving (against its lexical namespace) to a CLASS with known ancestry — RBS / registry core classes, or a project class whose `class Foo < Bar` superclass chain is discovered.",
            "A multi-class arm (`rescue A, B`) fires only when every class it names is covered by earlier clauses.",
        ],
        does_not_fire_when: &[
            "Any exception reference in either clause is an unresolved constant, a dynamic expression (`rescue klass_var`), or a splat (`rescue *ERRORS`) — the comparisons involving that clause stay silent.",
            "A resolved constant is a MODULE (the `rescue MyGem::Error` module-tag mixin pattern — module `===` semantics are custom), or a project constant the discovery cannot certify as a class.",
            "The later clause names a SUPERCLASS of an earlier one — the normal narrow-to-wide rescue order.",
            "The clauses belong to different (nested) `begin` nodes — only clauses of the same chain compare.",
        ],
        suppression: "`# rigor:disable shadowed-rescue-clause` on the shadowed `rescue` line.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "info"), ("balanced", "warning"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.3.0",
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
        id: "global.readonly-write",
        summary: "A read-only special global is written (NameError at runtime).",
        fires_when: &[
            "The write is `$g = value`, `$g op= value`, or a target of a multiple assignment (`$g, x = ...`, `*$g`) — forms that always write — to a special Ruby defines read-only: `$!`, `$$`, `$?`, `$<`, `$FILENAME`, `$*`, `$:` / `$LOAD_PATH` / `$-I`, `$\"` / `$LOADED_FEATURES`, `$-W`, `$-p`, `$-l`, `$-a`.",
            "Whatever the value: the setter raises `NameError` (`$! is a read-only variable`) before it looks at it.",
        ],
        does_not_fire_when: &[
            "The write is `$g ||= v` or `$g &&= v`, which writes only when the current value is falsy (truthy): `$LOAD_PATH ||= []` never writes.",
            "The write is a `for` index or a `rescue => $g` reference.",
            "Any project or `pre_eval:` file aliases the special (`alias $! $err`, on either side), which can make the name another variable.",
            "Mutating the value is not a write: `$LOAD_PATH << dir` and `$LOADED_FEATURES.delete(f)` stay silent.",
            "The write sits in the dead arm of a decidable version guard. Other code that never runs is not recognised, and a write there still fires.",
        ],
        suppression: "`# rigor:disable global.readonly-write` on the write.",
        severity_authored: "error",
        severity_by_profile: [("lenient", "error"), ("balanced", "error"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.4.0",
    },
    Entry {
        id: "global.write-type-mismatch",
        summary: "A special global is assigned a literal its setter rejects (TypeError at runtime).",
        fires_when: &[
            "The write is `$g = literal` to a special whose setter checks the value: `$/` / `$-0`, `$,` and `$\\` (a String or nil), `$;` / `$-F` (a String, a Regexp, nil, or an object with `to_str`), `$~` (a MatchData or nil), `$0` / `$PROGRAM_NAME` (a String or an object with `to_str`), `$.` (an Integer, a Float, or an object with `to_int`), `$-i` (a String, nil, false, or an object with `to_str`), and `$stdout` / `$>` / `$stderr` (an object that responds to `write`, ADR-117 WD2).",
            "The value is a literal node — an Integer, Float, Rational or imaginary literal, a String, Symbol or Regexp literal (interpolated or not), an Array or Hash literal, or `nil` / `true` / `false`, parenthesised or not — whose class the setter does not take: `$/ = 1`, `$/ = /x/`, `$~ = \"x\"`, `$0 = nil`, `$. = \"3\"`, `$stdout = 1`.",
            "Where the setter also converts (`to_str`, `to_int`) or asks for `write`, the literal's object cannot answer that method: RBS gives its class neither the method nor a `method_missing` / `respond_to_missing?` / `respond_to?` of its own, and the program defines none of them anywhere.",
            "The envelope is the interpreter's setter, not the global's RBS declaration: `$/ = /x/` fires although `$;` takes a Regexp.",
        ],
        does_not_fire_when: &[
            "The special's setter accepts every value: `$stdin` (never checked, ADR-117 WD2), `$_`, `$VERBOSE`, `$DEBUG`, `$=`; nor `$@`, whose setter depends on whether `$!` is set.",
            "The value is not a literal — a variable, a method call, a constant, a conditional — whatever its inferred type: a class RBS declares without `write` or `to_str` does not prove the object lacks it.",
            "RBS gives the literal's class the method or a hatch of its own (a `sig/` reopening or `include` counts).",
            "The program — any project or `pre_eval:` file — defines the method or a `method_missing` / `respond_to_missing?` / `respond_to?` anywhere, in any spelling (`def`, `define_method`, `define_singleton_method`, `alias`, `alias_method`, `attr_*`, a delegation macro, a string `eval`, the same through `send`) and on any receiver, or defines a method whose name no literal spells (a computed `define_method` or `send` name, a string `eval` whose code is not a plain literal, a name whose bytes are not valid in its encoding). This is blanket: no census of where a definition lands can be complete, so the literal of every class declines, whatever the definition's owner.",
            "An ancestor's surface is rewritten from outside (`Integer.include(M)`, `Object.include(M)`, a string `class_eval`), or a top-level `include` / `prepend` / `extend`, or an ancestor's `include`, names a module RBS does not rule out.",
            "For `$stdout` / `$>` / `$stderr`, a refinement that may add `write`, to any class, and a `using` in effect at the write. Which class it refines is not followed. The `to_str` / `to_int` conversions, and a refined `respond_to_missing?` / `respond_to?`, ignore refinements, so those still fire.",
            "Any project or `pre_eval:` file aliases the special (`alias $stdout $out`, on either side).",
            "The write is `$g op= v`, `$g ||= v`, `$g &&= v`, a multiple-assignment target, a `for` index, or a `rescue => $g` reference: their value is not type-checked.",
            "The write sits in the dead arm of a decidable version guard. Other code that never runs (`if false`, after `return`) is not recognised, and a write there still fires.",
        ],
        suppression: "`# rigor:disable global.write-type-mismatch` on the write.",
        severity_authored: "error",
        severity_by_profile: [("lenient", "warning"), ("balanced", "error"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.4.0",
    },
    Entry {
        id: "plugin.api-surface",
        summary: "Plugin code calls a member a `Rigor::Plugin` interface does not declare.",
        fires_when: &[
            "The call's receiver is typed by a `Rigor::Plugin` interface (`_Scope`, `_Environment`, `_Services`, `_FactStore`, `_Configuration`), possibly with `nil`: a hook's `scope`, `scope.environment`, `services.fact_store`, and so on.",
            "The interface does not declare the called method, and neither does `Object` (with `Kernel` and `BasicObject`).",
            "The call sits inside a class whose superclass chain reaches `Rigor::Plugin::Base`.",
            "The `plugin-api-surface` bleeding-edge feature is enabled, or a `severity_overrides:` key names `plugin.api-surface` (the rule resolves `:off` otherwise; a dot-free `plugin` key does not reach it).",
        ],
        does_not_fire_when: &[
            "The interface declares the method (`scope.type_of(node)`), or every object answers it (`scope.nil?`, `scope.respond_to?(...)`). A call that a `respond_to?` guard protects still fires.",
            "The value lost its interface type: a join with an untyped value, a helper parameter with no signature, or the result of a `&.` call on a value that may be nil. It reads as `untyped` there.",
            "The call is outside a `Rigor::Plugin::Base` subclass, or inside `Rigor::Plugin::Base` itself.",
            "The `plugin-api-surface` bleeding-edge feature is not enabled (the default).",
        ],
        suppression: "`# rigor:disable plugin.api-surface` on the call line, or `disable: [\"plugin.api-surface\"]` in `.rigor.yml`. The fix is to call a member the interface declares or a public namespace (`Rigor::Reflection`, ...).",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "off"), ("balanced", "off"), ("strict", "off")],
        evidence_tier: Some("high"),
        since: "0.4.8",
    },
    Entry {
        id: "rbs.contradicting-signature",
        summary: "Two declarations of one method provably contradict: no value or call satisfies both.",
        fires_when: &[
            "A method is declared both in the project's `sig/` and by an inline `# @rbs` / `#:` annotation, and the return or a parameter every call must pass holds two proven-disjoint types — distinct literals, a literal outside such a class, or two classes written absolutely whose RBS comes from Ruby core or the stdlib, neither an RBS ancestor of the other (`::String` against `::Integer`) — or the two keyword-free declarations accept positional counts that cannot meet, or one requires a keyword the other cannot take in any form. Several overloads are paired by correspondence, never by order. Positioned at the `.rbs` member; Rigor reads that declaration.",
            "A member-level `%a{rigor:v1:return: …}` or `%a{rigor:v1:param: …}` refinement is proven disjoint from the type its own member declares, in every overload it applies to (`rigor:v1:return: positive-int` on `-> String`).",
        ],
        does_not_fire_when: &[
            "One declaration refines the other (`String` against `non-empty-string`, `Symbol` against `:asc | :desc`, `bool` against `TrueClass`): the two merge to the more precise one, silently.",
            "Either side is `untyped`, `void` or `top` in that position — consistent with everything.",
            "Rigor cannot prove the two contradict: a module or interface on either side, a class the project declares (in `sig/`, inline or only in Ruby), a gem's class, any relative class name, two element types of one generic class, a type alias, `self`, a type variable, a proc, an optional or rest position, any block position, overloads that do not pair one to one, or differently shaped parameter lists that still overlap. The inline signature is then dropped and reported as `source-rbs-annotation-not-honoured` (info).",
            "A refinement is written on one overload rather than on the member: call sites do not honour it.",
            "Two `.rbs` files declare the same method (that fails the class's definition build, `rbs.coverage.definition-build-failed`), or a project `.rbs` collides with bundled RBS (the file is quarantined, `rbs.coverage.quarantined-signature`).",
        ],
        suppression: "`disable: [\"rbs.contradicting-signature\"]` in `.rigor.yml`, or a baseline entry. A `# rigor:disable` comment does not reach a row positioned in a `.rbs` file. The real fix is to regenerate the stale signature or correct the annotation.",
        severity_authored: "error",
        severity_by_profile: [("lenient", "error"), ("balanced", "error"), ("strict", "error")],
        evidence_tier: Some("high"),
        since: "0.4.0",
    },
    Entry {
        id: "static.value-use.void",
        summary: "A value recovered from an author-declared `-> void` return is used in value context.",
        fires_when: &[
            "A call resolves through direct RBS dispatch (the receiver's own resolvable class) to a method whose selected overload declares a `-> void` return.",
            "That call node sits in a value position: an assignment right-hand side (`x = obj.log(...)`), a call's explicit receiver (`obj.log(...).inspect`), or a call's positional argument.",
            "The `use-of-void-value` bleeding-edge feature is enabled (the rule resolves `:off` otherwise).",
        ],
        does_not_fire_when: &[
            "The void call is a bare statement (statement context) — a `void` result is accepted there.",
            "The value is a legitimate `top` (an author-declared `-> top` return) or any non-void type — only an author-declared `-> void` is recorded, so a real `top` never fires.",
            "The `-> void` return was resolved through the Object / user-class ancestor fallback rather than direct dispatch (the transitive / ancestor case is deferred, ADR-100 WD4).",
            "The `use-of-void-value` bleeding-edge feature is not enabled (the default).",
        ],
        suppression: "`# rigor:disable static.value-use.void` on the call line, or `disable: [\"static.value-use.void\"]` in `.rigor.yml`.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "off"), ("balanced", "off"), ("strict", "off")],
        evidence_tier: Some("high"),
        since: "0.3.0",
    },
    Entry {
        id: "suppression.empty",
        summary: "A `# rigor:disable[-file]` comment lists no rules.",
        fires_when: &[
            "A comment is exactly the bare marker (`# rigor:disable` / `# rigor:disable-file`) with nothing but whitespace or commas after it.",
            "Such a marker suppresses nothing — the author almost certainly meant to name rules or `all`.",
        ],
        does_not_fire_when: &[
            "At least one token follows the marker (each token is then checked by `suppression.unknown-rule` instead).",
            "The marker does not open the comment (documentation prose quoting the syntax).",
            "Non-token text follows the marker (documentation prose mentioning the syntax).",
        ],
        suppression: "Complete the marker (`# rigor:disable <rule>` / `all`) or delete it; `disable: [\"suppression.empty\"]` in `.rigor.yml`.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "warning"), ("balanced", "warning"), ("strict", "warning")],
        evidence_tier: Some("high"),
        since: "0.3.0",
    },
    Entry {
        id: "suppression.unknown-marker",
        summary: "A comment uses a suppression marker Rigor does not recognise (`rigor:disable-next-line`, `rigor:enable`, ...).",
        fires_when: &[
            "A comment OPENS with `rigor:disable-<suffix>` for a suffix other than `file`, or with `rigor:enable[-<suffix>]` — typically the RuboCop reflex `# rigor:disable-next-line <rule>` — followed by nothing or a rule-list-shaped remainder.",
            "Such a marker is invisible to the whole suppression grammar, so it silently suppresses nothing; Rigor's only markers are `# rigor:disable <rules>` (same line) and `# rigor:disable-file <rules>`.",
        ],
        does_not_fire_when: &[
            "The marker is one of the two recognised forms (their tokens are then checked by `suppression.unknown-rule` / `suppression.empty` instead).",
            "The marker does not open the comment (documentation prose quoting the spelling).",
            "Non-token text follows the marker (documentation prose mentioning the spelling).",
        ],
        suppression: "Rewrite as `# rigor:disable <rules>` on the offending line (Rigor has no next-line or enable form) or delete the comment; `disable: [\"suppression.unknown-marker\"]` in `.rigor.yml`.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "warning"), ("balanced", "warning"), ("strict", "warning")],
        evidence_tier: Some("high"),
        since: "0.3.0",
    },
    Entry {
        id: "suppression.unknown-rule",
        summary: "A `# rigor:disable[-file]` comment names a rule that does not exist.",
        fires_when: &[
            "A `# rigor:disable` / `# rigor:disable-file` marker carries a token that is not a canonical rule id, a legacy alias, `all`, or a family wildcard (`call` / `flow` / ...).",
            "The token is also not a known non-catalogue engine diagnostic (`rbs_extended.*`, `dynamic.*`, `rbs.*`, `pre-eval.*`, or a bare engine id such as `load-error`).",
            "Typically a typo — `call.undefined-metod` — leaving the suppression silently ineffective.",
        ],
        does_not_fire_when: &[
            "The token resolves (canonical id, legacy alias, `all`, family wildcard, known engine id).",
            "The token starts with `plugin.` — plugins load dynamically, so their rule vocabulary cannot be enumerated statically and under-warning is the FP-safe direction.",
            "The marker does not open the comment (documentation prose quoting the syntax, a doubled `##` comment, or an `=begin` block) — that is not parsed as a suppression either.",
            "The comment merely mentions the marker followed by non-token text (documentation prose like \"`# rigor:disable <rule>` comments\") — that is not parsed as a suppression either.",
        ],
        suppression: "Fix or remove the dead token; `# rigor:disable suppression.unknown-rule` on the same line, or `disable: [\"suppression.unknown-rule\"]` in `.rigor.yml`.",
        severity_authored: "warning",
        severity_by_profile: [("lenient", "warning"), ("balanced", "warning"), ("strict", "warning")],
        evidence_tier: Some("high"),
        since: "0.3.0",
    },
];

// ---------------------------------------------------------------------------
// `rigor sig-gen` skip reasons — the second catalogue `explain` consults
// (reference `SigGen::SkipReasonCatalog`, ADR-108 WD4).
//
// A `sig.skipped.*` id is an ANSWER ("Rigor cannot prove a type here"), and the
// command that exists to answer identifiers must be able to answer it: it
// resolves BEFORE the rule catalogue, carries no severity / profile /
// suppression (a skip reports a signature that was NOT written), and renders
// its own text and JSON shapes.
// ---------------------------------------------------------------------------

/// One skip-reason entry — mirrors the reference's `SkipReasonCatalog::Entry`
/// (`id`, `summary`, `explanation`, `next_step`).
struct SkipEntry {
    /// The `sig.skipped.*` identifier `sig-gen` prints on a skipped row.
    id: &'static str,
    /// Single-line headline (the index column text).
    summary: &'static str,
    /// Why the signature was withheld (rendered after the boilerplate note).
    explanation: &'static str,
    /// What the user can do about it (rendered under "What to do:").
    next_step: &'static str,
}

/// The skip-reason catalogue — content mirrors the reference's
/// `SkipReasonCatalog::ENTRIES` verbatim, in the same insertion order (the
/// index prints [`SKIP_ENTRIES`] unsorted, unlike the rule catalogue).
const SKIP_ENTRIES: &[SkipEntry] = &[
    SkipEntry {
        id: "sig.skipped.untyped-return",
        summary: "The method's return types as Dynamic[top], so no signature was written.",
        explanation: "Rigor inferred the body but the value it returns has no proven type — the last expression widened to Dynamic[top] somewhere up its chain. Emitting `untyped` here would record noise as a contract: the file would gain a signature that says nothing and stops a later, better inference from being noticed. This is the commonest skip reason on a project whose dependencies have no RBS.",
        next_step: "Do not write the return type by hand — the skip is the finding. Trace the returned expression back to where it became dynamic (`rigor annotate PATH` reads the `#=>` column, `rigor trace --format=json --line=N` for one line) and close THAT: install or write RBS for the dependency, enable the plugin for the framework, or report an engine gap.",
    },
    SkipEntry {
        id: "sig.skipped.user-authored",
        summary: "An RBS declaration for this method already exists and was left alone.",
        explanation: "Not a gap. The project already states a type for this member, and sig-gen does not overwrite a hand-authored contract without being told to. Where `sig/` and an inline annotation both declare the member, ADR-112 WD5's consistency rule decides which binds.",
        next_step: "Read the existing declaration. If it disagrees with what Rigor infers, raise the disagreement rather than silently retyping it; pass `--overwrite` only once you have decided the generated type is the better contract.",
    },
    SkipEntry {
        id: "sig.skipped.unrenderable-rbs",
        summary: "Rigor rendered a line that `rbs` itself rejects, so the method was dropped.",
        explanation: "A defect in Rigor's renderer, not in your code. The method is skipped rather than written so one pathological signature cannot make the whole generated file unparseable, and the count is reported so the defect is not silent.",
        next_step: "Report it, with the method and the file. `rigor sig-gen --print PATH` shows what was generated for the rest of the file and is the useful attachment.",
    },
    SkipEntry {
        id: "sig.skipped.complex-shape",
        summary: "Reserved for a shape too complex to render; no code path produces it today.",
        explanation: "The identifier is part of the reserved `sig.skipped.*` family and is carried so the family stays stable, but no generator path emits it.",
        next_step: "If you have actually seen this id in output, that is itself worth reporting — it means a path exists that this catalogue does not describe.",
    },
    SkipEntry {
        id: "sig.skipped.unresolvable-superclass",
        summary: "The class's superclass chain does not end in a class the RBS environment knows.",
        explanation: "Writing the declaration would make things worse rather than better: RBS would read the class as rooted at an unknown parent and collapse it on the next run, losing the members it can resolve today.",
        next_step: "Give the environment the missing ancestor — install or write RBS for the gem that defines it, or add the project file that declares it to the analysed paths — then re-run `rigor sig-gen`.",
    },
    SkipEntry {
        id: "sig.skipped.inline-declared",
        summary: "The member is declared inline, and `sig_gen.inline_declared: skip` keeps it out of `sig/`.",
        explanation: "Not a gap. The project asked sig-gen to leave every method its inline `# @rbs` / `#:` reader declares out of `sig/`, the setting for a project whose Steep reads the same annotations (`inline: true` beside `signature \"sig\"`), where a `sig/` copy would declare the method twice (`DuplicatedMethodDefinition`). Rigor still reads the inline declaration when it analyses the source; a consumer reading only the shipped `sig/` does not see this method.",
        next_step: "Nothing, if the setting is intended. To ship the member in `sig/`, remove `sig_gen.inline_declared: skip` from `.rigor.yml` and stop Steep from reading the annotations of the files you generate for.",
    },
    SkipEntry {
        id: "sig.skipped.inline-generic-class",
        summary: "The class is generic by an inline declaration, and sig-gen does not write type parameters.",
        explanation: "The class (or one it is nested in) takes type parameters from `# @rbs generic`, and `sig/` either does not declare it yet or declares it with differently named parameters. sig-gen would have to open it with a header that has no type parameters, which rbs rejects (`GenericParameterMismatchError`) — the class, and every class whose signature mentions it, would fail its definition build and read `Dynamic[top]` — or write members whose `T` the `sig/` declaration does not bind. The inline declaration still binds when the source is analysed.",
        next_step: "Declare the class in `sig/` with the same type parameters as the inline declaration (`class Box[T]` ... `end`) and re-run; sig-gen then writes the members into it.",
    },
    SkipEntry {
        id: "sig.skipped.generic-reopening",
        summary: "The method reopens a generic class whose type parameters sig-gen cannot copy onto a header.",
        explanation: "The class (or one it is nested in) is declared generic by loaded RBS — core, stdlib, a gem or `sig/` — and one of its type parameters has an upper bound or a default. sig-gen copies a declaration's type parameters onto the header it writes only when every one is plain, because a bound or default is a type spelled relative to the declaring file and can name a different class under another nesting. A header without the parameters, or with ones that differ, is rejected by rbs (`GenericParameterMismatchError`): the class, and every class whose signature mentions it, would fail its definition build.",
        next_step: "Declare the class in `sig/` with the declaration's own type parameters (`class Box[T < Comparable]` ... `end`) and re-run; sig-gen then writes the members into that declaration.",
    },
    SkipEntry {
        id: "sig.skipped.inline-differs",
        summary: "The method's inline declaration and its `sig/` declaration disagree; neither was changed.",
        explanation: "The method is declared inline by `# @rbs` / `#:` and in `sig/` (a `def` or an `attr_*`), and the two do not state the same types, or `sig/` lacks an annotation written inline. Parameter names, union spelling and a `::` that does not change what a name resolves to are ignored; overload order is not, because RBS answers a call with the first overload that matches. For a parameter-only annotation (`# @rbs name: T`, no `return:`) only the parameters are compared, and the return follows the ordinary proposal rules. sig-gen does not presume either side right, so it refuses, and `--write` and `--check` exit 1.",
        next_step: "Decide which declaration is right. To keep the inline one, re-run with `--overwrite`, which replaces the whole `sig/` member with it (an overload written only in `sig/` is dropped); to keep the `sig/` one, edit the annotation to match, or delete it. A parameter-only annotation whose parameters differ stays refused under `--overwrite`, because its return would be inferred under the `sig/` parameters: delete the `sig/` member and re-run, or add `# @rbs return:` so nothing is inferred.",
    },
    SkipEntry {
        id: "sig.skipped.overridden-by-unsigned-subclass",
        summary: "A subclass overrides this method and its override was not itself emitted.",
        explanation: "A declaration on the parent is inherited by every subclass that does not redeclare the member. Emitting it here would hand the subclass a type describing the parent's body, not the override the subclass actually runs.",
        next_step: "Generate signatures for the overriding subclass too — widen the `sig-gen` path set to cover it — and the parent's declaration becomes safe to emit.",
    },
    SkipEntry {
        id: "sig.skipped.overridden-bot",
        summary: "The method's body never returns, and the analysed code overrides the method.",
        explanation: "The inferred return is `bot`, but the analysed code defines the method again, with `def` or `define_method`: a subclass (signed or not), a class that includes the method's module, a module included later or prepended, or an extending class's own singleton method. `bot` describes only the base body the overrides replace, and every override returns a value, so the declaration would contradict each of them (#3029).",
        next_step: "Declare the return every implementation shares (often `untyped` or the overrides' common type) by hand in `sig/`.",
    },
    SkipEntry {
        id: "sig.skipped.abstract-method",
        summary: "The method is abstract: its body only raises an error asking for an implementation.",
        explanation: "The body ends in `raise NotImplementedError`, in an error class named `Abstract...` or `NotImplemented...`, or in a raise whose message asks for the method to be implemented, defined or overridden. Its inferred return is `bot`, but the method is meant to be replaced, and the replacements may live outside the analysed code, as in a plugin gem (#3029).",
        next_step: "Declare the return the implementations must provide by hand in `sig/`.",
    },
    SkipEntry {
        id: "sig.skipped.template-method",
        summary: "The method's `bot` return may come only from calling an overridden or abstract method.",
        explanation: "The inferred return is `bot`, and the body does not end in a direct `raise`, `fail`, `throw`, `exit` or `abort`. It calls a method the analysed code overrides or declares abstract, directly or through a chain of self-dispatched calls, so the `bot` may be only the base body of that call, which an override replaces at run time. The call is matched by name, whatever its receiver (#3029).",
        next_step: "Declare the return by hand in `sig/`, usually the return the overridden hook's implementations share.",
    },
    SkipEntry {
        id: "sig.skipped.top-level-def",
        summary: "A top-level `def` defines a private `Object` method, and sig-gen writes no signature for it.",
        explanation: "Not a gap in inference. A `def` outside any `class` or `module` has no class to declare the member under, and Ruby makes it a private method on `Object`. sig-gen reports it instead of dropping it silently, so a file that defines only top-level methods is not mistaken for one that was never read. Whether to emit it as a private `Object` method is undecided (#1676); `def self.x` and `def` inside a block body are not reported.",
        next_step: "Nothing is required. To type the method, move it into a module (`module_function` or `extend self` keeps the call sites), or write the `Object` declaration by hand in `sig/`.",
    },
    SkipEntry {
        id: "sig.skipped.uncertain-module-function",
        summary: "Whether the method is a module function depends on a visibility call that may not run.",
        explanation: "A bare `module_function` makes the module's later methods module functions, and a later bare `public`, `private` or `protected` ends that. Here such a call sits in control flow or a block between the two, so it may or may not run, and the method is either a module function (`def self?.name`) or an instance method only (`def name`). sig-gen writes neither rather than guess (#1968).",
        next_step: "Move the visibility call to the module body's own statements, or name the methods it should cover (`public :name`, `module_function :name`), so the reading is certain.",
    },
    SkipEntry {
        id: "sig.skipped.platform-guarded",
        summary: "The method is defined in a file loaded only under a platform guard, so its proposal was withheld.",
        explanation: "The file is `require`d / `require_relative`d only inside a conditional on `RUBY_ENGINE`, `RUBY_PLATFORM` or `defined?(…)`, so it is not an ordinary definition for every platform — the shape is `if RUBY_ENGINE == 'truffleruby'` guarding a pure-Ruby fallback whose real methods a C extension provides elsewhere. sig-gen withholds the proposal when another scanned file also defines the method (the guarded copy would compete with or duplicate the definition this platform loads), or when the inferred return is `bot` — the shape a raising stub takes, and never a contract worth writing from a file the current engine may never read (#3035).",
        next_step: "Declare the method for the platform the project targets — hand-write the `sig/` entry, or cover the file that defines it on this platform in the `sig-gen` paths — rather than taking the guarded file's stub as the contract.",
    },
];

/// A `sig.skipped.*` token → its skip-reason entry (reference
/// `SkipReasonCatalog.resolve`). Exact id only — `sig` / `sig.skipped` / an
/// unknown suffix resolve `None`, so the token falls through to the rule
/// catalogue and reads `Unknown rule`.
fn resolve_skip(token: &str) -> Option<&'static SkipEntry> {
    SKIP_ENTRIES.iter().find(|e| e.id == token)
}

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
    if let Some((_, canonical)) = rigor_rules::legacy_aliases()
        .iter()
        .find(|(legacy, _)| *legacy == token)
    {
        return ENTRIES.iter().filter(|e| e.id == *canonical).collect();
    }
    if rigor_rules::rule_families().contains(&token) {
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
/// the SAME resolution `explain` uses, skip reasons included — the reference's
/// `rigor_explain` tool shells out to `explain --format=json`, which answers
/// `sig.skipped.*` too), or `Err` when nothing resolves. Reuses the single
/// `ENTRIES` table — no duplication of catalogue content.
pub(crate) fn explain_json(query: Option<&str>) -> Result<serde_json::Value, String> {
    use serde_json::json;
    // A `sig.skipped.*` token resolves against the second catalogue first, as
    // `cmd_explain` does (reference `ExplainCommand#run`).
    if let Some(skip) = query.and_then(resolve_skip) {
        let entry = json!({
            "id": skip.id,
            "kind": "sig_skip_reason",
            "summary": skip.summary,
            "explanation": skip.explanation,
            "next_step": skip.next_step,
        });
        return Ok(json!({ "skip_reasons": [entry], "count": 1 }));
    }
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
    // Reference `ExplainCommand#parse_options`: one `%w[text json]` switch
    // (`--format=j` completes to `json`; a non-member is `invalid argument:`).
    const SWITCHES: &[crate::optparse::Switch] = &[crate::optparse::Switch::new(
        "format",
        &[("format", false)],
        crate::optparse::ArgStyle::Required,
        crate::optparse::ValueKind::Exact(&["text", "json"]),
        "--format",
        "=FORMAT",
        &["Output format (text | json). Default: text."],
    )];
    const PARSER: crate::optparse::OptParser =
        crate::optparse::OptParser::new("Usage: rigor explain [options] [<rule>]", SWITCHES);

    let items = match PARSER.parse(args).items_or_exit() {
        Ok(items) => items,
        Err(code) => return code,
    };
    let mut format = String::from("text");
    let mut positional: Vec<String> = Vec::new();
    for item in items {
        match item {
            Item::Positional(p) => positional.push(p),
            Item::Opt { key: "format", value, .. } => {
                format = value.unwrap().as_str().to_string();
            }
            _ => unreachable!("the switch table is closed"),
        }
    }
    // `@argv.shift` — the first positional; any extras are never read upstream.
    let token = positional.first().map(String::as_str);

    match token {
        None => {
            render_index(&format);
            ExitCode::SUCCESS
        }
        Some(tok) => {
            // Reference `ExplainCommand#run`: the skip-reason catalogue resolves
            // BEFORE the rule catalogue (ADR-108 WD4).
            if let Some(skip) = resolve_skip(tok) {
                render_skip_reason(skip, &format);
                return ExitCode::SUCCESS;
            }
            let entries = resolve(tok);
            if entries.is_empty() {
                eprintln!("Unknown rule: {tok}");
                eprintln!("Run `rigor explain` with no arguments to list every rule.");
                return ExitCode::from(64);
            }
            render_entries(&entries, &format);
            ExitCode::SUCCESS
        }
    }
}

/// The no-argument listing: each rule's id + one-line summary.
fn render_index(format: &str) {
    if format == "json" {
        println!("{}", json_array(&all()));
        // v0.4.7 `SHAPE_NOTICES[:explain_rules]` (ADR-126 WD4).
        eprintln!("{}", crate::json_document::explain_rules_notice());
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
    // The wildcard line is generated from the shared `RULE_FAMILIES` table, so
    // it can never lag the resolver's vocabulary again (rigor-rs#255).
    let wildcards = rigor_rules::rule_families()
        .iter()
        .map(|f| format!("`{f}`"))
        .collect::<Vec<_>>()
        .join(", ");
    println!("Family wildcards ({wildcards}) print every rule under that prefix.");
    println!();
    println!("`rigor sig-gen` skip reasons:");
    println!();
    for entry in SKIP_ENTRIES {
        // `ljust(33)`, like the reference's index column.
        println!("  {:<33} {}", entry.id, entry.summary);
    }
}

/// The full rendering of one `sig.skipped.*` entry (reference
/// `render_skip_reason`): its own shapes on both formats — a bare JSON OBJECT
/// (not an array) and a fixed boilerplate note where the severity table would
/// sit on a rule.
fn render_skip_reason(entry: &SkipEntry, format: &str) {
    if format == "json" {
        let fields: Vec<(&str, String)> = vec![
            ("id", jstr(entry.id)),
            ("kind", jstr("sig_skip_reason")),
            ("summary", jstr(entry.summary)),
            ("explanation", jstr(entry.explanation)),
            ("next_step", jstr(entry.next_step)),
        ];
        let lines: Vec<String> = fields
            .iter()
            .map(|(k, v)| format!("  {}: {}", jstr(k), v))
            .collect();
        println!("{{\n{}\n}}", lines.join(",\n"));
        eprintln!("{}", crate::json_document::explain_skip_reasons_notice());
        return;
    }
    println!("{}", entry.id);
    println!("{}", "=".repeat(entry.id.len()));
    println!();
    println!("{}", entry.summary);
    println!();
    println!("A `rigor sig-gen` skip reason, not a diagnostic rule: it reports a signature that was NOT");
    println!("written, so it has no severity and nothing to suppress.");
    println!();
    println!("{}", entry.explanation);
    println!();
    println!("What to do:");
    println!("  {}", entry.next_step);
}

/// Render the resolved entries (one for an id, many for a family).
fn render_entries(entries: &[&Entry], format: &str) {
    if format == "json" {
        println!("{}", json_array(entries));
        eprintln!("{}", crate::json_document::explain_rules_notice());
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
                "call.deprecated-ruby2-keywords",
                "call.possible-nil-receiver",
                "call.raise-non-exception",
                "call.self-undefined-method",
                "call.undefined-method",
                "call.unresolved-toplevel",
                "call.wrong-arity",
            ]
        );

        // The families rigor-rs#255 found unresolvable.
        assert_eq!(resolve("effect").len(), 4);
        assert_eq!(resolve("static").len(), 1);
        assert_eq!(resolve("global").len(), 2);
    }

    #[test]
    fn unknown_rule_resolves_empty() {
        assert!(resolve("bogus.rule").is_empty());
    }

    #[test]
    fn skip_reason_resolves_exact_ids_only() {
        let e = resolve_skip("sig.skipped.untyped-return").unwrap();
        assert_eq!(e.summary, "The method's return types as Dynamic[top], so no signature was written.");
        // Prefixes, unknown suffixes and rules are not skip reasons.
        assert!(resolve_skip("sig").is_none());
        assert!(resolve_skip("sig.skipped").is_none());
        assert!(resolve_skip("sig.skipped.bogus").is_none());
        assert!(resolve_skip("call.undefined-method").is_none());
        // The catalogue covers every `sig.skipped.*` id and keeps the
        // reference's insertion order (the index prints it unsorted).
        assert_eq!(SKIP_ENTRIES.len(), 16);
        assert_eq!(SKIP_ENTRIES[0].id, "sig.skipped.untyped-return");
        assert_eq!(SKIP_ENTRIES[15].id, "sig.skipped.platform-guarded");
    }

    #[test]
    fn documentation_url_anchors_on_dashed_id() {
        let e = resolve("call.undefined-method");
        assert_eq!(
            e[0].documentation_url(),
            "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-call-undefined-method"
        );
    }

    #[test]
    fn all_is_id_sorted_and_complete() {
        let ids: Vec<&str> = all().iter().map(|e| e.id).collect();
        // 36 rules: the reference's full `ALL_RULES` catalogue.
        assert_eq!(ids.len(), 36);
        // Sorted ascending by id.
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted);
    }

    /// rigor-rs#255 acceptance: the family tokens `explain` expands and the
    /// catalogue ids it lists both come from the `rigor-rules` tables, so the
    /// two cannot drift. Every shared family must expand to at least one
    /// entry, and the catalogue's id set must be exactly `ALL_CANONICAL_RULES`.
    #[test]
    fn catalogue_agrees_with_the_rules_crate_tables() {
        use std::collections::BTreeSet;
        for family in rigor_rules::rule_families() {
            assert!(
                !resolve(family).is_empty(),
                "family `{family}` expands to no catalogue entry"
            );
        }
        let entries: BTreeSet<&str> = ENTRIES.iter().map(|e| e.id).collect();
        let canonical: BTreeSet<&str> = rigor_rules::all_canonical_rules().iter().copied().collect();
        assert_eq!(entries, canonical);
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
