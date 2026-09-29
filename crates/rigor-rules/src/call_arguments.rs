//! The rules that read a method's RBS parameters: `call.wrong-arity`
//! and `call.argument-type-mismatch` (ADR-64).

use rigor_index::{CoreIndex, OverloadSignature, RetainedParamType};
use rigor_infer::Typer;
use rigor_parse::{LoweredAst, NodeId};
use rigor_types::{Interner, Type};

use crate::{
    catalog, concrete_class_name, render_receiver, Diagnostic, ScopedEnv, Severity,
    CALL_ARGUMENT_TYPE_MISMATCH, CALL_WRONG_ARITY,
};

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
pub(crate) fn check_wrong_arity(
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
pub(crate) fn arg_is_pure_nil(
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
    scoped: &ScopedEnv,
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
        // `argument_scope(arg)`: each argument types from the scope it was
        // ENTERED from (rigor-rs#136) — a later arg's env still sees the
        // earlier args' effects, the first arg's does not.
        let arg_env = scoped.at(ast, typer, ast.get(arg).span(), interner);
        let arg_ty = typer.type_of(ast, arg, &arg_env, interner);

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
    scoped: &ScopedEnv,
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

        // `argument_scope(arg)`: the scope the argument was entered from.
        let arg_env = scoped.at(ast, typer, ast.get(arg).span(), interner);
        let arg_ty = typer.type_of(ast, arg, &arg_env, interner);

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
///   (reference `check_rules.rb:1955`);
/// - each argument types from the scope it was entered from (reference
///   `argument_scope`, rigor-rs#136): `f(b.unshift("s"), b.first)` types the
///   second arg against the scope the first left, while the first arg still
///   sees the call's entry scope.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_argument_type_mismatch(
    ast: &LoweredAst,
    receiver: rigor_parse::NodeId,
    method: &str,
    args: &[rigor_parse::NodeId],
    args_all_plain: bool,
    message_span: (usize, usize),
    scoped: &ScopedEnv,
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
    let call_env = scoped.at(ast, typer, message_span, interner);
    let recv_ty = typer.type_of(ast, receiver, &call_env, interner);

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
        single_overload_mismatch(&overloads[0], args, ast, scoped, typer, interner, index, source)
    } else {
        multi_overload_mismatch(
            &overloads, method, args, ast, scoped, typer, interner, index, source,
        )
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
