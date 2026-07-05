//! Diagnostic rules + the structured `Diagnostic` type (ADR-0014: rule id,
//! severity, primary/secondary annotations, subdiagnostics). All rules run in a
//! single converged AST walk (ADR-0005), not one pass per rule. The tracer
//! bullet's first rule is `call.undefined-method`.
#![allow(dead_code)]

use rigor_index::CoreIndex;
use rigor_infer::Typer;
use rigor_parse::{LoweredAst, Node};
use rigor_types::{Interner, Scalar, Type};

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
            documentation_url: "https://github.com/rigortype/rigor/blob/main/docs/manual/04-diagnostics.md#rule-call-undefined-method",
        }),
        CALL_WRONG_ARITY => Some(&RuleEntry {
            default_severity: Severity::Error,
            evidence_tier: "high",
            documentation_url: "https://github.com/rigortype/rigor/blob/main/docs/manual/04-diagnostics.md#rule-call-wrong-arity",
        }),
        CALL_POSSIBLE_NIL_RECEIVER => Some(&RuleEntry {
            // `error` under the default `balanced` profile (reference
            // severity_profile.rb), matching the sibling call.* rules whose
            // catalog default mirrors their balanced severity. An FP here would
            // be an ERROR on guarded code — hence the zero-FP decline scan.
            default_severity: Severity::Error,
            evidence_tier: "medium",
            documentation_url: "https://github.com/rigortype/rigor/blob/main/docs/manual/04-diagnostics.md#rule-call-possible-nil-receiver",
        }),
        CALL_UNRESOLVED_TOPLEVEL => Some(&RuleEntry {
            // Authored `:warning` (balanced), `:off` in lenient. Evidence tier
            // `low`: a firing is frequently a resolution gap (the defining file
            // is outside the analyzed set, or the method is metaprogrammed) that
            // routes to the `pre_eval:` review path, not a definite typo.
            default_severity: Severity::Warning,
            evidence_tier: "low",
            documentation_url: "https://github.com/rigortype/rigor/blob/main/docs/manual/04-diagnostics.md#rule-call-unresolved-toplevel",
        }),
        FLOW_DEAD_ASSIGNMENT => Some(&RuleEntry {
            default_severity: Severity::Warning,
            evidence_tier: "medium",
            documentation_url: "https://github.com/rigortype/rigor/blob/main/docs/manual/04-diagnostics.md#rule-flow-dead-assignment",
        }),
        DEF_OVERRIDE_VISIBILITY_REDUCED => Some(&RuleEntry {
            default_severity: Severity::Warning,
            // The oracle stamps this rule `high` (a purely structural Liskov
            // signature check over the project ancestor chain); mirror exactly.
            evidence_tier: "high",
            documentation_url: "https://github.com/rigortype/rigor/blob/main/docs/manual/04-diagnostics.md#rule-def-override-visibility-reduced",
        }),
        FLOW_UNREACHABLE_BRANCH => Some(&RuleEntry {
            default_severity: Severity::Warning,
            // The oracle stamps this `high` (a purely SYNTACTIC literal-predicate
            // check — no typer, no folding); mirror exactly.
            evidence_tier: "high",
            documentation_url: "https://github.com/rigortype/rigor/blob/main/docs/manual/04-diagnostics.md#rule-flow-unreachable-branch",
        }),
        FLOW_ALWAYS_RAISES => Some(&RuleEntry {
            // `error` — a provable `ZeroDivisionError` (the oracle stamps it
            // error / high). An FP here would be an ERROR on correct code, so the
            // decline gate in `check_always_raises` is intentionally strict.
            default_severity: Severity::Error,
            evidence_tier: "high",
            documentation_url: "https://github.com/rigortype/rigor/blob/main/docs/manual/04-diagnostics.md#rule-flow-always-raises",
        }),
        FLOW_ALWAYS_TRUTHY_CONDITION => Some(&RuleEntry {
            // The oracle stamps this `warning` / medium (an inferred-constant
            // predicate; the inferred counterpart to the high-evidence syntactic
            // `unreachable-branch`).
            default_severity: Severity::Warning,
            evidence_tier: "medium",
            documentation_url: "https://github.com/rigortype/rigor/blob/main/docs/manual/04-diagnostics.md#rule-flow-always-truthy-condition",
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
    let typer = Typer::with_source_and_folder(index, source, folder);
    let env = typer.build_toplevel_env(ast, interner);
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
                ..
            } => Some((
                id,
                *recv,
                method.clone(),
                args.clone(),
                !block_body.is_empty(),
                *message_span,
                *safe_nav,
            )),
            _ => None,
        })
        .collect();

    for (call_id, recv, method, args, has_block, message_span, safe_nav) in calls {
        // Rule precedence at one call site (avoid double-emit):
        //   1. undefined-method  (method absent on the receiver class, incl. nil)
        //   2. wrong-arity       (method present but arg count out of envelope)
        //   3. possible-nil-receiver (union receiver with a nil arm)
        // The reference emits exactly one of these per call; we mirror that by
        // returning the first that fires.
        let diag = check_call(ast, recv, &method, message_span, &env, &typer, interner, index)
            .or_else(|| {
                check_wrong_arity(ast, recv, &method, &args, has_block, message_span, &env, &typer, interner, index)
            })
            .or_else(|| {
                check_nil_receiver(
                    ast, call_id, recv, &method, message_span, safe_nav, &env, &typer, interner,
                    index,
                )
            })
            .or_else(|| {
                check_always_raises(
                    ast, recv, &method, &args, has_block, message_span, &env, &typer, interner,
                    index,
                )
            });
        if let Some(diag) = diag {
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
            ..
        } = node
        {
            dead_assignments_in_def(ast, def_id, def_name, body, *span, &mut out);
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

    out
}

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
/// Toplevel `Kernel` methods that the RUNTIME Ruby injects but the vendored
/// RBS does not model, so `class_has_method("Object", …)` misses them. The
/// reference resolves these via runtime reflection on `Object`; rigor-rs mirrors
/// that result with this small, FP-safe allowlist. `gem` is RubyGems' `Kernel#gem`
/// (the only core-only case the corpus FP audit surfaced). Extend as real signal
/// appears — never a false positive, only a missed witness if wrong.
const RUNTIME_KERNEL_TOPLEVEL: &[&str] = &["gem"];

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
    let scope_spans: Vec<rigor_parse::Span> = ast
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
            if source.is_toplevel_def(method) {
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

    // Project-`sig/`-declared class instance (ADR-0033): `Widget.new` types to a
    // source-registry `Nominal` that `class_name_of` (core-id only) will not
    // resolve, so recover the name from the source registry. Witness a typo ONLY
    // when the class was INTRODUCED by the project's OWN signatures
    // (`is_project_sig_class`) — the reference treats project sig as
    // authoritative, unlike a bundled stdlib/gem RBS class (`Pathname.new.typo`),
    // which stays lenient, and unlike an in-source-only class (not sig-declared),
    // which also stays lenient. `class_has_method` keeps its conservative gate
    // (an incomplete ancestor chain ⇒ `true` ⇒ silent), so this never fires on a
    // sig class whose charted super is unknown.
    if index.class_name_of(interner, recv_ty).is_none() {
        if let Some(name) = typer.source().class_name_for_id_of(interner, recv_ty) {
            if index.is_project_sig_class(name) && !index.class_has_method(name, method) {
                let name = name.to_string();
                let receiver_render = render_receiver(interner, recv_ty, &name);
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
    if !index.knows_class(class_name) {
        return None;
    }
    if index.class_has_method(class_name, method) {
        return None;
    }

    // We have witnessed absence over a core/RBS class.
    let class_name = class_name.to_string();

    // Render the receiver in the reference's value-in-message style: the bare
    // value for a `Constant` (`"Hello"`, `3`), else the class name. The
    // `message` field is presentation, not contract (ADR-0030).
    let receiver_render = render_receiver(interner, recv_ty, &class_name);
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
    has_block: bool,
    message_span: (usize, usize),
    env: &rigor_infer::TypeEnv,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
) -> Option<Diagnostic> {
    // A block selects a DIFFERENT RBS overload, which usually has a different
    // positional arity (`arr.select { } / arr.map { }` take 0 positional args,
    // but the no-block envelope spans the Enumerator overloads). The reference
    // DOES witness block-form arity by reading the block overload's own arity;
    // we only store a single arity envelope collapsed over ALL overloads, so we
    // cannot isolate the block overload's positional count here. Rather than
    // witness against the wrong (collapsed) envelope — which would risk a false
    // positive — we stay silent on arity for any block-bearing call. This is the
    // zero-FP-safe conservative choice (a missed witness, never an extra one);
    // block-form RETURN typing IS modeled (see `Typer::type_block_call`), so
    // chained undefined-method on a block result is still witnessed — only the
    // block-call's own arity is deferred until per-overload arity is stored.
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

/// Apply `call.possible-nil-receiver` to a single call with a receiver.
///
/// ## Slice 1 — the nilable-RBS-return slice with a conservative decline scan
///
/// The reference fires when flow analysis proves the receiver is `T | nil` on a
/// live path AND no guard narrowed nil away (ADR-58 + Slice-6 local narrowing).
/// rigor-rs has no flow scopes yet (ADR-0022 deferred), so this slice replaces
/// the full narrowing with a **whole-method-body syntactic DECLINE scan**: we
/// mint a `C | nil` receiver only from a CERTAIN nilable RBS return on a KNOWN
/// core class, and then DECLINE silently if ANY guard-like construct touches the
/// candidate local. Recall is intentionally small; **soundness (zero FP) is the
/// invariant** — this rule is error-severity, so an FP would be an error on
/// legitimately guarded code.
///
/// The firing conditions, in order (every `None`/decline is FP-safe):
/// 1. NOT a safe-nav call (`x&.foo` short-circuits on nil ⇒ not a bug —
///    reference clause 2).
/// 2. The receiver is a bare `LocalVariableRead x` (the only narrowing surface
///    the reference itself trusts; chained/method-call receivers are deferred).
/// 3. `x` is bound, within the enclosing `def`, by EXACTLY ONE assignment
///    `x = <call>` whose RHS call has a CERTAIN nilable RBS return on a KNOWN
///    core receiver class — yielding the single non-nil arm `C` (`C | nil`).
///    Nil is NEVER minted from a Dynamic / unknown / project receiver, nor from
///    a non-nilable return (the keystone: `method_return_nilable` carries the
///    `?` bit straight from RBS).
/// 4. The DECLINE scan finds NOTHING that guards/mutates `x` (see
///    [`nil_local_is_guarded`]).
/// 5. `method` is ABSENT on `NilClass` (else the call is sound on the nil arm —
///    `to_s`/`to_a`/`inspect`/`nil?`/… all live on NilClass and must not fire).
/// 6. `method` is PRESENT on `C` (the non-nil arm defines it — otherwise this is
///    `call.undefined-method`'s job, exactly one diagnostic per call site).
//
// TODO(spec): full nil-source coverage (T | nil params, `@ivar = nil` seeds,
// project-method nilable returns) + true flow narrowing needs ADR-0022 scopes;
// this slice deliberately models ONLY the core nilable-RBS-return nil-source.
#[allow(clippy::too_many_arguments)]
fn check_nil_receiver(
    ast: &LoweredAst,
    call_id: rigor_parse::NodeId,
    receiver: rigor_parse::NodeId,
    method: &str,
    message_span: (usize, usize),
    safe_nav: bool,
    env: &rigor_infer::TypeEnv,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
) -> Option<Diagnostic> {
    // (1) Safe-nav calls short-circuit on nil at runtime ⇒ never a bug.
    if safe_nav {
        return None;
    }

    // (2) Receiver must be a bare local read `x`.
    let Node::LocalVariableRead { name: x, .. } = ast.get(receiver) else {
        return None;
    };

    // (2b) Find the enclosing named `def` body (span-containment, the
    // `dead_assignments_in_def` pattern). The call's message span must lie
    // within exactly this def. Corpus nil-receiver hits all live in `def`
    // bodies; a top-level call has no enclosing def and is deferred (silent).
    let (def_span, def_body) = enclosing_def(ast, message_span)?;

    // (3) Resolve `x`'s nil-source: the single assignment `x = <call>` inside
    // this def whose RHS call has a CERTAIN nilable core RBS return ⇒ class `C`
    // (the single non-nil arm of `C | nil`). Anything less certain ⇒ None.
    let core_arm = nilable_local_core_arm(ast, x, def_span, env, typer, interner, index)?;

    // (4) The DECLINE scan — any guard/mutation touching `x` ⇒ silent.
    if nil_local_is_guarded(ast, x, def_span, &def_body, receiver) {
        return None;
    }

    // (5) The method must be ABSENT on NilClass (else sound on the nil arm).
    if index.class_has_method("NilClass", method) {
        return None;
    }

    // (6) The method must be PRESENT on the non-nil arm `C` (else this is
    // `call.undefined-method`'s call, not ours — one diagnostic per site).
    if !index.class_has_method(core_arm, method) {
        return None;
    }

    // Fire. Message is byte-exact with the reference's
    // `build_nil_receiver_diagnostic`: ``possible nil receiver: `m' is
    // undefined on NilClass``. Severity resolves to the catalog default
    // (`error` under balanced — matching the reference's severity_profile).
    let _ = call_id; // (call id reserved for future scoping; span is the anchor)
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

/// The enclosing named `def`'s `(span, body)` for a node at `inner_span`: the
/// SMALLEST `Node::Definition { name: Some(_) }` whose span contains
/// `inner_span`. `None` when no named def encloses it (a top-level call —
/// deferred in this slice). Smallest-enclosing handles nested defs correctly.
fn enclosing_def(
    ast: &LoweredAst,
    inner_span: (usize, usize),
) -> Option<(rigor_parse::Span, Vec<rigor_parse::NodeId>)> {
    let mut best: Option<(rigor_parse::Span, &[rigor_parse::NodeId])> = None;
    for (_id, n) in ast.iter() {
        if let Node::Definition {
            name: Some(_),
            body,
            span,
            ..
        } = n
        {
            if span_within(inner_span, *span) {
                let take = match best {
                    None => true,
                    // Prefer the tighter (smaller) enclosing span.
                    Some((b, _)) => (span.1 - span.0) < (b.1 - b.0),
                };
                if take {
                    best = Some((*span, body));
                }
            }
        }
    }
    best.map(|(s, b)| (s, b.to_vec()))
}

/// Resolve a local `x`'s nilable core arm WITHIN one `def`: if `x` is bound by
/// EXACTLY ONE plain assignment `x = <call>` (span-contained in `def_span`)
/// whose RHS call types to a KNOWN core receiver class on which the called
/// method has a CERTAIN nilable RBS return (`method_return_nilable` ⇒
/// `(C, true)`), return `Some(C)` — the single non-nil arm of `C | nil`.
///
/// This is the slice's ONLY nil-source. It is the zero-FP keystone: nil is
/// minted ONLY from a certain nilable-RBS-return on a known core class — never
/// from a Dynamic / unknown / project receiver, a non-nilable return, or any
/// non-call RHS. More than one assignment to `x`, or any assignment whose RHS
/// is not such a call, ⇒ `None` (decline; the multi-write case is also caught
/// by the guard scan, but we bail here first).
fn nilable_local_core_arm(
    ast: &LoweredAst,
    x: &str,
    def_span: rigor_parse::Span,
    env: &rigor_infer::TypeEnv,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
) -> Option<&'static str> {
    // Gather the plain `x = …` writes inside this def (op-writes excluded — they
    // read+write and are handled by the guard scan).
    let mut sources: Vec<rigor_parse::NodeId> = Vec::new();
    for (id, n) in ast.iter() {
        if let Node::LocalVariableWrite { name, span, .. } = n {
            if name == x && span_within(*span, def_span) {
                sources.push(id);
            }
        }
    }
    // Exactly one source assignment (a re-assignment defeats the single-arm
    // certainty and is also a guard-scan decline).
    if sources.len() != 1 {
        return None;
    }
    let Node::LocalVariableWrite { value, .. } = ast.get(sources[0]) else {
        return None;
    };

    // The RHS must be a Call `recv.m(...)` whose `recv` types to a KNOWN core
    // class and whose `m` has a CERTAIN nilable RBS return on that class.
    let Node::Call {
        receiver: Some(rhs_recv),
        method: rhs_method,
        ..
    } = ast.get(*value)
    else {
        return None;
    };
    // Type the RHS receiver using a method-body-local env (so `s = String.new;
    // s.byteslice(..)` resolves `s` to `Nominal[String]` — the corpus shape).
    // SCOPED to this rule (does not perturb other rules' top-level-only typing).
    let body_env = typer.build_method_body_env(ast, def_span, env, interner);
    let rhs_recv_ty = typer.type_of(ast, *rhs_recv, &body_env, interner);
    // CRITICAL parity gate (zero-FP keystone vs. the oracle's constant-folding):
    // the reference CONSTANT-FOLDS a literal-receiver core call (`"hi".byteslice`
    // ⇒ `"hi"`) to a concrete NON-nil value, so it never sees `C | nil` and stays
    // silent. rigor-rs does NOT fold, so it would mint a spurious union and fire
    // — an FP. We therefore mint nil ONLY from a NON-constant `Nominal` core
    // receiver (the unfoldable case, e.g. `String.new`, where the oracle DOES
    // type `C | nil` and fire). A `Constant` RHS receiver ⇒ decline.
    if matches!(interner.get(rhs_recv_ty), Type::Constant(_)) {
        return None;
    }
    let rhs_class = index.class_name_of(interner, rhs_recv_ty)?;
    if !index.knows_class(rhs_class) {
        return None;
    }
    // `(C, nilable)` — require nilable=true (a plain return mints NO nil).
    match index.method_return_nilable(rhs_class, rhs_method) {
        Some((core, true)) if index.knows_class(core) => Some(core),
        _ => None,
    }
}

/// The DECLINE scan (zero-FP keystone): `true` if ANY guard-like or mutating
/// construct touches local `x` anywhere in the `def` body, so the nil-receiver
/// rule must stay silent. We UNDER-approximate aggressively — declining costs
/// only recall, never soundness. `fire_use` is the firing receiver read, which
/// is the ONE use we expect and do not count against ourselves.
///
/// Declines on (mirroring every narrowing surface the reference has, plus a few
/// it does NOT narrow on — declining there only loses recall):
///   - a `.nil?` call on `x` anywhere (`x.nil?`);
///   - `x` appearing in ANY condition position — predicate of
///     `if`/`unless`/`while`/`until`/ternary (the `If`/`Loop` predicate), or an
///     operand of `&&`/`||` (`Logical` left/right);
///   - a safe-nav call on `x` anywhere (`x&.…`);
///   - any reassignment of `x` after the source (a second `LocalVariableWrite`
///     or any `LocalVariableOpWrite` incl. `||=`);
///   - `x` as receiver of `present?` / `blank?` / `presence` (the reference does
///     NOT narrow on these, so it would FIRE through them; declining here is the
///     safe under-approximation — loses recall, never an FP).
// collapsible_match: this is the zero-FP guard scan; each match arm carries its own
// explanatory comment and the Call arm holds two sequential guards. Folding the inner
// `if`s into arm guards would lengthen the guards and orphan the per-arm rationale.
#[allow(clippy::collapsible_match)]
fn nil_local_is_guarded(
    ast: &LoweredAst,
    x: &str,
    def_span: rigor_parse::Span,
    _def_body: &[rigor_parse::NodeId],
    fire_use: rigor_parse::NodeId,
) -> bool {
    // Helper: does node `id` resolve to a read of local `x`?
    let is_read_of_x = |id: rigor_parse::NodeId| -> bool {
        matches!(ast.get(id), Node::LocalVariableRead { name, .. } if name == x)
    };

    for (_id, n) in ast.iter() {
        match n {
            // Any op-write of x (`x ||= d`, `x += …`) is a reassignment/guard
            // ⇒ decline. (Plain re-writes are already excluded upstream:
            // `nilable_local_core_arm` requires EXACTLY ONE plain write of x, so
            // a two-write body never reaches the scan — the source resolver
            // returns None first.)
            Node::LocalVariableOpWrite { name, span, .. }
                if name == x && span_within(*span, def_span) =>
            {
                return true;
            }
            Node::Call {
                receiver: Some(recv),
                method,
                safe_nav,
                span,
                ..
            } if span_within(*span, def_span) && is_read_of_x(*recv) => {
                // Safe-nav on x, or a nil?/present?/blank?/presence guard on x.
                if *safe_nav {
                    return true;
                }
                if matches!(method.as_str(), "nil?" | "present?" | "blank?" | "presence") {
                    return true;
                }
            }
            // x in an if/unless/ternary predicate, or a while/until predicate.
            Node::If { predicate, span, .. } if span_within(*span, def_span) => {
                if predicate_mentions_local(ast, *predicate, x) {
                    return true;
                }
            }
            Node::Loop {
                predicate: Some(predicate),
                span,
                ..
            } if span_within(*span, def_span) => {
                if predicate_mentions_local(ast, *predicate, x) {
                    return true;
                }
            }
            // x as an operand of && / || (Logical).
            Node::Logical {
                left, right, span, ..
            } if span_within(*span, def_span) => {
                if predicate_mentions_local(ast, *left, x)
                    || predicate_mentions_local(ast, *right, x)
                {
                    return true;
                }
            }
            _ => {}
        }
    }

    let _ = fire_use;
    false
}

/// Whether the (possibly compound) condition node `cond` mentions a read of
/// local `x` anywhere in its subtree — span-contained. Used to detect `x` in a
/// predicate / logical-operand position. Span-scan (not structural recursion)
/// for the same orphan-proof reason as the dead-assignment collector: a read of
/// `x` lands in the arena regardless of any lossy lowering link, and lies within
/// the condition node's span. We OVER-detect (any read of x whose span is inside
/// the condition's span), which is FP-safe (over-declining loses only recall).
fn predicate_mentions_local(ast: &LoweredAst, cond: rigor_parse::NodeId, x: &str) -> bool {
    let cond_span = ast.get(cond).span();
    ast.iter().any(|(_, n)| {
        matches!(n, Node::LocalVariableRead { name, span }
            if name == x && span_within(*span, cond_span))
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
            Node::LocalVariableWrite { span, .. }
                if span_within(*span, def_span)
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
        // A `begin ... end` (and the lowered Statements wrapper, which uses the
        // same owned shape) — its last statement is the real trailing node.
        Node::BeginRescue { body, .. } | Node::Statements { body, .. } => match body.last() {
            Some(&inner) => descend_trailing(ast, inner),
            None => Some(id),
        },
        _ => Some(id),
    }
}

/// Render the receiver for the diagnostic message: the bare literal value for a
/// value-pinned `Constant`, else the resolved class name.
fn render_receiver(interner: &Interner, ty: rigor_types::TypeId, class_name: &str) -> String {
    match interner.get(ty) {
        Type::Constant(scalar) => render_scalar(scalar),
        _ => class_name.to_string(),
    }
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

use std::collections::{HashMap, HashSet};

/// The sentinel rule id of the synthetic internal-error diagnostic emitted on a
/// per-file panic (ADR-0016). Such diagnostics carry no real rule and MUST NEVER
/// be suppressed — they represent failures the user cannot silence away (matches
/// the reference's `rule == nil` guard in `filter_suppressed`).
const INTERNAL_ERROR_RULE: &str = "internal-error";

/// Family-wildcard tokens (`call`, `flow`, …). A token in this set expands to
/// every canonical rule whose id starts with `<token>.` (reference
/// `RULE_FAMILIES`). Only `call` can match an implemented rule today; the rest
/// are carried for forward-compat with the reference's catalogue.
const RULE_FAMILIES: &[&str] = &["call", "flow", "assert", "dump", "def"];

/// The canonical rule ids rigor-rs can actually emit. Family expansion and the
/// `disable all` wildcard are checked against this set, so a `call` family token
/// only ever expands to these three (the reference expands against its full
/// `ALL_RULES`, but the extra ids it would add match no rigor-rs diagnostic).
const IMPLEMENTED_RULES: &[&str] = &[
    CALL_UNDEFINED_METHOD,
    CALL_WRONG_ARITY,
    CALL_POSSIBLE_NIL_RECEIVER,
    FLOW_DEAD_ASSIGNMENT,
    DEF_OVERRIDE_VISIBILITY_REDUCED,
    FLOW_ALWAYS_RAISES,
    FLOW_UNREACHABLE_BRANCH,
];

/// The canonical rule ids rigor-rs can actually emit — the implemented coverage
/// scope, a SOUND SUBSET of the reference's catalogue (ADR-0008). Reported by
/// `rigor doctor` so users know which rules are live.
pub fn implemented_rules() -> &'static [&'static str] {
    IMPLEMENTED_RULES
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
        _ => None,
    }
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
    comments: &[(usize, String)],
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
    comments: &[(usize, String)],
) -> (HashMap<usize, SuppressSet>, SuppressSet) {
    let mut line_suppressions: HashMap<usize, SuppressSet> = HashMap::new();
    let mut file_suppressions = SuppressSet::default();

    for (line, text) in comments {
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
mod tests {
    use super::*;
    use rigor_parse::{lower, parse};

    fn run(src: &[u8]) -> Vec<Diagnostic> {
        let ast = lower(&parse(src));
        let mut interner = Interner::new();
        let index = CoreIndex::new();
        analyze(&ast, &mut interner, &index)
    }

    #[test]
    fn flags_typo_method_on_string_literal() {
        let src = b"s = \"Hello\"\ns.lenght\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        let d = &diags[0];
        assert_eq!(d.rule_id, CALL_UNDEFINED_METHOD);
        assert_eq!(d.message, "undefined method `lenght' for \"Hello\"");
        // Severity must be Error for undefined-method.
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.source_family, "builtin");
        // receiver_type matches the reference's rendering: the literal value
        // `"Hello"` (with surrounding double quotes), not the bare class name.
        assert_eq!(d.receiver_type.as_deref(), Some("\"Hello\""));
        assert_eq!(d.method_name.as_deref(), Some("lenght"));
        // The span must cover exactly `lenght`.
        assert_eq!(&src[d.start_offset..d.end_offset], b"lenght");
    }

    #[test]
    fn parenthesized_receiver_types_through_the_parens() {
        // `(15).frobnicate` — a parenthesized literal receiver types as its inner
        // Constant (parens are pure grouping), so undefined-method witnesses.
        // Real-corpus coverage-gap audit: closed ~13 undefined-method gaps.
        let diags = run(b"(15).frobnicate\n");
        assert_eq!(diags.len(), 1, "expected undefined-method, got {diags:?}");
        assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
        assert_eq!(diags[0].receiver_type.as_deref(), Some("15"));
        // A valid method through the parens stays silent.
        assert!(run(b"(15).succ\n").is_empty(), "valid method must be silent");
    }

    #[test]
    fn known_method_is_silent() {
        let diags = run(b"s = \"Hello\"\ns.length\n");
        assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
    }

    #[test]
    fn dynamic_receiver_is_silent() {
        // `@x` is an untyped ivar => Dynamic[top] => never guess. (An ivar, not a
        // bare `x`, so `call.unresolved-toplevel` — a separate rule — stays out.)
        let diags = run(b"@x.foo\n");
        assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
    }

    // --- call.possible-nil-receiver (the nilable-RBS-return slice) -----------

    /// Diagnostics filtered to just the nil-receiver rule.
    fn nil_diags(src: &[u8]) -> Vec<Diagnostic> {
        run(src)
            .into_iter()
            .filter(|d| d.rule_id == CALL_POSSIBLE_NIL_RECEIVER)
            .collect()
    }

    #[test]
    fn nil_receiver_fires_on_nilable_core_return_no_guard() {
        // `s : String` (via String.new), `s.byteslice -> String?` mints
        // `String | nil`; `upcase` is on String, absent on NilClass; no guard
        // ⇒ fire. Byte-exact with the oracle (verified against the reference:
        // line 4, col 5, error). The nil-source RHS receiver `s` is a
        // NON-constant Nominal (the unfoldable case the oracle also fires on).
        let src = b"def f\n  s = String.new\n  x = s.byteslice(0, 2)\n  x.upcase\nend\n";
        let diags = nil_diags(src);
        assert_eq!(diags.len(), 1, "expected one nil-receiver diag, got {diags:?}");
        let d = &diags[0];
        assert_eq!(d.rule_id, CALL_POSSIBLE_NIL_RECEIVER);
        assert_eq!(d.severity, Severity::Error, "balanced profile ⇒ error");
        assert_eq!(d.source_family, "builtin");
        assert_eq!(d.method_name.as_deref(), Some("upcase"));
        assert_eq!(
            d.message,
            "possible nil receiver: `upcase' is undefined on NilClass"
        );
        // Anchored on the method-name token `upcase`.
        assert_eq!(&src[d.start_offset..d.end_offset], b"upcase");
    }

    #[test]
    fn nil_receiver_silent_on_constant_receiver_oracle_folds() {
        // A LITERAL receiver (`"hello".byteslice`) is constant-folded by the
        // reference to a concrete non-nil value ⇒ it never sees `C | nil` and
        // stays silent. rigor-rs must NOT mint nil from a Constant RHS receiver
        // (the zero-FP keystone vs. the oracle's folding).
        let src = b"def f\n  x = \"hello\".byteslice(0, 2)\n  x.upcase\nend\n";
        assert!(
            nil_diags(src).is_empty(),
            "constant receiver must not mint nil (oracle folds it)"
        );
    }

    #[test]
    fn nil_receiver_silent_on_method_present_on_nilclass() {
        // `to_s` lives on NilClass ⇒ the call is sound on the nil arm ⇒ silent
        // (matches NilClass's tiny method set: to_s/to_a/inspect/nil?/…).
        let src = b"def f\n  s = String.new\n  x = s.byteslice(0, 2)\n  x.to_s\nend\n";
        assert!(nil_diags(src).is_empty(), "to_s is on NilClass ⇒ silent");
    }

    #[test]
    fn nil_receiver_silent_on_guards() {
        // Every guard form the decline scan recognizes ⇒ ZERO diagnostics
        // (each verified against the oracle, which narrows and stays silent).
        let prelude = "def f\n  s = String.new\n  x = s.byteslice(0, 2)\n";
        let cases: &[&str] = &[
            // `.nil?` guard then use.
            "  return if x.nil?\n  x.upcase\nend\n",
            // truthy guard via `unless`.
            "  raise unless x\n  x.upcase\nend\n",
            // x in an `if` predicate.
            "  if x then x.upcase end\nend\n",
            // x as a `&&` operand.
            "  x && x.upcase\nend\n",
            // safe-nav on x.
            "  x&.upcase\nend\n",
            // reassignment guarded by nil?.
            "  x = \"d\" if x.nil?\n  x.upcase\nend\n",
            // `||=` reassignment (op-write).
            "  x ||= \"d\"\n  x.upcase\nend\n",
        ];
        for tail in cases {
            let src = format!("{prelude}{tail}");
            let diags = nil_diags(src.as_bytes());
            assert!(
                diags.is_empty(),
                "guarded case must be silent:\n{src}\ngot {diags:?}"
            );
        }
    }

    #[test]
    fn nil_receiver_silent_on_dynamic_and_chained_receiver() {
        // RHS receiver is a method param (Dynamic) ⇒ no known core class ⇒ no
        // mint. And a chained `n.to_s.byteslice` (n.to_s is Dynamic) ⇒ silent.
        let param = b"def f(s)\n  x = s.byteslice(0, 2)\n  x.upcase\nend\n";
        assert!(nil_diags(param).is_empty(), "Dynamic RHS receiver ⇒ silent");
        let chained = b"def f(n)\n  x = n.to_s.byteslice(0, 2)\n  x.upcase\nend\n";
        assert!(nil_diags(chained).is_empty(), "chained Dynamic ⇒ silent");
    }

    #[test]
    fn nil_receiver_silent_on_non_nilable_return() {
        // `s.upcase -> String` (NOT nilable) ⇒ no nil minted ⇒ silent even
        // though `lenght` is absent (that path is undefined-method's job, and
        // here `length` is present so nothing fires at all).
        let src = b"def f\n  s = String.new\n  x = s.upcase\n  x.length\nend\n";
        assert!(
            nil_diags(src).is_empty(),
            "non-nilable return must not mint nil"
        );
    }

    #[test]
    fn flags_wrong_arity_on_string_include() {
        // `String#include?` is arity (1, 1); two args is wrong-arity.
        let src = b"s = \"x\"\ns.include?(\"a\", \"b\")\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
        let d = &diags[0];
        assert_eq!(d.rule_id, CALL_WRONG_ARITY);
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.source_family, "builtin");
        assert_eq!(d.receiver_type.as_deref(), Some("String"));
        assert_eq!(d.method_name.as_deref(), Some("include?"));
        assert_eq!(
            d.message,
            "wrong number of arguments to `include?' on String (given 2, expected 1)"
        );
        // Anchored on the method-name token `include?`.
        assert_eq!(&src[d.start_offset..d.end_offset], b"include?");
    }

    #[test]
    fn wrong_arity_renders_range_for_gsub() {
        // `String#gsub` is arity (1, 2); three args -> `expected 1..2`.
        let src = b"s = \"x\"\ns.gsub(\"a\", \"b\", \"c\")\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
        let d = &diags[0];
        assert_eq!(d.rule_id, CALL_WRONG_ARITY);
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.receiver_type.as_deref(), Some("String"));
        assert_eq!(d.method_name.as_deref(), Some("gsub"));
        assert_eq!(
            d.message,
            "wrong number of arguments to `gsub' on String (given 3, expected 1..2)"
        );
    }

    #[test]
    fn correct_arity_is_silent() {
        // 1-arg include?, 1-arg and 2-arg gsub are all within envelope.
        assert!(run(b"s = \"x\"\ns.include?(\"a\")\n").is_empty());
        assert!(run(b"s = \"x\"\ns.gsub(\"a\")\n").is_empty());
        assert!(run(b"s = \"x\"\ns.gsub(\"a\", \"b\")\n").is_empty());
    }

    #[test]
    fn nil_literal_receiver_is_undefined_method() {
        // `x = nil; x.upcase` — receiver types to Constant[Nil]; the reference
        // routes a definitely-nil receiver to `call.undefined-method`, not
        // `possible-nil-receiver`. We match that.
        let src = b"x = nil\nx.upcase\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
        let d = &diags[0];
        assert_eq!(d.rule_id, CALL_UNDEFINED_METHOD);
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.message, "undefined method `upcase' for nil");
        assert_eq!(&src[d.start_offset..d.end_offset], b"upcase");
    }

    #[test]
    fn no_false_positives_on_valid_code() {
        // A spread of valid calls across modeled classes must stay silent —
        // no arity, undefined-method, or nil diagnostics.
        assert!(run(b"s = \"x\"\ns.upcase\n").is_empty());
        assert!(run(b"n = 1\nn.abs\n").is_empty());
        assert!(run(b"s = \"hi\"\ns.gsub(\"a\", \"b\")\n").is_empty());
        // Dynamic (ivar) receiver with any arity stays silent (never guess).
        assert!(run(b"@x.foo(1, 2, 3)\n").is_empty());
        // A nullary call in its valid form stays silent.
        assert!(run(b"s = \"x\"\ns.chars\n").is_empty());
    }

    #[test]
    fn variadic_arity_method_does_not_fire() {
        // `String#concat` is variadic (`(*string | Integer) -> self`), so its
        // arity envelope has no upper bound => wrong-arity must NOT fire no
        // matter how many positional args are passed. (Real RBS now models a
        // concrete envelope for nearly every method; a variadic one is the case
        // where many args are still legal.)
        let diags = run(b"s = \"x\"\ns.concat(\"a\", \"b\")\n");
        assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
    }

    #[test]
    fn block_bearing_call_is_not_witnessed() {
        // `{...}.select { block }.keys` — `select` with a block returns a Hash
        // (`.keys` is valid), and `select` with a block takes 0 positional args
        // (no wrong-arity). Block-form RETURN typing is now modeled, but a VALID
        // chained call on the (correct) block result must still stay silent.
        let diags = run(b"h = {a: 1}\nx = h.select { |k, v| v > 0 }.keys\n");
        assert!(diags.is_empty(), "block-call chain must be silent, got {diags:?}");
        // The same chain without the witnessing chain still silent on the block call.
        let diags2 = run(b"[1, 2].each_with_index { |e, i| e }\n");
        assert!(diags2.is_empty(), "expected no diagnostics, got {diags2:?}");
        // The exact reported FP shape (gitlab-foss authorize_granular_scopes_service.rb:102):
        // a hash-literal-shorthand receiver chained DIRECTLY into `.select { }.keys`.
        // Two FPs must NOT fire: (a) wrong-arity on `select` (block ⇒ 0 positional
        // args, but the no-block envelope is 1..N — arity stays silent on block
        // calls), and (b) undefined-method `keys` on the block result (the block
        // form returns Hash, on which `keys` is valid). The reference is silent on
        // this whole line; rigor-rs must be too.
        let diags3 = run(
            b"def f(token, boundaries, permissions)\n{ token:, boundaries:, permissions: }.select { |_, value| value.nil? }.keys\nend\n",
        );
        assert!(diags3.is_empty(), "literal-receiver block chain must be silent, got {diags3:?}");
    }

    #[test]
    fn block_call_result_typo_is_witnessed() {
        // RECOVERED coverage (CURRENT_WORK §4): the block-form RETURN is now
        // RBS-modeled, so a typo on the CHAINED result is witnessed again,
        // matching the reference. Guarded on the real RBS tree (under the stub
        // fallback block returns are unmodeled ⇒ silent ⇒ no diagnostic to find).
        let idx = CoreIndex::new();
        if !idx.knows_class("Enumerable") || !idx.class_has_method("Array", "map") {
            return;
        }
        // `arr.map { }.frist` -> map block form returns Array; `.frist` undefined.
        let diags = run(b"arr = [1, 2, 3]\narr.map { |n| n + 1 }.frist\n");
        assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
        assert_eq!(diags[0].rule_id, "call.undefined-method");
        assert_eq!(diags[0].method_name.as_deref(), Some("frist"));

        // `arr.select { }.frist` -> Array; `.frist` undefined.
        let diags = run(b"arr = [1, 2, 3]\narr.select { |n| n > 1 }.frist\n");
        assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
        assert_eq!(diags[0].method_name.as_deref(), Some("frist"));

        // `arr.each { }.frist` -> `each` returns self (Array); `.frist` undefined.
        let diags = run(b"arr = [1, 2, 3]\narr.each { |n| n }.frist\n");
        assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
        assert_eq!(diags[0].method_name.as_deref(), Some("frist"));

        // `s.tap { }.lenght` -> `tap` returns self (String); `.lenght` undefined.
        let diags = run(b"s = \"hello\"\ns.tap { |x| x }.lenght\n");
        assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
        assert_eq!(diags[0].method_name.as_deref(), Some("lenght"));
    }

    #[test]
    fn in_source_return_chain_typo_is_witnessed() {
        // ADR-0023 tier-4b: `user.full_name.lenght` where `def full_name;
        // "#{a} #{b}"; end` infers full_name : String, so `.lenght` on the
        // String result is witnessed against the real String RBS.
        let src = b"class User\n  def full_name\n    \"#{first} #{last}\"\n  end\nend\nuser = User.new\nuser.full_name.lenght\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
        assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
        assert_eq!(diags[0].method_name.as_deref(), Some("lenght"));
        assert_eq!(diags[0].receiver_type.as_deref(), Some("String"));
    }

    #[test]
    fn in_source_return_chain_valid_call_stays_silent() {
        // The other side: a VALID method on the inferred core return must NOT
        // fire — `full_name : String`, and `.length` is valid on String.
        let src = b"class User\n  def full_name\n    \"#{first} #{last}\"\n  end\nend\nuser = User.new\nuser.full_name.length\n";
        let diags = run(src);
        assert!(diags.is_empty(), "valid String#length on the inferred return must be silent, got {diags:?}");
    }

    #[test]
    fn in_source_passthrough_param_return_is_witnessed() {
        // ADR-0023 tier-4b call-site PARAMETER BINDING: `def echo(x); x; end`
        // returns its arg's type, so `c.echo("a")` binds String and `.lenght`
        // witnesses against String — the reference witnesses the same call
        // (`undefined method 'lenght' for "a"`, same class, value-render aside).
        let src = b"class C\n  def echo(x)\n    x\n  end\nend\nc = C.new\nc.echo(\"a\").lenght\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
        assert_eq!(diags[0].method_name.as_deref(), Some("lenght"));
        assert_eq!(diags[0].receiver_type.as_deref(), Some("String"));
    }

    #[test]
    fn in_source_core_transform_param_return_is_witnessed() {
        // Core-transform via the param: `def up(x); x.upcase; end` returns the
        // core return of `String#upcase` (String) when the arg is a String, so
        // `.frob` on the result witnesses against String.
        let src = b"class C\n  def up(x)\n    x.upcase\n  end\nend\nc = C.new\nc.up(\"a\").frob\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
        assert_eq!(diags[0].method_name.as_deref(), Some("frob"));
        assert_eq!(diags[0].receiver_type.as_deref(), Some("String"));
    }

    #[test]
    fn in_source_param_bound_unknown_arg_is_silent() {
        // The decline side: a param-bound method whose ARG types Dynamic (an
        // unknown receiver's result) ⇒ no core class to bind ⇒ silent.
        let src = b"class C\n  def echo(x)\n    x\n  end\nend\nc = C.new\nc.echo(@whatever).lenght\n";
        let diags = run(src);
        assert!(diags.is_empty(), "param bound to an unknown-typed arg must stay silent, got {diags:?}");
    }

    #[test]
    fn in_source_splat_param_method_is_silent() {
        // A splat signature declines param binding entirely (no 1:1 index map),
        // so even a String arg does not witness — a missed witness, never an FP.
        let src = b"class C\n  def echo(*xs)\n    xs\n  end\nend\nc = C.new\nc.echo(\"a\").lenght\n";
        let diags = run(src);
        assert!(diags.is_empty(), "splat-param method must decline param binding, got {diags:?}");
    }

    #[test]
    fn block_call_result_valid_call_stays_silent() {
        // The other side of the recovery: a VALID method on the (correctly
        // modeled) block result must NOT fire — `Hash#select { }` returns Hash,
        // so `.keys` is valid (the FP class the placeholder originally guarded).
        let idx = CoreIndex::new();
        if !idx.knows_class("Enumerable") || !idx.class_has_method("Array", "map") {
            return;
        }
        // `h.select { }.keys` -> Hash#keys valid -> silent.
        let diags = run(b"h = { a: 1 }\nh.select { |k, v| v > 0 }.keys\n");
        assert!(diags.is_empty(), "Hash#select block result is Hash; .keys valid, got {diags:?}");
        // `h.reject { }.keys` -> Hash#reject block form returns Hash -> .keys valid.
        let diags = run(b"h = { a: 1 }\nh.reject { |k, v| v > 0 }.keys\n");
        assert!(diags.is_empty(), "Hash#reject block result is Hash; .keys valid, got {diags:?}");
        // `arr.map { }.first` -> Array#first valid -> silent.
        let diags = run(b"arr = [1, 2, 3]\narr.map { |n| n }.first\n");
        assert!(diags.is_empty(), "Array#map block result is Array; .first valid, got {diags:?}");
    }

    // --- in-source / non-core `.new` instances: reference leniency -----------
    //
    // The reference does NOT witness `undefined-method` on a project-defined
    // class instance, nor on a non-core `X.new` instance (Pathname/Set/Struct):
    // it gates on `rbs_class_known?` (check_rules.rb:556) and treats a miss there
    // leniently (ADR-0023 tier-4). rigor-rs mirrors that — these receivers are
    // typed (for chaining) but never witnessed. Every case below MUST be silent.

    #[test]
    fn in_source_instance_typo_is_silent_lenient() {
        // `class Point; def x; end; end; p = Point.new; p.y` — `y` is undefined on
        // Point, but Point is a project class (not RBS-known) ⇒ the reference stays
        // silent (leniency: Ruby defines methods dynamically). So must rigor-rs.
        let diags = run(b"class Point\n  def x\n  end\nend\np = Point.new\np.y\n");
        assert!(diags.is_empty(), "project-class miss must be silent, got {diags:?}");
    }

    #[test]
    fn defined_in_source_method_is_silent() {
        // `p.x` where Point defines `x` ⇒ no diagnostic (and silent regardless).
        let diags = run(b"class Point\n  def x\n  end\nend\np = Point.new\np.x\n");
        assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
    }

    #[test]
    fn inherited_object_method_on_source_instance_is_silent() {
        // `p.frozen?` — inherited from Object via the source class's implicit
        // super; must not be a false positive.
        let diags = run(b"class Point\n  def x\n  end\nend\np = Point.new\np.frozen?\n");
        assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
    }

    #[test]
    fn unknown_superclass_keeps_source_instance_silent() {
        // `class User < ApplicationRecord; end; u = User.new; u.anything` — silent
        // both because the super is unknown AND because a project class is never
        // witnessed. The zero-FP keystone for Rails models.
        let diags = run(
            b"class User < ApplicationRecord\nend\nu = User.new\nu.totally_made_up_xyz\n",
        );
        assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
    }

    #[test]
    fn source_subclass_typo_is_silent_lenient() {
        // `class Animal; def speak; end; end; class Dog < Animal; end` — neither
        // an inherited method nor a typo is witnessed on the project class `Dog`
        // (reference leniency), even though the chain Dog->Animal->Object is known.
        let ok = run(b"class Animal\n  def speak\n  end\nend\nclass Dog < Animal\nend\nd = Dog.new\nd.speak\n");
        assert!(ok.is_empty(), "inherited method must be silent, got {ok:?}");
        let bad = run(b"class Animal\n  def speak\n  end\nend\nclass Dog < Animal\nend\nd = Dog.new\nd.fly\n");
        assert!(bad.is_empty(), "project-class typo must be silent (leniency), got {bad:?}");
    }

    #[test]
    fn reopened_source_class_is_silent_lenient() {
        // A project class is never witnessed, reopened or not — including a typo.
        assert!(run(b"class C\n  def a\n  end\nend\nclass C\n  def b\n  end\nend\nc = C.new\nc.a\n").is_empty());
        let typo = run(b"class C\n  def a\n  end\nend\nclass C\n  def b\n  end\nend\nc = C.new\nc.zzz\n");
        assert!(typo.is_empty(), "project-class typo must be silent (leniency), got {typo:?}");
    }

    #[test]
    fn non_core_rbs_new_instance_typo_is_silent_lenient() {
        // `Pathname.new("a").nonexist` — Pathname is RBS-known but NOT a core
        // class round-tripped by id, so it resolves only through the registry
        // surface. The reference is silent on `Pathname.new.typo` (leniency on a
        // non-core `.new` instance); rigor-rs mirrors that — always silent.
        let diags = run(b"p = Pathname.new(\"a\")\np.nonexist\n");
        assert!(diags.is_empty(), "non-core .new instance miss must be silent, got {diags:?}");
    }

    #[test]
    fn metaclass_constructor_chained_new_is_silent() {
        // `Struct.new(:a, :b).new(1, 2)` — `Struct.new` returns a CLASS, not a
        // Struct instance; the chained `.new` must not be witnessed absent.
        let diags = run(b"Struct.new(:a, :b).new(1, 2)\n");
        assert!(diags.is_empty(), "Struct.new(...).new must be silent, got {diags:?}");
    }

    #[test]
    fn core_new_instance_typo_still_flags() {
        // The core `.new` path is still witnessed (matches the reference, which
        // flags `Array.new.bogus`): `Array` IS a core class round-tripped by id.
        let idx = CoreIndex::new();
        let diags = run(b"Array.new.bogus_xyz\n");
        if idx.knows_class("Array") {
            assert_eq!(diags.len(), 1, "expected core .new typo flagged, got {diags:?}");
            assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
            assert_eq!(diags[0].method_name.as_deref(), Some("bogus_xyz"));
        }
    }

    #[test]
    fn real_rbs_method_on_rbs_instance_is_silent() {
        // `Pathname.new("a").basename` — a real method, never a false positive.
        let diags = run(b"p = Pathname.new(\"a\")\np.basename\n");
        assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
    }

    #[test]
    fn dynamic_unknown_constant_new_is_silent() {
        // `Widget.new.foo` where Widget is unknown ⇒ Dynamic ⇒ silent.
        let diags = run(b"w = Widget.new\nw.foo\n");
        assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
    }

    // --- singleton (class-method) witnessing on bare constants ---------------
    //
    // A bare top-level RBS constant (`Time`, `Array`) types to `Singleton(C)`;
    // a class-method typo on it is witnessed (`Time.current`), while real class
    // methods, instance-only names, `.new`, and project-class collisions stay
    // silent. All guarded on real RBS being loaded (stub ⇒ assert silent).

    #[test]
    fn time_current_flags_singleton() {
        let idx = CoreIndex::new();
        let diags = run(b"Time.current\n");
        if idx.knows_class("Time") {
            assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
            let d = &diags[0];
            assert_eq!(d.rule_id, CALL_UNDEFINED_METHOD);
            assert_eq!(d.severity, Severity::Error);
            assert_eq!(d.message, "undefined method `current' for singleton(Time)");
            assert_eq!(d.receiver_type.as_deref(), Some("singleton(Time)"));
            assert_eq!(d.method_name.as_deref(), Some("current"));
        } else {
            assert!(diags.is_empty(), "stub fallback must be silent, got {diags:?}");
        }
    }

    #[test]
    fn time_real_class_methods_and_new_are_silent() {
        // `Time.now` / `Time.name` are real class methods; `Time.new` constructs
        // an instance (intercepted before singleton typing). All silent.
        assert!(run(b"Time.now\n").is_empty(), "Time.now must be silent");
        assert!(run(b"Time.name\n").is_empty(), "Time.name must be silent");
        assert!(run(b"Time.new\n").is_empty(), "Time.new must be silent");
    }

    #[test]
    fn array_wrap_flags_singleton_but_new_is_silent() {
        let idx = CoreIndex::new();
        // `Array.wrap` is an ActiveSupport extension, not core ⇒ flagged absent.
        // (`@x` ivar arg, not a bare `x`, so unresolved-toplevel stays out.)
        let diags = run(b"Array.wrap(@x)\n");
        if idx.knows_class("Array") {
            assert_eq!(diags.len(), 1, "expected Array.wrap flagged, got {diags:?}");
            assert_eq!(diags[0].message, "undefined method `wrap' for singleton(Array)");
            assert_eq!(diags[0].receiver_type.as_deref(), Some("singleton(Array)"));
        } else {
            assert!(diags.is_empty(), "stub fallback must be silent, got {diags:?}");
        }
        // `Array.new` constructs an instance ⇒ silent (not singleton-typed).
        assert!(run(b"Array.new\n").is_empty(), "Array.new must be silent");
    }

    #[test]
    fn project_class_collision_is_silent() {
        // A file that DEFINES `class Group` and also calls `Group.where(1)`: even
        // though `Group` may be a top-level RBS class, the project defines it, so
        // the gate refuses to singleton-type it ⇒ silent (cross-file zero-FP).
        let diags = run(b"class Group\nend\nGroup.where(1)\n");
        assert!(diags.is_empty(), "project-class collision must be silent, got {diags:?}");
    }

    #[test]
    fn secure_random_hex_is_silent_extend_surface() {
        // `SecureRandom.hex` — its class methods come via an `extend`ed module, so
        // the class-method surface is incomplete ⇒ conservative ⇒ silent.
        let diags = run(b"SecureRandom.hex\n");
        assert!(diags.is_empty(), "SecureRandom.hex must be silent, got {diags:?}");
    }

    // -- flow.dead-assignment --------------------------------------------
    //
    // Pure AST/structural; faithful port of `DeadAssignmentCollector`. Each test
    // mirrors a skip/fire case verified against the oracle.

    /// The single dead-assignment diagnostic in `src`, or panic if not exactly 1.
    fn dead(src: &[u8]) -> Vec<Diagnostic> {
        run(src)
            .into_iter()
            .filter(|d| d.rule_id == FLOW_DEAD_ASSIGNMENT)
            .collect()
    }

    #[test]
    fn dead_assignment_fires_on_genuine_dead_write() {
        // `def foo; result = 1; 77; end` — `result` is written, never read, and
        // not the trailing statement ⇒ fires. Byte-exact against the oracle.
        let src = b"def foo\n  result = 1\n  77\nend\n";
        let diags = dead(src);
        assert_eq!(diags.len(), 1, "expected one dead-assignment, got {diags:?}");
        let d = &diags[0];
        assert_eq!(d.rule_id, FLOW_DEAD_ASSIGNMENT);
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.source_family, "builtin");
        assert_eq!(d.receiver_type, None);
        assert_eq!(d.method_name, None);
        assert_eq!(d.message, "local `result' assigned in `foo' but never read");
        // Anchored on the NAME token `result` (col 3 in the oracle).
        assert_eq!(&src[d.start_offset..d.end_offset], b"result");
    }

    #[test]
    fn dead_assignment_trailing_write_is_silent() {
        // `def foo; result = 1; end` — the write IS the trailing statement
        // (implicit return) ⇒ silent.
        assert!(dead(b"def foo\n  result = 1\nend\n").is_empty());
    }

    #[test]
    fn dead_assignment_underscore_prefix_is_silent() {
        // `_unused` is intentionally-unused by convention ⇒ silent.
        assert!(dead(b"def foo\n  _unused = 1\n  77\nend\n").is_empty());
    }

    #[test]
    fn dead_assignment_op_write_read_is_silent() {
        // THE FP-GATE CASE: `total = 0; total += 1; other` — the op-write reads
        // `total`, so `total` is read ⇒ the plain write must NOT flag.
        let diags = dead(b"def f\n  total = 0\n  total += 1\n  other\nend\n");
        assert!(diags.is_empty(), "op-write read must suppress dead-assignment, got {diags:?}");
        // and the same for ||= / &&=.
        assert!(dead(b"def f\n  x = 0\n  x ||= 5\n  y\nend\n").is_empty());
        assert!(dead(b"def f\n  x = 0\n  x &&= 5\n  y\nend\n").is_empty());
    }

    #[test]
    fn dead_assignment_read_in_block_is_silent() {
        // A read inside a block body counts as a read ⇒ silent.
        let diags = dead(b"def f\n  x = 1\n  [1].each { |n| x }\n  77\nend\n");
        assert!(diags.is_empty(), "block read must suppress, got {diags:?}");
    }

    #[test]
    fn dead_assignment_read_in_interpolation_is_silent() {
        // A read inside string interpolation counts as a read ⇒ silent.
        let diags = dead(b"def f\n  x = 1\n  \"v=#{x}\"\n  77\nend\n");
        assert!(diags.is_empty(), "interpolation read must suppress, got {diags:?}");
    }

    #[test]
    fn dead_assignment_def_receiver_read_is_silent() {
        // A local used as a singleton-def RECEIVER (`def x.m`) IS read — the
        // receiver is evaluated in the enclosing scope. Real-corpus FP audit
        // (textbringer): before lowering the receiver, `x` looked assigned-but-
        // never-read here.
        let diags = dead(b"def f\n  x = Object.new\n  def x.m\n    1\n  end\n  77\nend\n");
        assert!(diags.is_empty(), "def-receiver read must suppress, got {diags:?}");
    }

    #[test]
    fn dead_assignment_block_pass_read_is_silent() {
        // A read inside a `&expr` block-pass argument counts as a read ⇒ silent.
        // Regression: a `&action` block-pass previously lowered to nothing, so the
        // `action` read never surfaced in the arena and the loop-condition write
        // was falsely flagged (gitlab-foss after_commit_queue.rb, matched vs the
        // v0.2.6 oracle which stays silent).
        let src = b"def f\n  while x = q.pop\n    g(&x)\n  end\nend\n";
        assert!(dead(src).is_empty(), "block-pass `&x` read must suppress, got {:?}", dead(src));
        // The direct form too: `foo(&blk)` after `blk = ...`.
        assert!(
            dead(b"def f\n  blk = make\n  run(&blk)\nend\n").is_empty(),
            "a `&blk` read must count"
        );
    }

    #[test]
    fn dead_assignment_nested_def_isolation() {
        // An OUTER write read only by an INNER def is a closure capture? No — a
        // nested `def` is a fresh scope, but the reference gathers READS with no
        // def barrier, so an inner read of `x` DOES count. Conversely the inner
        // def's OWN write `y` is scanned as its own unit and fires there. Here we
        // assert: outer `x` written+read-in-inner is silent; inner `y` dead fires
        // (one diagnostic, anchored in the inner def).
        let src = b"def outer\n  x = 1\n  def inner\n    y = 2\n    3\n  end\n  x\nend\n";
        let diags = dead(src);
        assert_eq!(diags.len(), 1, "expected one (inner y), got {diags:?}");
        assert_eq!(diags[0].message, "local `y' assigned in `inner' but never read");
        // And the outer write is NOT a candidate inside `inner` (def barrier on
        // writes): `def inner` body doesn't see outer `x`.
        assert!(!diags.iter().any(|d| d.message.contains("`x'")));
    }

    #[test]
    fn dead_assignment_multi_write_is_silent() {
        // `a, b = foo` lowers to `Node::Other` (no LocalVariableWrite) ⇒ never a
        // candidate ⇒ silent, matching the reference (MultiWriteNode skipped).
        let diags = dead(b"def f\n  a, b = bar\n  77\nend\n");
        assert!(diags.is_empty(), "multi-write must be silent, got {diags:?}");
    }

    #[test]
    fn dead_assignment_top_level_and_class_body_writes_are_silent() {
        // Top-level and class/module BODY assignments are never scanned (only
        // named def bodies are) ⇒ silent.
        assert!(dead(b"result = 1\n77\n").is_empty());
        assert!(dead(b"class C\n  CONST_LOCAL = 1\n  77\nend\n").is_empty());
    }

    #[test]
    fn dead_assignment_fires_inside_class_method_body() {
        // A genuine dead write inside a class instance method fires, named by the
        // method (`bar`), exactly once (no double-emit from the method_bodies
        // harvest).
        let src = b"class C\n  def bar\n    tmp = 1\n    99\n  end\nend\n";
        let diags = dead(src);
        assert_eq!(diags.len(), 1, "expected one, got {diags:?}");
        assert_eq!(diags[0].message, "local `tmp' assigned in `bar' but never read");
    }

    #[test]
    fn dead_assignment_read_after_write_is_silent() {
        // The basic positive-control: a write that IS later read stays silent.
        assert!(dead(b"def f\n  x = 1\n  x\n  77\nend\n").is_empty());
    }

    #[test]
    fn dead_assignment_begin_rescue_trailing_unwrapped() {
        // A method whose body is a `begin ... end` — the trailing statement is the
        // begin block's last statement. `result = 1` as that tail is an implicit
        // return ⇒ silent.
        let src = b"def f\n  begin\n    result = 1\n  end\nend\n";
        assert!(dead(src).is_empty(), "begin-wrapped trailing write must be silent");
    }

    // -- flow.unreachable-branch ------------------------------------------
    //
    // Pure SYNTACTIC/AST; faithful port of `unreachable_branch_diagnostic`. Each
    // case was verified byte-exact against the Ruby oracle. The keyword-inversion
    // (`if` vs `unless`) cases are the parity keystone: anchoring on the wrong
    // branch would land the diagnostic on LIVE code.

    /// The `flow.unreachable-branch` diagnostics in `src`, in source order.
    fn unreach(src: &[u8]) -> Vec<Diagnostic> {
        run(src)
            .into_iter()
            .filter(|d| d.rule_id == FLOW_UNREACHABLE_BRANCH)
            .collect()
    }

    /// 1-based (line, column) of a byte offset in `src` — the same coordinates the
    /// CLI/JSON reporter prints, so anchors can be asserted against the oracle.
    fn line_col(src: &[u8], offset: usize) -> (usize, usize) {
        let mut line = 1usize;
        let mut col = 1usize;
        for &b in &src[..offset] {
            if b == b'\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    #[test]
    fn unreachable_branch_if_false_anchors_dead_then() {
        // `if false…else…` — falsey predicate, THEN branch dead. Oracle: 2:3
        // (the dead then-branch's first statement), "always falsey".
        let src = b"if false\n  dead_then\nelse\n  live_else\nend\n";
        let d = unreach(src);
        assert_eq!(d.len(), 1, "expected one diagnostic, got {d:?}");
        assert_eq!(d[0].message, "unreachable branch: literal predicate is always falsey");
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(line_col(src, d[0].start_offset), (2, 3));
    }

    #[test]
    fn unreachable_branch_unless_false_anchors_dead_else() {
        // `unless false…else…` — the KEYWORD INVERTS: falsey predicate kills the
        // ELSE branch. Oracle: 3:1 (the `else` keyword), "always falsey".
        let src = b"unless false\n  live_then\nelse\n  dead_else\nend\n";
        let d = unreach(src);
        assert_eq!(d.len(), 1, "expected one diagnostic, got {d:?}");
        assert_eq!(d[0].message, "unreachable branch: literal predicate is always falsey");
        assert_eq!(line_col(src, d[0].start_offset), (3, 1));
    }

    #[test]
    fn unreachable_branch_if_true_anchors_dead_else() {
        // `if true…else…` — truthy predicate kills the ELSE branch. Oracle: 3:1
        // (the `else` keyword), "always truthy".
        let src = b"if true\n  live\nelse\n  dead\nend\n";
        let d = unreach(src);
        assert_eq!(d.len(), 1, "expected one diagnostic, got {d:?}");
        assert_eq!(d[0].message, "unreachable branch: literal predicate is always truthy");
        assert_eq!(line_col(src, d[0].start_offset), (3, 1));
    }

    #[test]
    fn unreachable_branch_if_nil_kills_then() {
        // `nil` is falsey ⇒ THEN dead, "always falsey".
        let src = b"if nil\n  dead_n\nelse\n  live_n\nend\n";
        let d = unreach(src);
        assert_eq!(d.len(), 1, "expected one diagnostic, got {d:?}");
        assert_eq!(d[0].message, "unreachable branch: literal predicate is always falsey");
        assert_eq!(line_col(src, d[0].start_offset), (2, 3));
    }

    #[test]
    fn unreachable_branch_truthy_literals_kill_else() {
        // Integer / String / Symbol literals are all truthy in Ruby (incl. `0`,
        // `""`) ⇒ ELSE dead, "always truthy". Verified against the oracle.
        for src in [
            b"if 5\n  a\nelse\n  b\nend\n".as_slice(),
            b"if \"x\"\n  a\nelse\n  b\nend\n".as_slice(),
            b"if :sym\n  a\nelse\n  b\nend\n".as_slice(),
        ] {
            let d = unreach(src);
            assert_eq!(d.len(), 1, "expected one diagnostic for {src:?}, got {d:?}");
            assert_eq!(
                d[0].message,
                "unreachable branch: literal predicate is always truthy"
            );
        }
    }

    #[test]
    fn unreachable_branch_if_false_no_else_anchors_then() {
        // `if false; dead; end` (no else) — THEN dead, still fires (no else node
        // is needed; the dead branch is the present, non-empty then). Oracle: 2:3.
        let src = b"if false\n  dead_only\nend\n";
        let d = unreach(src);
        assert_eq!(d.len(), 1, "expected one diagnostic, got {d:?}");
        assert_eq!(line_col(src, d[0].start_offset), (2, 3));
    }

    #[test]
    fn unreachable_branch_empty_dead_then_is_silent() {
        // `if false` with an EMPTY then but a live else — the dead (then) branch
        // is absent ⇒ DECLINE (verified silent in the oracle).
        let src = b"if false\nelse\n  live2\nend\n";
        assert!(unreach(src).is_empty(), "empty dead then must be silent");
        // `if false; end` — both branches empty ⇒ DECLINE.
        assert!(unreach(b"if false\nend\n").is_empty(), "no branches must be silent");
    }

    #[test]
    fn unreachable_branch_empty_else_node_still_fires() {
        // `if true…else[empty]` — truthy kills the ELSE branch; the `else` clause
        // NODE exists even though its body is empty, so the oracle FIRES at 3:1.
        let src = b"if true\n  live\nelse\nend\n";
        let d = unreach(src);
        assert_eq!(d.len(), 1, "empty-but-present else node must fire, got {d:?}");
        assert_eq!(line_col(src, d[0].start_offset), (3, 1));
    }

    #[test]
    fn unreachable_branch_non_literal_is_silent() {
        // Non-literal predicate (`if x`) ⇒ DECLINE.
        assert!(unreach(b"if x\n  a\nelse\n  b\nend\n").is_empty(), "variable predicate silent");
        // Constant predicate (`if DEBUG`) ⇒ DECLINE: the reference uses SYNTACTIC
        // literal detection, NOT the folder, so a constant never flags.
        assert!(
            unreach(b"if DEBUG\n  a\nelse\n  b\nend\n").is_empty(),
            "constant predicate must not fold ⇒ silent"
        );
        // Interpolated string (`"a#{x}"`) is NOT a plain literal ⇒ DECLINE.
        assert!(
            unreach(b"if \"a#{x}b\"\n  a\nelse\n  b\nend\n").is_empty(),
            "interpolated string predicate silent"
        );
    }

    #[test]
    fn unreachable_branch_while_true_is_silent() {
        // `while true` is a LOOP (a different rule's territory), not an If ⇒ this
        // rule is silent here.
        assert!(
            unreach(b"while true\n  loopy\nend\n").is_empty(),
            "while-true is not unreachable-branch"
        );
    }

    #[test]
    fn unreachable_branch_ternary_fires() {
        // Prism parses a ternary as an IfNode, so a literal-predicate ternary is
        // flagged too (verified against the oracle: `false ? a : b` fires falsey).
        let d = unreach(b"x = false ? aa : bb\n");
        assert_eq!(d.len(), 1, "literal-predicate ternary must fire, got {d:?}");
        assert_eq!(d[0].message, "unreachable branch: literal predicate is always falsey");
    }

    // -- flow.always-truthy-condition -------------------------------------
    //
    // The inferred-constant counterpart to unreachable-branch (ADR-0022 first
    // flow slice). Fires only when the dominating flow scope folds the predicate
    // to a `Type::Constant`; the branch-join is the zero-FP keystone. Each
    // positive was verified byte-exact (rule, line, column) against the oracle.

    /// The `flow.always-truthy-condition` diagnostics in `src`, in source order.
    fn always_truthy(src: &[u8]) -> Vec<Diagnostic> {
        run(src)
            .into_iter()
            .filter(|d| d.rule_id == FLOW_ALWAYS_TRUTHY_CONDITION)
            .collect()
    }

    #[test]
    fn always_truthy_literal_assigned_constant_fires() {
        // `ca = 5; if ca` — `ca` folds to `5` (dominating straight-line write) ⇒
        // always truthy. Oracle: 2:4 (the predicate node), "always truthy".
        let src = b"ca = 5\nif ca\n  puts ca\nend\n";
        let d = always_truthy(src);
        assert_eq!(d.len(), 1, "expected one diagnostic, got {d:?}");
        assert_eq!(
            d[0].message,
            "condition is always truthy (the surrounding flow proves it folds to a constant)"
        );
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(line_col(src, d[0].start_offset), (2, 4));
    }

    #[test]
    fn always_truthy_nil_constant_is_falsey() {
        // `cb = nil; if cb` — only nil/false are falsey ⇒ "always falsey".
        let src = b"cb = nil\nif cb\n  noop\nend\n";
        let d = always_truthy(src);
        assert_eq!(d.len(), 1, "expected one diagnostic, got {d:?}");
        assert_eq!(
            d[0].message,
            "condition is always falsey (the surrounding flow proves it folds to a constant)"
        );
        assert_eq!(line_col(src, d[0].start_offset), (2, 4));
    }

    #[test]
    fn always_truthy_inferred_fold_fires() {
        // `cc = 1 + 1; if cc` — an INFERRED constant (folded arithmetic, not a
        // syntactic literal). This is the case unreachable-branch cannot reach.
        let d = always_truthy(b"cc = 1 + 1\nif cc\n  noop\nend\n");
        assert_eq!(d.len(), 1, "inferred-constant predicate must fire, got {d:?}");
        assert!(d[0].message.contains("always truthy"));
    }

    #[test]
    fn always_truthy_unless_false_is_falsey() {
        // The `unless` keyword: predicate `cd` folds to `false` ⇒ "always falsey"
        // (polarity is the predicate VALUE, independent of which branch runs).
        let d = always_truthy(b"cd = false\nunless cd\n  noop\nend\n");
        assert_eq!(d.len(), 1, "unless-false predicate must fire, got {d:?}");
        assert!(d[0].message.contains("always falsey"));
    }

    #[test]
    fn always_truthy_branch_reassignment_widens_silent() {
        // THE KEYSTONE. `na = 5`, then a CONDITIONAL reassignment ⇒ `na` is
        // `5 | <recompute>` at the second `if` — the flow join widens it, so NO
        // fire. The flat (non-flow) env would keep `na = 5` and falsely fire.
        let src = b"na = 5\nif guard\n  na = recompute\nend\nif na\n  noop\nend\n";
        assert!(
            always_truthy(src).is_empty(),
            "a conditionally-reassigned local must NOT fold to a constant"
        );
    }

    #[test]
    fn always_truthy_defensive_predicate_silent() {
        // A defensive predicate call (`nil?`/`empty?`/…) reads as an explicit
        // runtime check; the reference skips it ⇒ silent.
        assert!(
            always_truthy(b"nb = 5\nif nb.nil?\n  noop\nend\n").is_empty(),
            "defensive `.nil?` predicate must be silent"
        );
    }

    #[test]
    fn always_truthy_loop_nested_silent() {
        // A predicate inside a loop/block body is suppressed (loop-mutation
        // modelling is incomplete) ⇒ silent, matching the reference envelope.
        let src = b"nc = 7\nwhile guard\n  if nc\n    noop\n  end\nend\n";
        assert!(always_truthy(src).is_empty(), "loop-nested predicate must be silent");
    }

    #[test]
    fn always_truthy_param_never_folds_silent() {
        // A method parameter is `Dynamic[top]`, never a constant ⇒ silent.
        let src = b"def m(flag)\n  if flag\n    noop\n  end\nend\n";
        assert!(always_truthy(src).is_empty(), "a param predicate must never fold");
    }

    #[test]
    fn always_truthy_skips_syntactic_literal() {
        // A SYNTACTIC literal predicate is owned by unreachable-branch; always-
        // truthy must NOT double-fire on it (the reference skips literals here).
        assert!(
            always_truthy(b"if true\n  live\nend\n").is_empty(),
            "literal predicate is unreachable-branch's domain, not always-truthy's"
        );
    }

    // -- call.unresolved-toplevel -----------------------------------------
    //
    // An implicit-self call at TOPLEVEL (outside any class/module) whose name
    // resolves against neither the Object/Kernel surface nor a same-file toplevel
    // def. Each case verified byte-exact (rule, line, column) against the oracle.

    /// The `call.unresolved-toplevel` diagnostics in `src`, in source order.
    fn unresolved(src: &[u8]) -> Vec<Diagnostic> {
        run(src)
            .into_iter()
            .filter(|d| d.rule_id == CALL_UNRESOLVED_TOPLEVEL)
            .collect()
    }

    #[test]
    fn unresolved_toplevel_fires_on_undefined_call() {
        // A bare implicit-self call to an undefined method at toplevel. Oracle:
        // 1:1, method `undefined_xyz`.
        let src = b"undefined_xyz\n";
        let d = unresolved(src);
        assert_eq!(d.len(), 1, "expected one diagnostic, got {d:?}");
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].message.starts_with("unresolved toplevel call to `undefined_xyz`"));
        assert_eq!(line_col(src, d[0].start_offset), (1, 1));
    }

    #[test]
    fn unresolved_toplevel_kernel_method_resolves_silent() {
        // Kernel methods (`def self?.x` in core RBS ⇒ instance methods on Kernel,
        // included by Object) resolve ⇒ silent.
        for src in [
            b"puts \"x\"\n".as_slice(),
            b"require \"set\"\n".as_slice(),
            b"loop { break }\n".as_slice(),
            b"raise \"e\"\n".as_slice(),
        ] {
            assert!(
                unresolved(src).is_empty(),
                "Kernel call must resolve silently: {src:?}"
            );
        }
    }

    #[test]
    fn unresolved_toplevel_same_file_def_silent() {
        // A same-file toplevel `def` resolves a later toplevel call to it.
        assert!(
            unresolved(b"def helper\n  42\nend\nhelper\n").is_empty(),
            "a same-file toplevel def must resolve the call"
        );
    }

    #[test]
    fn unresolved_toplevel_inside_class_body_silent() {
        // An implicit-self call inside a class/module body is NOT toplevel
        // (ADR-24 leniency) ⇒ silent even when unresolved.
        assert!(
            unresolved(b"class Widget\n  some_macro\n  def run\n    also_missing\n  end\nend\n")
                .is_empty(),
            "in-class implicit-self calls are not toplevel"
        );
    }

    #[test]
    fn unresolved_toplevel_fires_inside_toplevel_def_body() {
        // A toplevel `def`'s BODY is still toplevel (scope.toplevel? = outside any
        // class/module) ⇒ an unresolved implicit-self call there FIRES. Oracle: 2:3.
        let src = b"def m\n  still_missing\nend\n";
        let d = unresolved(src);
        assert_eq!(d.len(), 1, "toplevel def body is toplevel, got {d:?}");
        assert_eq!(line_col(src, d[0].start_offset), (2, 3));
    }

    #[test]
    fn catalog_entries_are_correct() {
        let entry = catalog(CALL_UNDEFINED_METHOD).expect("catalog entry must exist");
        assert_eq!(entry.default_severity, Severity::Error);
        assert_eq!(entry.evidence_tier, "high");
        assert!(entry.documentation_url.contains("call-undefined-method"));

        let entry = catalog(CALL_WRONG_ARITY).expect("catalog entry must exist");
        assert_eq!(entry.default_severity, Severity::Error);
        assert_eq!(entry.evidence_tier, "high");

        let entry = catalog(CALL_POSSIBLE_NIL_RECEIVER).expect("catalog entry must exist");
        // `error` under the default balanced profile (matches the oracle).
        assert_eq!(entry.default_severity, Severity::Error);
        assert_eq!(entry.evidence_tier, "medium");

        let entry = catalog(FLOW_DEAD_ASSIGNMENT).expect("catalog entry must exist");
        assert_eq!(entry.default_severity, Severity::Warning);
        assert_eq!(entry.evidence_tier, "medium");
        assert!(entry.documentation_url.contains("flow-dead-assignment"));

        let entry = catalog(FLOW_UNREACHABLE_BRANCH).expect("catalog entry must exist");
        assert_eq!(entry.default_severity, Severity::Warning);
        assert_eq!(entry.evidence_tier, "high");
        assert!(entry.documentation_url.contains("flow-unreachable-branch"));

        let entry = catalog(FLOW_ALWAYS_TRUTHY_CONDITION).expect("catalog entry must exist");
        assert_eq!(entry.default_severity, Severity::Warning);
        assert_eq!(entry.evidence_tier, "medium");
        assert!(entry.documentation_url.contains("flow-always-truthy-condition"));

        let entry = catalog(CALL_UNRESOLVED_TOPLEVEL).expect("catalog entry must exist");
        assert_eq!(entry.default_severity, Severity::Warning);
        assert_eq!(entry.evidence_tier, "low");
        assert!(entry.documentation_url.contains("call-unresolved-toplevel"));

        assert!(catalog("unknown.rule").is_none());
    }

    // -- in-source suppression --------------------------------------------

    fn diag(rule: &'static str) -> Diagnostic {
        Diagnostic {
            rule_id: rule,
            start_offset: 0,
            end_offset: 0,
            message: String::new(),
            severity: Severity::Error,
            source_family: "builtin",
            receiver_type: None,
            method_name: None,
        }
    }

    fn surviving_rules(
        diags: Vec<(usize, Diagnostic)>,
        comments: &[(usize, String)],
    ) -> Vec<(usize, &'static str)> {
        filter_suppressed(diags, comments)
            .into_iter()
            .map(|(line, d)| (line, d.rule_id))
            .collect()
    }

    #[test]
    fn line_disable_drops_only_that_lines_rule() {
        let diags = vec![
            (2, diag(CALL_UNDEFINED_METHOD)),
            (4, diag(CALL_UNDEFINED_METHOD)),
        ];
        let comments = vec![(4, "# rigor:disable call.undefined-method".to_string())];
        // Only the L4 diagnostic is suppressed; L2 survives.
        assert_eq!(surviving_rules(diags, &comments), vec![(2, CALL_UNDEFINED_METHOD)]);
    }

    #[test]
    fn line_disable_all_drops_every_rule_on_that_line() {
        let diags = vec![
            (3, diag(CALL_UNDEFINED_METHOD)),
            (3, diag(CALL_WRONG_ARITY)),
            (5, diag(CALL_WRONG_ARITY)),
        ];
        let comments = vec![(3, "# rigor:disable all".to_string())];
        assert_eq!(surviving_rules(diags, &comments), vec![(5, CALL_WRONG_ARITY)]);
    }

    #[test]
    fn disable_file_drops_rule_on_every_line() {
        let diags = vec![
            (2, diag(CALL_UNDEFINED_METHOD)),
            (9, diag(CALL_UNDEFINED_METHOD)),
            (9, diag(CALL_WRONG_ARITY)),
        ];
        // The directive sits on line 1 but scopes the whole file.
        let comments = vec![(1, "# rigor:disable-file undefined-method".to_string())];
        assert_eq!(surviving_rules(diags, &comments), vec![(9, CALL_WRONG_ARITY)]);
    }

    #[test]
    fn disable_file_all_drops_everything() {
        let diags = vec![
            (2, diag(CALL_UNDEFINED_METHOD)),
            (4, diag(CALL_WRONG_ARITY)),
            (6, diag(CALL_POSSIBLE_NIL_RECEIVER)),
        ];
        let comments = vec![(1, "# rigor:disable-file all".to_string())];
        assert!(filter_suppressed(diags, &comments).is_empty());
    }

    #[test]
    fn family_token_call_expands_to_all_call_rules() {
        let diags = vec![
            (2, diag(CALL_UNDEFINED_METHOD)),
            (2, diag(CALL_WRONG_ARITY)),
            (2, diag(CALL_POSSIBLE_NIL_RECEIVER)),
        ];
        let comments = vec![(2, "# rigor:disable call".to_string())];
        assert!(filter_suppressed(diags, &comments).is_empty());
    }

    #[test]
    fn legacy_alias_resolves_to_canonical_id() {
        let diags = vec![(4, diag(CALL_UNDEFINED_METHOD))];
        let comments = vec![(4, "# rigor:disable undefined-method".to_string())];
        assert!(filter_suppressed(diags, &comments).is_empty());
    }

    #[test]
    fn comma_and_whitespace_separated_tokens() {
        let diags = vec![
            (2, diag(CALL_UNDEFINED_METHOD)),
            (2, diag(CALL_WRONG_ARITY)),
        ];
        let comments = vec![(2, "# rigor:disable undefined-method, wrong-arity".to_string())];
        assert!(filter_suppressed(diags, &comments).is_empty());
    }

    #[test]
    fn unrelated_rule_or_line_is_not_suppressed() {
        // A disable for a DIFFERENT rule on the same line must not drop it.
        let same_line = filter_suppressed(
            vec![(4, diag(CALL_UNDEFINED_METHOD))],
            &[(4, "# rigor:disable wrong-arity".to_string())],
        );
        assert_eq!(same_line.len(), 1);

        // A disable on a DIFFERENT line must not drop it.
        let other_line = filter_suppressed(
            vec![(4, diag(CALL_UNDEFINED_METHOD))],
            &[(7, "# rigor:disable undefined-method".to_string())],
        );
        assert_eq!(other_line.len(), 1);
    }

    #[test]
    fn disable_file_negative_lookahead_not_read_as_line_disable() {
        // `disable-file` must NOT also register as a line-level `disable` for the
        // comment's own line (reference `(?!-file)`).
        let line_set =
            parse_suppression_comments(&[(3, "# rigor:disable-file undefined-method".to_string())]);
        assert!(line_set.0.get(&3).is_none());
        assert!(line_set.1.suppresses(CALL_UNDEFINED_METHOD));
    }

    #[test]
    fn internal_error_is_never_suppressed() {
        let diags = vec![(2, diag(INTERNAL_ERROR_RULE))];
        let comments = vec![(2, "# rigor:disable all".to_string())];
        // Even `disable all` cannot silence an internal-error diagnostic.
        assert_eq!(filter_suppressed(diags, &comments).len(), 1);
    }

    #[test]
    fn suppress_set_from_tokens_legacy_alias() {
        // The public config helper expands the legacy alias to its canonical id.
        let set = SuppressSet::from_tokens(&["undefined-method"]);
        assert!(set.suppresses(CALL_UNDEFINED_METHOD));
        assert!(!set.suppresses(CALL_WRONG_ARITY));
    }

    #[test]
    fn suppress_set_from_tokens_call_family_and_canonical() {
        // `call` family wildcard expands to every implemented call.* id.
        let set = SuppressSet::from_tokens(&["call"]);
        assert!(set.suppresses(CALL_UNDEFINED_METHOD));
        assert!(set.suppresses(CALL_WRONG_ARITY));
        assert!(set.suppresses(CALL_POSSIBLE_NIL_RECEIVER));
        // A canonical id passes through to itself.
        let set = SuppressSet::from_tokens(&[CALL_WRONG_ARITY]);
        assert!(set.suppresses(CALL_WRONG_ARITY));
        assert!(!set.suppresses(CALL_UNDEFINED_METHOD));
    }

    #[test]
    fn suppress_set_from_tokens_never_matches_internal_error() {
        // Neither `all` nor an explicit `internal-error` token may match the
        // internal-error sentinel — it stays reportable through config too.
        assert!(!SuppressSet::from_tokens(&["all"]).suppresses(INTERNAL_ERROR_RULE));
        assert!(!SuppressSet::from_tokens(&["internal-error"]).suppresses(INTERNAL_ERROR_RULE));
    }

    #[test]
    fn suppress_set_from_tokens_empty_and_unknown_are_inert() {
        let empty: [&str; 0] = [];
        assert!(!SuppressSet::from_tokens(&empty).suppresses(CALL_UNDEFINED_METHOD));
        // An unknown token matches no real diagnostic.
        let set = SuppressSet::from_tokens(&["not-a-real-rule"]);
        assert!(!set.suppresses(CALL_UNDEFINED_METHOD));
        assert!(!set.suppresses(CALL_WRONG_ARITY));
    }

    // --- ADR-35 slice 1: def.override-visibility-reduced ----------------------

    /// The override-visibility diagnostics in one source string (single-file).
    fn override_vis(src: &[u8]) -> Vec<Diagnostic> {
        run(src)
            .into_iter()
            .filter(|d| d.rule_id == DEF_OVERRIDE_VISIBILITY_REDUCED)
            .collect()
    }

    /// Cross-file: analyze `files[focus]` against a PROJECT source built over all
    /// `files`, returning only the override-visibility diagnostics.
    fn override_vis_project(files: &[&[u8]], focus: usize) -> Vec<Diagnostic> {
        let asts: Vec<_> = files.iter().map(|s| lower(&parse(s))).collect();
        let refs: Vec<&LoweredAst> = asts.iter().collect();
        let index = CoreIndex::new();
        let source = rigor_infer::SourceIndex::build_project(&refs, &index);
        let mut interner = Interner::new();
        analyze_with_source(&asts[focus], &mut interner, &index, &source)
            .into_iter()
            .filter(|d| d.rule_id == DEF_OVERRIDE_VISIBILITY_REDUCED)
            .collect()
    }

    #[test]
    fn override_vis_fires_public_to_private_across_superclass() {
        // The oracle fixture: B < A, A#foo public, B#foo private ⇒ fires.
        let src = b"class A\n  def foo; end\nend\nclass B < A\n  private\n  def foo; end\nend\n";
        let diags = override_vis(src);
        assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
        let d = &diags[0];
        assert_eq!(d.rule_id, DEF_OVERRIDE_VISIBILITY_REDUCED);
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.source_family, "builtin");
        assert_eq!(d.method_name.as_deref(), Some("foo"));
        assert_eq!(
            d.message,
            "visibility of `foo' reduced from public to private (overrides A#foo); breaks substitutability"
        );
        // Anchored on the overriding def's name token.
        assert_eq!(&src[d.start_offset..d.end_offset], b"foo");
    }

    #[test]
    fn override_vis_fires_public_to_protected() {
        let src = b"class A\n  def foo; end\nend\nclass B < A\n  protected\n  def foo; end\nend\n";
        let diags = override_vis(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].message,
            "visibility of `foo' reduced from public to protected (overrides A#foo); breaks substitutability"
        );
    }

    #[test]
    fn override_vis_silent_on_widening() {
        // private parent ⇒ public override is a WIDENING (not a reduction) ⇒ silent.
        let src = b"class A\n  private\n  def foo; end\nend\nclass B < A\n  def foo; end\nend\n";
        assert!(override_vis(src).is_empty());
        // protected ⇒ public widening too.
        let src2 = b"class A\n  protected\n  def foo; end\nend\nclass B < A\n  def foo; end\nend\n";
        assert!(override_vis(src2).is_empty());
    }

    #[test]
    fn override_vis_silent_when_ancestor_is_rbs_or_unknown() {
        // `class B < ApplicationRecord` — the super is not a project source class
        // ⇒ no project ancestor defines the method ⇒ silent (RBS carve-out).
        let src = b"class B < ApplicationRecord\n  private\n  def foo; end\nend\n";
        assert!(override_vis(src).is_empty());
    }

    #[test]
    fn override_vis_silent_when_no_ancestor_defines() {
        // B < A but A does not define `foo` ⇒ silent.
        let src = b"class A\n  def other; end\nend\nclass B < A\n  private\n  def foo; end\nend\n";
        assert!(override_vis(src).is_empty());
    }

    #[test]
    fn override_vis_silent_on_singleton_def() {
        // `def self.foo` is a singleton method — never in the visibility table ⇒
        // silent even under a bare `private`.
        let src = b"class A\n  def foo; end\nend\nclass B < A\n  private\n  def self.foo; end\nend\n";
        assert!(override_vis(src).is_empty());
    }

    #[test]
    fn override_vis_silent_on_private_def_form() {
        // `private def foo` records `foo` at the running default (Public),
        // mirroring the reference gap ⇒ Public-vs-Public is no reduction ⇒ silent.
        let src = b"class A\n  def foo; end\nend\nclass B < A\n  private def foo; end\nend\n";
        assert!(override_vis(src).is_empty());
    }

    #[test]
    fn override_vis_fires_across_included_module() {
        // M#foo public (included into B); B#foo private ⇒ fires, overrides M#foo.
        let src = b"module M\n  def foo; end\nend\nclass B\n  include M\n  private\n  def foo; end\nend\n";
        let diags = override_vis(src);
        assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
        assert_eq!(
            diags[0].message,
            "visibility of `foo' reduced from public to private (overrides M#foo); breaks substitutability"
        );
    }

    #[test]
    fn override_vis_fires_cross_file() {
        // Parent A in file 0, subclass B (private override) in file 1 — built via
        // `build_project`, the walk resolves A across files and fires.
        let a = b"class A\n  def foo; end\nend\n" as &[u8];
        let b = b"class B < A\n  private\n  def foo; end\nend\n" as &[u8];
        let diags = override_vis_project(&[a, b], 1);
        assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
        assert_eq!(
            diags[0].message,
            "visibility of `foo' reduced from public to private (overrides A#foo); breaks substitutability"
        );
    }

    #[test]
    fn override_vis_catalog_entry_matches_oracle() {
        let e = catalog(DEF_OVERRIDE_VISIBILITY_REDUCED).expect("catalog entry must exist");
        assert_eq!(e.default_severity, Severity::Warning);
        assert_eq!(e.evidence_tier, "high");
        assert_eq!(
            e.documentation_url,
            "https://github.com/rigortype/rigor/blob/main/docs/manual/04-diagnostics.md#rule-def-override-visibility-reduced"
        );
    }

    // --- flow.always-raises (Integer ÷/% by constant-zero divisor) -----------

    /// Diagnostics filtered to just the always-raises rule.
    fn always_raises_diags(src: &[u8]) -> Vec<Diagnostic> {
        run(src)
            .into_iter()
            .filter(|d| d.rule_id == FLOW_ALWAYS_RAISES)
            .collect()
    }

    #[test]
    fn always_raises_fires_on_literal_int_div_zero() {
        // Byte-exact with the oracle: `5 / 0` ⇒ error, message anchored on `/`.
        let src = b"5 / 0\n";
        let diags = always_raises_diags(src);
        assert_eq!(diags.len(), 1, "expected one diag, got {diags:?}");
        let d = &diags[0];
        assert_eq!(d.rule_id, FLOW_ALWAYS_RAISES);
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.source_family, "builtin");
        assert_eq!(
            d.message,
            "always raises ZeroDivisionError: `/' by zero on Integer receiver"
        );
        // Span anchors on the operator token (the message loc), matching the
        // oracle's column.
        assert_eq!(&src[d.start_offset..d.end_offset], b"/");
    }

    #[test]
    fn always_raises_fires_on_modulo_zero() {
        let src = b"10 % 0\n";
        let diags = always_raises_diags(src);
        assert_eq!(diags.len(), 1, "expected one diag, got {diags:?}");
        assert_eq!(
            diags[0].message,
            "always raises ZeroDivisionError: `%' by zero on Integer receiver"
        );
    }

    #[test]
    fn always_raises_fires_through_local_binding() {
        // `x = 5; x / 0` — the receiver folds to `Constant[Int(5)]` (Integer-
        // rooted), the divisor is constant zero ⇒ fire (oracle parity).
        let src = b"x = 5\nx / 0\n";
        let diags = always_raises_diags(src);
        assert_eq!(diags.len(), 1, "expected one diag, got {diags:?}");
        assert_eq!(
            diags[0].message,
            "always raises ZeroDivisionError: `/' by zero on Integer receiver"
        );
    }

    #[test]
    fn always_raises_fires_on_named_ops() {
        // `div`, `modulo`, `divmod` are in the reference's op set.
        for (src, op) in [
            (b"7.div(0)\n" as &[u8], "div"),
            (b"8.modulo(0)\n", "modulo"),
            (b"9.divmod(0)\n", "divmod"),
        ] {
            let diags = always_raises_diags(src);
            assert_eq!(diags.len(), 1, "expected one diag for {op}, got {diags:?}");
            assert_eq!(
                diags[0].message,
                format!("always raises ZeroDivisionError: `{op}' by zero on Integer receiver")
            );
        }
    }

    #[test]
    fn always_raises_silent_on_nonzero_divisor() {
        // `5 / 2` — a valid division, never raises ⇒ silent.
        assert!(always_raises_diags(b"5 / 2\n").is_empty());
    }

    #[test]
    fn always_raises_silent_on_float_divisor() {
        // `5 / 0.0` — Float division by zero is `Infinity`, NOT an error. The
        // oracle is silent; rigor-rs must be too (the divisor is not a constant
        // Integer zero).
        assert!(always_raises_diags(b"5 / 0.0\n").is_empty());
    }

    #[test]
    fn always_raises_silent_on_float_receiver() {
        // `5.0 / 0` — Float receiver ⇒ Float division ⇒ `Infinity`, not an error.
        // The oracle declines (receiver not Integer-rooted); rigor-rs must too.
        assert!(always_raises_diags(b"5.0 / 0\n").is_empty());
    }

    #[test]
    fn always_raises_silent_on_nonconstant_divisor() {
        // `x / y` with `y` non-constant ⇒ the divisor is not a constant zero ⇒
        // decline (never guess on a dynamic divisor).
        assert!(always_raises_diags(b"x = 5\nx / y\n").is_empty());
    }

    #[test]
    fn always_raises_silent_on_dynamic_receiver() {
        // `x / 0` where `x` is never bound ⇒ Dynamic receiver, not Integer-rooted
        // ⇒ decline (zero-FP keystone).
        assert!(always_raises_diags(b"x / 0\n").is_empty());
    }

    #[test]
    fn always_raises_silent_on_block_call() {
        // A block changes dispatch ⇒ decline. `5.div(0) { }` is contrived but
        // exercises the block gate.
        assert!(always_raises_diags(b"5.div(0) { 1 }\n").is_empty());
    }

    #[test]
    fn always_raises_catalog_entry_matches_oracle() {
        let e = catalog(FLOW_ALWAYS_RAISES).expect("catalog entry must exist");
        assert_eq!(e.default_severity, Severity::Error);
        assert_eq!(e.evidence_tier, "high");
        assert_eq!(
            e.documentation_url,
            "https://github.com/rigortype/rigor/blob/main/docs/manual/04-diagnostics.md#rule-flow-always-raises"
        );
    }

    // --- ADR-0033: project `sig/`-declared class instance witnessing ----------

    /// Analyze `src` against a CoreIndex built WITH a project `sig/` dir holding
    /// `class Widget; def spin: () -> Integer; end`. Uses a real temp dir (sig
    /// ingestion is filesystem-driven). Returns undefined-method diagnostics.
    fn run_with_widget_sig(label: &str, src: &[u8]) -> Vec<Diagnostic> {
        // `label` makes the dir unique per test — tests run in parallel threads
        // sharing one process id, so a shared path would let one test's cleanup
        // wipe another's sig file mid-run.
        let dir = std::env::temp_dir()
            .join(format!("rigor-rules-sig-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp sig dir");
        std::fs::write(dir.join("widget.rbs"), "class Widget\n  def spin: () -> Integer\nend\n")
            .expect("write sig");
        let index = CoreIndex::for_project(&[], std::slice::from_ref(&dir));
        let ast = lower(&parse(src));
        let refs = [&ast];
        let source = rigor_infer::SourceIndex::build_project(&refs, &index);
        let mut interner = Interner::new();
        let diags = analyze_with_source(&ast, &mut interner, &index, &source)
            .into_iter()
            .filter(|d| d.rule_id == CALL_UNDEFINED_METHOD)
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        diags
    }

    #[test]
    fn project_sig_new_instance_typo_is_witnessed() {
        // `Widget.new.spni` — Widget is declared in project sig/, so the reference
        // (and now rigor-rs) witnesses the typo on the `.new` instance.
        let diags = run_with_widget_sig("typo", b"Widget.new.spni\n");
        assert_eq!(diags.len(), 1, "expected witness, got {diags:?}");
        assert_eq!(diags[0].receiver_type.as_deref(), Some("Widget"));
        assert_eq!(diags[0].method_name.as_deref(), Some("spni"));
        assert_eq!(diags[0].severity, Severity::Error);
    }

    #[test]
    fn project_sig_new_instance_valid_method_is_silent() {
        // `spin` IS declared ⇒ no diagnostic (the sig is authoritative both ways).
        assert!(run_with_widget_sig("valid", b"Widget.new.spin\n").is_empty());
    }

    #[test]
    fn project_sig_new_instance_through_variable_is_witnessed() {
        // The instance type survives the local binding (`w = Widget.new; w.spni`).
        let diags = run_with_widget_sig("var", b"w = Widget.new\nw.spin\nw.spni\n");
        assert_eq!(diags.len(), 1, "expected one witness, got {diags:?}");
        assert_eq!(diags[0].receiver_type.as_deref(), Some("Widget"));
    }

    #[test]
    fn bundled_stdlib_new_instance_stays_lenient_with_sig_loaded() {
        // Provenance gate: even with a project sig/ present, a bundled stdlib
        // class (`Pathname`, NOT project-sig) keeps the reference's `.new`
        // leniency — its typo must NOT be witnessed.
        assert!(run_with_widget_sig("pathname", b"Pathname.new(\"a\").spni\n").is_empty());
    }
}
