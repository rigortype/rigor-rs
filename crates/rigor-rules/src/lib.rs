//! Diagnostic rules + the structured `Diagnostic` type (ADR-0014: rule id,
//! severity, primary/secondary annotations, subdiagnostics). All rules run in a
//! single converged AST walk (ADR-0005), not one pass per rule. The tracer
//! bullet's first rule is `call.undefined-method`.
#![allow(dead_code)]

use rigor_index::CoreIndex;
use rigor_infer::{build_toplevel_env, type_of};
use rigor_parse::{LoweredAst, Node};
use rigor_types::{Interner, Scalar, Type};

/// A diagnostic finding, identified by `rule_id` + location (ADR-0002 parity is
/// defined over this pair). Skeleton.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub rule_id: &'static str,
    pub start_offset: usize,
    pub end_offset: usize,
    pub message: String,
}

/// The stable id of the headline tracer-bullet rule (ADR-0030 taxonomy).
pub const CALL_UNDEFINED_METHOD: &str = "call.undefined-method";

/// Analyze a lowered AST and return all diagnostics, in source order.
///
/// This is the single converged walk (ADR-0005): it builds the top-level type
/// environment once, then visits every node, applying each rule. Only
/// `call.undefined-method` exists in this slice.
pub fn analyze(ast: &LoweredAst, interner: &mut Interner, index: &CoreIndex) -> Vec<Diagnostic> {
    let env = build_toplevel_env(ast, interner);
    let mut out = Vec::new();

    // Visit nodes in id order, which is source-discovery order, so diagnostics
    // come out deterministically (ADR-0020 determinism).
    let calls: Vec<_> = ast
        .iter()
        .filter_map(|(id, node)| match node {
            Node::Call {
                receiver: Some(recv),
                method,
                message_span,
                ..
            } => Some((id, *recv, method.clone(), *message_span)),
            _ => None,
        })
        .collect();

    for (_id, recv, method, message_span) in calls {
        if let Some(diag) = check_call(ast, recv, &method, message_span, &env, interner, index) {
            out.push(diag);
        }
    }

    out
}

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
    interner: &mut Interner,
    index: &CoreIndex,
) -> Option<Diagnostic> {
    let recv_ty = type_of(ast, receiver, env, interner);

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

    Some(Diagnostic {
        rule_id: CALL_UNDEFINED_METHOD,
        start_offset: message_span.0,
        end_offset: message_span.1,
        message,
    })
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
}
