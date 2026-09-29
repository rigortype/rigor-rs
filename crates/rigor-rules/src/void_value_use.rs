//! `static.value-use.void`: a call whose author-declared RBS return is
//! `void`, sitting in value context. The CLI runs it only under the
//! `use-of-void-value` bleeding-edge feature.

use rigor_index::CoreIndex;
use rigor_infer::Typer;
use rigor_parse::{LoweredAst, Node, NodeId};
use rigor_types::{Interner, Type};

use crate::{Diagnostic, ScopedEnv, Severity, STATIC_VALUE_USE_VOID};

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
        match node {
            Node::LocalVariableWrite { value, .. }
            | Node::InstanceVariableWrite { value, .. }
            | Node::ConstantWrite { value, .. } => {
                check_void_value_use(ast, *value, &env, &typer, interner, index, &mut out);

            }
            Node::Call { receiver, args, .. } => {
                if let Some(recv) = receiver {
                    check_void_value_use(ast, *recv, &env, &typer, interner, index, &mut out);

                }
                for &arg in args {
                    check_void_value_use(ast, arg, &env, &typer, interner, index, &mut out);

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
    scoped: &ScopedEnv,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
    out: &mut Vec<Diagnostic>,
) {
    let Node::Call { receiver: Some(recv), method, span, .. } = ast.get(value_id) else {
        return;
    };
    let (recv, method, span) = (*recv, method.clone(), *span);
    // The value types from the scope it was entered from (rigor-rs#136).
    let env = scoped.at(ast, typer, ast.get(value_id).span(), value_id, interner);
    let recv_ty = typer.type_of(ast, recv, &env, interner);
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
