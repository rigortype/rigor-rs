//! `call.raise-non-exception`, and the operand class resolution
//! (`concrete_class_name` and friends) the argument-type rule shares.

use rigor_index::CoreIndex;
use rigor_infer::Typer;
use rigor_parse::{LoweredAst, Node, NodeId};
use rigor_types::{Interner, Scalar, Type};

use crate::{catalog, render_receiver, Diagnostic, ScopedEnv, Severity, CALL_RAISE_NON_EXCEPTION};

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
pub(crate) enum RaiseVerdict {
    Legal,
    Illegal,
    Unknown,
}

/// Emit `call.raise-non-exception` for every implicit-self `raise`/`fail` whose
/// first positional operand is provably not a legal raise operand. Its OWN walk
/// over `receiver: None` calls (the main call walk is receiver-Some only), NOT
/// toplevel-restricted (fires inside method bodies). A faithful port of the
/// reference `raise_non_exception_diagnostic`.
pub(crate) fn raise_non_exception_diagnostics(
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
pub(crate) fn raise_operand_verdict(
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
pub(crate) fn concrete_class_name(
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
        Scalar::BigInt(_) => "Integer",
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
