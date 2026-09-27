//! A literal block's parameter list, lowered to `(name, BlockParamKind)` pairs
//! for `Node::Call`'s `block_params` (rigor-rs#140).

use crate::ruby_prism::{self, Node as PrismNode};

use super::constant_string;

/// How one name in a literal block's parameter list binds — the tag half of
/// [`Node::Call`]'s `block_params` (rigor-rs#140). `tap`/`then`/`yield_self`
/// invoke their block as `yield self`, so the binding kind decides what the
/// parameter's type is when a `break`/`next` arm reads it. The kind also
/// records which SLOT class a positional occupies — required, optional or
/// post — so the entry env can apply the reference's `BlockAutoSplat` spread
/// when the yielded value is array-shaped and the parameter list is one
/// CRuby auto-splats (`splats?`).
///
/// [`Node::Call`]: crate::ast::Node::Call
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockParamKind {
    /// The FIRST positional parameter — `|v|`, `|v, w|`, the implicit `it`,
    /// or `_1` — fed the receiver by a `yield self` yielder, so it binds to
    /// the receiver's own type. Under an auto-splat it binds the carrier's
    /// per-slot element type instead (a required slot).
    SelfArg,
    /// The first positional when it is OPTIONAL (`|v = 1|` with no required
    /// parameter ahead of it) — binds the receiver like [`SelfArg`], but
    /// under an auto-splat the reference fills optional slots with
    /// `Dynamic[top]`, not the element type.
    SelfOpt,
    /// A name inside a destructured FIRST positional `|(v, w)|` — fed the
    /// receiver destructured, so it binds to the receiver's element type(s).
    /// Counts as one required slot for `splats?`; the names themselves stay
    /// unbound (the reference marks destructure slots optimistic).
    DestructuredSelfArg,
    /// A non-first REQUIRED positional (`|v, w|`'s `w`), or a numbered
    /// `_2.._9` parameter (the reference's `ParameterShape.of_arity` makes
    /// every `_N` a required slot) — unbound without an auto-splat, bound to
    /// the carrier's element type under one.
    RequiredArg,
    /// A non-first OPTIONAL positional (`|a = 1, b = 2|`'s `b`) — unbound
    /// without an auto-splat, bound `Dynamic[top]` under one.
    OptionalArg,
    /// A post-rest positional (`|*r, z|`'s `z`) — unbound without an
    /// auto-splat, bound to the carrier's element type under one.
    PostArg,
    /// A name inside a destructured NON-first positional — `|a, (x, y)|`'s
    /// `x` and `y`, or the names of a post-rest destructure. Hidden from the
    /// enclosing env but bound to nothing modeled; each contiguous group
    /// occupies one positional slot for `splats?`.
    Other,
    /// A `*rest` parameter — binds the leftover argument list, `Array[untyped]`
    /// for the exactly-once yielders (the reference binds
    /// `Array[Dynamic[top]]`, or `Array[element]` under an `Array[T]` splat).
    Rest,
    /// The anonymous rest of a `|v,|` trailing-comma parameter list (Prism's
    /// `ImplicitRestNode`) — binds no name but counts as a rest for
    /// `splats?`, which is what makes `|v,|` splat a lone array argument.
    ImplicitRest,
    /// A keyword parameter `|k:|` / `|k: 1|` — hidden from the enclosing
    /// env but bound to nothing modeled, and contributes no positional slot.
    Keyword,
    /// A `**kw` keyword-rest parameter — binds the captured keyword hash,
    /// `Hash` for the exactly-once yielders (the reference binds
    /// `Hash[Symbol, Dynamic[top]]`, which erases to the same nominal).
    KwRest,
    /// A `|;local|` block-local declaration — hidden from the enclosing env
    /// and left UNBOUND, exactly as the reference's binder declares it (a
    /// read types `Dynamic[top]`, not `nil`).
    Local,
    /// A `&blk` block capture — hidden from the enclosing env and bound to
    /// `Proc` (the reference's binder types the captured block `Proc`).
    Block,
}

/// Every name a literal block's parameter list binds, tagged with its
/// [`BlockParamKind`] — the lowered input to `Node::Call::block_params`
/// (rigor-rs#140). `tap`/`then`/`yield_self` invoke their block as
/// `yield self`, so the FIRST positional (a required or optional name, the
/// implicit `it`, `_1`, or the members of a leading `|(v, w)|`) receives the
/// receiver; a `*rest` collects leftovers into an `Array`; `|;local|`
/// declarations hide without binding; and every remaining name — later
/// positionals, posts, keywords, `**kw`, `&blk`, `_2.._9` — is hidden from
/// the enclosing env but bound to nothing modeled.
pub(crate) fn block_param_names(bn: &ruby_prism::BlockNode<'_>) -> Vec<(String, BlockParamKind)> {
    let mut out: Vec<(String, BlockParamKind)> = Vec::new();
    let Some(params) = bn.parameters() else {
        return out;
    };
    if let Some(bp) = params.as_block_parameters_node() {
        if let Some(p) = bp.parameters() {
            // The first POSITIONAL entry (required or optional) is the one a
            // `yield self` call feeds. Each later positional keeps its SLOT
            // class (required / optional / post) so the block-entry env can
            // apply the reference's `BlockAutoSplat` when the receiver is
            // array-shaped and the parameter list is one CRuby splats.
            let mut first_positional_taken = false;
            for req in p.requireds().iter() {
                push_block_positional(
                    &req,
                    &mut first_positional_taken,
                    BlockParamKind::RequiredArg,
                    &mut out,
                );
            }
            for opt in p.optionals().iter() {
                push_block_positional(
                    &opt,
                    &mut first_positional_taken,
                    BlockParamKind::OptionalArg,
                    &mut out,
                );
            }
            // `rest` may be a named/anonymous `*r` (`RestParameterNode`) or
            // the trailing-comma `|v,|` (`ImplicitRestNode`) — the latter
            // binds no name but still counts as a rest for `splats?`.
            if let Some(rest) = p.rest() {
                if let Some(r) = rest.as_rest_parameter_node() {
                    if let Some(name) = r.name() {
                        out.push((
                            constant_string(name.as_slice()),
                            BlockParamKind::Rest,
                        ));
                    } else {
                        out.push((String::new(), BlockParamKind::ImplicitRest));
                    }
                } else if rest.as_implicit_rest_node().is_some() {
                    out.push((String::new(), BlockParamKind::ImplicitRest));
                }
            }
            for post in p.posts().iter() {
                push_block_other_positional(&post, &mut out);
            }
            for kw in p.keywords().iter() {
                if let Some(kwr) = kw.as_required_keyword_parameter_node() {
                    out.push((keyword_param_name(kwr.name().as_slice()), BlockParamKind::Keyword));
                } else if let Some(kwo) = kw.as_optional_keyword_parameter_node() {
                    out.push((keyword_param_name(kwo.name().as_slice()), BlockParamKind::Keyword));
                }
            }
            if let Some(kwr) = p.keyword_rest().and_then(|n| n.as_keyword_rest_parameter_node()) {
                if let Some(name) = kwr.name() {
                    out.push((constant_string(name.as_slice()), BlockParamKind::KwRest));
                }
            }
            if let Some(blk) = p.block() {
                if let Some(name) = blk.name() {
                    out.push((constant_string(name.as_slice()), BlockParamKind::Block));
                }
            }
        }
        // `|;local|` declarations: Prism reports them as
        // `BlockLocalVariableNode`s. They bind nothing the yield provides
        // (each starts nil at runtime), but they must still be hidden from
        // the enclosing env — `block_entry_env` removes `Local` names so a
        // body read of `local` cannot see the outer binding. The reference
        // leaves them readable through `block_entry_scope`, a leak that
        // produces extra outer-typed arms; hiding is the safe side.
        for local in bp.locals().iter() {
            if let Some(t) = local.as_block_local_variable_node() {
                out.push((constant_string(t.name().as_slice()), BlockParamKind::Local));
            }
        }
    } else if params.as_it_parameters_node().is_some() {
        // `{ it }` — the implicit single parameter is fed the receiver.
        out.push(("it".to_string(), BlockParamKind::SelfArg));
    } else if let Some(np) = params.as_numbered_parameters_node() {
        // `{ _1 + _2 }` — every `_N` is a REQUIRED positional in the
        // reference's `ParameterShape.of_arity`, so `_2..` are
        // `RequiredArg`: they auto-splat an array receiver exactly as
        // `|a, b|` does.
        for i in 1..=np.maximum() {
            out.push((
                format!("_{i}"),
                if i == 1 { BlockParamKind::SelfArg } else { BlockParamKind::RequiredArg },
            ));
        }
    }
    out
}

/// One positional entry of a block's `ParametersNode` — a `RequiredParameterNode`
/// or `OptionalParameterNode` binds its single name; a `MultiTargetNode` (a
/// destructured `|(v, w)|`) binds every nested local-target name. The FIRST
/// positional is a `yield self` argument ([`BlockParamKind::SelfArg`] /
/// [`BlockParamKind::SelfOpt`] / [`BlockParamKind::DestructuredSelfArg`]);
/// every later one keeps its slot class (`later_kind` — `RequiredArg` /
/// `OptionalArg`), except a nested destructure, whose names bind
/// [`BlockParamKind::Other`].
fn push_block_positional(
    node: &PrismNode<'_>,
    first_positional_taken: &mut bool,
    later_kind: BlockParamKind,
    out: &mut Vec<(String, BlockParamKind)>,
) {
    if let Some(req) = node.as_required_parameter_node() {
        let kind = if *first_positional_taken {
            later_kind
        } else {
            BlockParamKind::SelfArg
        };
        *first_positional_taken = true;
        out.push((constant_string(req.name().as_slice()), kind));
    } else if let Some(opt) = node.as_optional_parameter_node() {
        let kind = if *first_positional_taken {
            later_kind
        } else {
            BlockParamKind::SelfOpt
        };
        *first_positional_taken = true;
        out.push((constant_string(opt.name().as_slice()), kind));
    } else if let Some(mt) = node.as_multi_target_node() {
        let kind = if *first_positional_taken {
            BlockParamKind::Other
        } else {
            BlockParamKind::DestructuredSelfArg
        };
        *first_positional_taken = true;
        multi_target_names(&mt.as_node(), out, kind);
    }
    // Any other positional shape binds no name (e.g. an anonymous `|` hole).
}

/// A NON-first positional — a post-parameter (`|*r, z|`'s `z`) or a nested
/// destructured group behind the first. A single name binds
/// [`BlockParamKind::PostArg`]; a destructure's names bind
/// [`BlockParamKind::Other`].
fn push_block_other_positional(node: &PrismNode<'_>, out: &mut Vec<(String, BlockParamKind)>) {
    if let Some(req) = node.as_required_parameter_node() {
        out.push((constant_string(req.name().as_slice()), BlockParamKind::PostArg));
    } else if let Some(mt) = node.as_multi_target_node() {
        multi_target_names(&mt.as_node(), out, BlockParamKind::Other);
    }
}

/// Every local-target name inside a destructured parameter (`|(v, (w, *r))|`),
/// recursively — `lefts`, an optional `rest` (a `SplatNode` wrapping a target,
/// or a nested `MultiTargetNode`), and `rights`.
fn multi_target_names(node: &PrismNode<'_>, out: &mut Vec<(String, BlockParamKind)>, kind: BlockParamKind) {
    // Inside a block-parameter list the destructured names are
    // `RequiredParameterNode`s / `OptionalParameterNode`s / `RestParameterNode`s
    // (they read as `|(a, b)|`, not as assignment targets); assignment-style
    // destructures keep `LocalVariableTargetNode`.  Handle both shapes.
    if let Some(r) = node.as_required_parameter_node() {
        out.push((constant_string(r.name().as_slice()), kind));
        return;
    }
    if let Some(o) = node.as_optional_parameter_node() {
        out.push((constant_string(o.name().as_slice()), kind));
        return;
    }
    if let Some(r) = node.as_rest_parameter_node() {
        if let Some(nm) = r.name() {
            out.push((constant_string(nm.as_slice()), kind));
        }
        return;
    }
    if let Some(t) = node.as_local_variable_target_node() {
        out.push((constant_string(t.name().as_slice()), kind));
        return;
    }
    if let Some(splat) = node.as_splat_node() {
        if let Some(expr) = splat.expression() {
            multi_target_names(&expr, out, kind);
        }
        return;
    }
    if let Some(mt) = node.as_multi_target_node() {
        for l in mt.lefts().iter() {
            multi_target_names(&l, out, kind);
        }
        if let Some(rest) = mt.rest() {
            multi_target_names(&rest, out, kind);
        }
        for r in mt.rights().iter() {
            multi_target_names(&r, out, kind);
        }
    }
}

/// A keyword parameter's LOCAL name — Prism's `name` constant is the full
/// `k:` symbol, so strip the trailing colon the local binding never carries.
fn keyword_param_name(raw: &[u8]) -> String {
    let s = constant_string(raw);
    s.strip_suffix(':').unwrap_or(&s).to_string()
}
