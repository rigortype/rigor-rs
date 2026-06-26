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
            default_severity: Severity::Warning,
            evidence_tier: "medium",
            documentation_url: "https://github.com/rigortype/rigor/blob/main/docs/manual/04-diagnostics.md#rule-call-possible-nil-receiver",
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
    // Build a typer over the real index so non-folded nominal returns
    // (e.g. `Integer#to_s -> String`) resolve for chained-call typing.
    let typer = Typer::new(index);
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
                message_span,
                ..
            } => Some((id, *recv, method.clone(), args.clone(), *message_span)),
            _ => None,
        })
        .collect();

    for (_id, recv, method, args, message_span) in calls {
        // Rule precedence at one call site (avoid double-emit):
        //   1. undefined-method  (method absent on the receiver class, incl. nil)
        //   2. wrong-arity       (method present but arg count out of envelope)
        //   3. possible-nil-receiver (union receiver with a nil arm)
        // The reference emits exactly one of these per call; we mirror that by
        // returning the first that fires.
        let diag = check_call(ast, recv, &method, message_span, &env, &typer, interner, index)
            .or_else(|| {
                check_wrong_arity(ast, recv, &method, &args, message_span, &env, &typer, interner, index)
            })
            .or_else(|| {
                check_nil_receiver(ast, recv, &method, message_span, &env, &typer, interner, index)
            });
        if let Some(diag) = diag {
            out.push(diag);
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Rule implementations
// ---------------------------------------------------------------------------

/// Apply `call.undefined-method` to a single call with a receiver.
///
/// Zero-false-positive gate (ADR-0023): emit *only* when the receiver types to
/// a concrete class the [`CoreIndex`] models AND that class is known to lack
/// the method. If the receiver is `Dynamic`/unknown, or the class is outside
/// the index, emit nothing — never guess.
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

    // Resolve the receiver's class name; `None` => Dynamic/unknown => silent.
    let class_name = index.class_name_of(interner, recv_ty)?;

    // Only a class the index actually models can witness method *absence*.
    if !index.knows_class(class_name) {
        return None;
    }
    if index.class_has_method(class_name, method) {
        return None;
    }

    // Render the receiver in the reference's value-in-message style: the bare
    // value for a `Constant` (`"Hello"`, `3`), else the class name. The
    // `message` field is presentation, not contract (ADR-0030).
    let receiver_render = render_receiver(interner, recv_ty, class_name);
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
fn check_wrong_arity(
    ast: &LoweredAst,
    receiver: rigor_parse::NodeId,
    method: &str,
    args: &[rigor_parse::NodeId],
    message_span: (usize, usize),
    env: &rigor_infer::TypeEnv,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
) -> Option<Diagnostic> {
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
    let (min, max) = rigor_index::method_arity(class_name, method)?;

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

/// Apply `call.possible-nil-receiver` to a single call with a receiver.
///
/// In this slice the rule is intentionally narrow: it fires only when the
/// receiver type proves nil-on-some-path via a UNION carrier containing a nil
/// member, while a non-nil arm still defines the method. The literal-`nil`
/// receiver (`x = nil; x.upcase`) is deliberately NOT handled here — that types
/// to exactly `Constant[Nil]` and is owned by `call.undefined-method` (matching
/// the reference, which routes a definitely-nil receiver to undefined-method).
///
/// The tracer-bullet type lattice does not yet produce union receivers through
/// flow narrowing, so this rule is currently inert on the corpus; it exists to
/// hold the rule id + gate shape without ever introducing a false positive.
//
// TODO(spec): nil-on-a-live-path via union/flow narrowing — once a guard like
// `return if x.nil?` or a `T | nil` parameter produces a `Type::Union` with a
// nil arm, fire here when the called method exists on the non-nil arm(s) but
// not on NilClass (ADR-0022 flow narrowing).
fn check_nil_receiver(
    _ast: &LoweredAst,
    _receiver: rigor_parse::NodeId,
    _method: &str,
    _message_span: (usize, usize),
    _env: &rigor_infer::TypeEnv,
    _typer: &Typer,
    _interner: &mut Interner,
    _index: &CoreIndex,
) -> Option<Diagnostic> {
    // No union/flow narrowing in this slice => never fires. Returning `None`
    // keeps the zero-false-positive contract until flow narrowing lands.
    None
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
    fn known_method_is_silent() {
        let diags = run(b"s = \"Hello\"\ns.length\n");
        assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
    }

    #[test]
    fn dynamic_receiver_is_silent() {
        // `x` is never assigned => Dynamic[top] => never guess.
        let diags = run(b"x.foo\n");
        assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
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
        // Dynamic receiver with any arity stays silent (never guess).
        assert!(run(b"x.foo(1, 2, 3)\n").is_empty());
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
    fn catalog_entries_are_correct() {
        let entry = catalog(CALL_UNDEFINED_METHOD).expect("catalog entry must exist");
        assert_eq!(entry.default_severity, Severity::Error);
        assert_eq!(entry.evidence_tier, "high");
        assert!(entry.documentation_url.contains("call-undefined-method"));

        let entry = catalog(CALL_WRONG_ARITY).expect("catalog entry must exist");
        assert_eq!(entry.default_severity, Severity::Error);
        assert_eq!(entry.evidence_tier, "high");

        let entry = catalog(CALL_POSSIBLE_NIL_RECEIVER).expect("catalog entry must exist");
        assert_eq!(entry.default_severity, Severity::Warning);
        assert_eq!(entry.evidence_tier, "medium");

        assert!(catalog("unknown.rule").is_none());
    }
}
