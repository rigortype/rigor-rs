//! ADR-0023 tier-4b: project method RETURN inference (Pass 3) and the call-site parameter-binding
//! descriptors (Pass 3b), with the lookups the call hook reads.

use std::collections::{HashMap, HashSet};

use rigor_index::CoreIndex;
use rigor_parse::{LoweredAst, MethodBody, Node};
use rigor_types::Interner;

use super::{ParamBoundReturn, SourceIndex};

impl SourceIndex {
    /// The inferred CORE return-class NAME for a project method `(class,
    /// method)`, if tier-4b inferred one. `None` ⇒ no entry ⇒ the call types
    /// Dynamic (silent). Re-intern at the call site via [`CoreIndex::class_id`].
    ///
    /// [`CoreIndex::class_id`]: rigor_index::CoreIndex::class_id
    pub fn method_return(&self, class: &str, method: &str) -> Option<&str> {
        self.method_returns
            .get(&(class.to_string(), method.to_string()))
            .map(|s| s.as_str())
    }

    /// The ADR-0023 tier-4b call-site PARAMETER-BINDING descriptor for a project
    /// method `(class, method)`, if its tail roots on a positional param. `None`
    /// ⇒ no param-bound entry ⇒ the call site falls through (Dynamic, silent).
    /// The param-INDEPENDENT [`Self::method_return`] takes precedence at the
    /// call site: it is consulted FIRST and this map only on a miss. That
    /// precedence — not exclusivity — is the contract. A method reopened across
    /// files CAN have an entry in both maps (each def site is dispatched on its
    /// own; issue #92 §8), which is exactly why the order matters.
    /// See [`ParamBoundReturn`].
    pub fn param_bound_return(&self, class: &str, method: &str) -> Option<&ParamBoundReturn> {
        self.param_bound_returns
            .get(&(class.to_string(), method.to_string()))
    }
}

/// ADR-0023 tier-4b RETURN inference (the zero-FP minimal slice). For every
/// direct instance method `(class C, method m, body b)` harvested across all
/// `asts`, type `m`'s RETURN (tail) expression under an EMPTY [`TypeEnv`] using a
/// [`Typer`] over `core` + the already-built `idx`, and record `(C, m) -> core
/// class NAME` ONLY when the tail types to a concrete core/RBS class.
///
/// ## Why an EMPTY env is the whole safety argument
///
/// Typing the body under an empty env means any dependence on params / `self` /
/// ivars / branches / OTHER in-source methods naturally yields `Dynamic` (a param
/// read isn't bound, an ivar/self/unknown-constant types Dynamic, an in-source
/// method call resolves to a source Nominal whose core name is `None`), so the
/// concrete-core-class gate declines automatically. The witnessed return set is a
/// strict subset of the reference's body inference.
///
/// ## The gates (any failure ⇒ NO entry; see `check_rules` parity notes)
///
/// 1. Direct instance method — already guaranteed by harvesting (only named,
///    direct `Definition`s are in `method_bodies`; `def self.x` is excluded).
/// 2. Empty/absent body ⇒ decline.
/// 3. `has_explicit_return` (any `return` in the body) ⇒ decline — we read only
///    the tail; an explicit return could carry a different type.
/// 4. The tail is a branch/loop carrier (`If`/`Case`/`Loop`/`Logical`/
///    `BeginRescue`) ⇒ decline — no single concrete return.
/// 5. The tail types (empty env) to anything but a concrete core/RBS class
///    (Dynamic, a source Nominal, or `!knows_class`) ⇒ decline. This single
///    check subsumes param/ivar/self/unknown-constant/in-source-call/
///    non-foldable-call — all already Dynamic under the empty env.
/// 6. Reopen disagreement: the same `(C, m)` inferred twice with DIFFERENT core
///    returns ⇒ remove the entry (decline). Same return twice ⇒ keep.
///
/// ## Pass 3b — call-site PARAMETER BINDING (the param-DEPENDENT companion)
///
/// A method whose tail is a bare positional-PARAM read, or a no-arg core-method
/// CHAIN rooted at one (`def up(x); x.upcase; end`), is param-DEPENDENT, so it
/// yields no entry above (gate 5: a param read is Dynamic under the empty env).
/// We additionally record a [`ParamBoundReturn`] for it so the call site can bind
/// the ARGUMENT's type to the param and re-derive the core return. The extra
/// gates (any failure ⇒ NO param-bound entry, see [`infer_one_param_bound`]):
///   * the method must declare PLAIN POSITIONAL params only (`mb.params ==
///     Some(_)` — splat/post/kwargs/block/optional ⇒ `None` ⇒ decline);
///   * the tail's ROOT receiver must be a bare read of one of those params;
///   * every step of the chain must be a no-arg call (an arg would itself need
///     binding, which we don't model) ⇒ decline otherwise.
///
/// The same gates 2/3/4 (empty body / explicit return / branch tail) and the
/// reopen-disagreement rule apply, tracked independently from the param-
/// independent map.
///
/// ## Which map wins (the two are NOT mutually exclusive)
///
/// Per DEF SITE they are: one tail is either a concrete core class under the
/// empty env or param-rooted, never both, because [`infer_one_param_bound`] is
/// only consulted in the `else` arm below. Per `(class, method)` KEY they are
/// NOT — a method REOPENED across files has two independent sites, each
/// dispatched on its own, so `A#m` can land in both maps at once (issue #92 §8
/// probed exactly that: `def m; "s"; end` in one file, `def m(x); x; end` in
/// another). This is harmless because the call site consults `method_return`
/// FIRST and `param_bound_return` only on a miss (documented at
/// [`SourceIndex::param_bound_returns`]) — but do not lean on an exclusivity
/// that does not hold. The doc claim here used to assert it did.
// type_complexity: the two-map return shape is the real, documented output of this
// pass (param-independent vs param-bound returns); a type alias would only hide it.
#[allow(clippy::type_complexity)]
pub(crate) fn infer_method_returns(
    idx: &SourceIndex,
    core: &CoreIndex,
    asts: &[&LoweredAst],
) -> (
    HashMap<(String, String), String>,
    HashMap<(String, String), ParamBoundReturn>,
) {
    let typer = crate::Typer::with_source(core, idx);
    let empty_env = crate::TypeEnv::new();

    let mut returns: HashMap<(String, String), String> = HashMap::new();
    // Track keys seen with a DISAGREEING reopen so they are never re-added.
    let mut disagreed: HashSet<(String, String)> = HashSet::new();

    // Param-bound (call-site-binding) descriptors, with their own disagreement
    // blacklist (a reopen with a DIFFERENT param-bound shape ⇒ decline).
    let mut param_bound: HashMap<(String, String), ParamBoundReturn> = HashMap::new();
    let mut pb_disagreed: HashSet<(String, String)> = HashSet::new();

    // Upstream v0.4.5 (#2003): a same-file non-`def` definer written after a
    // `def` replaces it, so the `def` body no longer types the call. Both maps
    // start with those keys blacklisted.
    for ast in asts {
        for key in superseded_defs(ast) {
            disagreed.insert(key.clone());
            pb_disagreed.insert(key);
        }
    }

    for ast in asts {
        for (_, node) in ast.iter() {
            let (class_name, method_bodies) = match node {
                Node::ClassDef { name, method_bodies, .. } if !name.is_empty() => {
                    (name.as_str(), method_bodies)
                }
                Node::ModuleDef { name, method_bodies, .. } if !name.is_empty() => {
                    (name.as_str(), method_bodies)
                }
                _ => continue,
            };
            for mb in method_bodies {
                let key = (class_name.to_string(), mb.name.clone());
                if let Some(core_name) = infer_one_return(ast, &typer, core, &empty_env, mb) {
                    if disagreed.contains(&key) {
                        continue; // a prior reopen disagreed ⇒ stay declined.
                    }
                    match returns.get(&key) {
                        Some(prev) if prev != &core_name => {
                            // Gate 6: disagreeing reopens ⇒ remove + blacklist.
                            returns.remove(&key);
                            disagreed.insert(key);
                        }
                        _ => {
                            returns.insert(key, core_name);
                        }
                    }
                } else if let Some(pb) = infer_one_param_bound(ast, mb) {
                    // Pass 3b: a param-rooted tail. Same reopen-disagreement rule.
                    if pb_disagreed.contains(&key) {
                        continue;
                    }
                    match param_bound.get(&key) {
                        Some(prev) if prev != &pb => {
                            param_bound.remove(&key);
                            pb_disagreed.insert(key);
                        }
                        _ => {
                            param_bound.insert(key, pb);
                        }
                    }
                }
            }
        }
    }
    (returns, param_bound)
}

/// Upstream v0.4.5 (#2003, `scope_indexer.rb` `call_superseded_names` /
/// `supersede_def_nodes`): the `(class, method)` keys whose `def` a later
/// non-`def` definer in the same file replaces — each reader / writer an
/// `attr_*` call declares (`attr_writer` only `x=`), a literal
/// `define_method(:x)`, the new name of `alias_method :x, :y` or `alias x y`.
/// The reference contests the slot, so the readers that re-type the `def` body
/// decline. Events are read per class name across the file's bodies (a
/// same-file reopening counts, another file does not) in source order; a `def`
/// written after the definer wins again. A definer inside a method body, a
/// `class << self` body or a nested class belongs to another scope. The
/// recognition is generous (any receiverless or `self.` call, the `send`
/// family with a literal macro name, String or Symbol names): an extra key
/// only declines.
/// The event name of an unreadable `alias_method`: it supersedes every name
/// its class wrote a `def` of before it. Not a Ruby method name.
const WILDCARD_NAME: &str = "\0*";

fn superseded_defs(ast: &LoweredAst) -> HashSet<(String, String)> {
    use rigor_parse::Span;
    let within = |a: Span, b: Span| b.0 <= a.0 && a.1 <= b.1;
    let literal = |id: rigor_parse::NodeId| match ast.get(id) {
        Node::SymbolLit { value, .. } | Node::StringLit { value, .. } => Some(value.clone()),
        _ => None,
    };
    // (class name, offset, method, is_def)
    let mut events: Vec<(String, usize, String, bool)> = Vec::new();
    let scopes: Vec<(rigor_parse::NodeId, Span)> = ast
        .iter()
        .filter(|(_, n)| {
            matches!(n, Node::ClassDef { .. } | Node::ModuleDef { .. } | Node::Definition { .. })
        })
        .map(|(id, n)| (id, n.span()))
        .collect();
    for (class_id, node) in ast.iter() {
        let (Node::ClassDef { name, span, .. } | Node::ModuleDef { name, span, .. }) = node else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let class_span = *span;
        // The scopes nested in this body: a node inside one is not this
        // body's statement (a named `def` itself still is).
        let nested: Vec<(rigor_parse::NodeId, Span)> = scopes
            .iter()
            .filter(|(id, s)| *id != class_id && within(*s, class_span))
            .copied()
            .collect();
        for (id, n) in ast.iter() {
            let nspan = n.span();
            if id == class_id || !within(nspan, class_span) {
                continue;
            }
            if nested.iter().any(|(sid, s)| *sid != id && within(nspan, *s)) {
                continue;
            }
            match n {
                Node::Definition { name: Some(m), .. } => {
                    events.push((name.clone(), nspan.0, m.clone(), true));
                }
                Node::Alias { new_name, .. } => {
                    if let Some(m) = literal(*new_name) {
                        events.push((name.clone(), nspan.0, m, false));
                    }
                }
                Node::Call { receiver, method, args, .. } => {
                    if receiver.is_some_and(|r| !matches!(ast.get(r), Node::SelfExpr { .. })) {
                        continue;
                    }
                    let (macro_name, rest) = match method.as_str() {
                        "send" | "public_send" | "__send__" => {
                            let Some(m) = args.first().and_then(|&a| literal(a)) else { continue };
                            (m, &args[1..])
                        }
                        _ => (method.clone(), &args[..]),
                    };
                    let names: Vec<String> = rest.iter().filter_map(|&a| literal(a)).collect();
                    // Upstream v0.4.6 (#2225, `unread_alias_method?`): an
                    // `alias_method` whose two names `AliasNames` cannot read
                    // (`alias_method n, :to_s`, `self.alias_method :x, :y`, a
                    // splat) still replaces a method: every name the class
                    // wrote before it is contested (`AliasMap#copy_into`).
                    if macro_name == "alias_method"
                        && (receiver.is_some() || rest.len() != 2 || names.len() != 2)
                    {
                        events.push((name.clone(), nspan.0, WILDCARD_NAME.to_string(), false));
                        continue;
                    }
                    let members: Vec<String> = match macro_name.as_str() {
                        "attr_reader" => names,
                        "attr_writer" => names.iter().map(|b| format!("{b}=")).collect(),
                        "attr_accessor" => names
                            .iter()
                            .flat_map(|b| [b.clone(), format!("{b}=")])
                            .collect(),
                        "define_method" | "alias_method" => names.into_iter().take(1).collect(),
                        _ => continue,
                    };
                    for m in members {
                        events.push((name.clone(), nspan.0, m, false));
                    }
                }
                _ => {}
            }
        }
    }
    events.sort_by_key(|(_, off, _, _)| *off);
    // (class, method) -> (a def seen, the last event was a definer).
    let mut state: HashMap<(String, String), (bool, bool)> = HashMap::new();
    for (class, _, method, is_def) in events {
        if method == WILDCARD_NAME {
            for ((c, _), slot) in state.iter_mut() {
                if *c == class && slot.0 {
                    slot.1 = true;
                }
            }
            continue;
        }
        let slot = state.entry((class, method)).or_insert((false, false));
        if is_def {
            *slot = (true, false);
        } else if slot.0 {
            slot.1 = true;
        }
    }
    state
        .into_iter()
        .filter(|(_, (_, superseded))| *superseded)
        .map(|(key, _)| key)
        .collect()
}

/// Run gates 2–5 for one method body and return the inferred CORE class NAME, or
/// `None` to decline. Uses a fresh scratch [`Interner`] per call (the inferred
/// NAME is what we keep; the interned ids are throwaway, re-interned at the call
/// site against the analysis interner).
fn infer_one_return(
    ast: &LoweredAst,
    typer: &crate::Typer<'_>,
    core: &CoreIndex,
    empty_env: &crate::TypeEnv,
    mb: &MethodBody,
) -> Option<String> {
    // Gate 3: any explicit `return` ⇒ decline.
    if mb.has_explicit_return {
        return None;
    }
    // Gate 2: empty/absent body ⇒ decline. The return expression is the LAST
    // direct statement (lowering flattened the Statements wrapper).
    let &ret_id = mb.body.last()?;

    // Gate 4: a branch/loop carrier tail has no single concrete return ⇒ decline.
    if is_branch_carrier(ast.get(ret_id)) {
        return None;
    }

    // Gate 5: type the tail under the EMPTY env; keep ONLY a concrete core/RBS
    // class. A scratch interner is fine — we discard the ids and keep the name.
    let mut scratch = Interner::new();
    let ty = typer.type_of(ast, ret_id, empty_env, &mut scratch);
    let core_name = core.class_name_of(&scratch, ty)?;
    if core.knows_class(core_name) {
        Some(core_name.to_string())
    } else {
        None
    }
}

/// Run the call-site PARAMETER-BINDING gates for one method body and return a
/// [`ParamBoundReturn`] descriptor, or `None` to decline. Called ONLY when the
/// param-independent [`infer_one_return`] already declined (the tail is not a
/// concrete core class under the empty env) — so this never double-records.
///
/// The accepted tail shapes (anything else ⇒ `None`):
///   * a bare positional-param read (`def full(x); x; end`) ⇒
///     `ParamBoundReturn { param_index, chain: [] }`;
///   * a no-arg core-method CHAIN whose ROOT receiver is a bare positional-param
///     read (`def up(x); x.upcase.strip; end`) ⇒ `{ param_index, chain:
///     ["upcase", "strip"] }`.
///
/// Gates (any failure ⇒ `None`; a decline is never a false positive):
///   * `has_explicit_return` ⇒ decline (gate 3 — we read only the tail);
///   * empty body ⇒ decline (gate 2);
///   * `params == None` (splat/post/kwargs/block/optional) ⇒ decline — the
///     call-site positional binder needs a clean 1:1 index mapping;
///   * the tail's root isn't a bare read of a declared positional param ⇒
///     decline (an ivar/self/local-not-a-param/another-param-combination root is
///     not bindable here);
///   * any chain step carries ARGUMENTS ⇒ decline (we bind only the root param;
///     a step arg would itself need binding, which this slice doesn't model);
///   * any chain step carries a BLOCK ⇒ decline (the block-overload return is a
///     separate model; keep this purely the no-arg/no-block core path).
fn infer_one_param_bound(ast: &LoweredAst, mb: &MethodBody) -> Option<ParamBoundReturn> {
    // Gate 3: any explicit `return` ⇒ decline.
    if mb.has_explicit_return {
        return None;
    }
    // Only plain-positional signatures bind (None ⇒ splat/kwargs/etc. ⇒ decline).
    let params = mb.params.as_ref()?;
    // Gate 2: empty/absent body ⇒ decline.
    let &ret_id = mb.body.last()?;

    // Peel the no-arg/no-block core-method chain off the tail, innermost-last:
    // `x.upcase.strip` walks `strip`'s receiver `x.upcase`, then `upcase`'s
    // receiver `x`, collecting method names; the innermost receiver must be a
    // bare param read. We push outer-first then reverse to source (apply) order.
    let mut chain: Vec<String> = Vec::new();
    let mut cursor = ret_id;
    loop {
        match ast.get(cursor) {
            // A bare local read: the chain root. It must name a declared
            // positional param (its index is the binding slot).
            Node::LocalVariableRead { name, .. } => {
                let param_index = params.iter().position(|p| p == name)?;
                chain.reverse(); // collected outer-first ⇒ flip to apply order.
                return Some(ParamBoundReturn { param_index, chain });
            }
            // A call on a receiver: a chain step. It must be a NO-ARG, NO-BLOCK
            // call (an arg/block would need its own binding we don't model).
            Node::Call { receiver: Some(r), method, args, block_body, .. } => {
                if !args.is_empty() || !block_body.is_empty() {
                    return None;
                }
                chain.push(method.clone());
                cursor = *r;
            }
            // Anything else as the root (ivar/self/literal/another carrier) ⇒
            // not a bindable param tail.
            _ => return None,
        }
    }
}

/// Whether a tail node is a branch/loop carrier whose type is not a single
/// concrete class (gate 4). `BeginRescue` also covers a lowered parenthesized
/// expression and an inline `rescue` body — both decline conservatively.
fn is_branch_carrier(node: &Node) -> bool {
    matches!(
        node,
        Node::If { .. }
            | Node::Case { .. }
            | Node::When { .. }
            | Node::Loop { .. }
            | Node::Logical { .. }
            | Node::BeginRescue { .. }
    )
}
