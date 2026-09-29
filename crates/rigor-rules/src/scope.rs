//! Scope helpers the rules share: the per-use-site local env (`ScopedEnv`),
//! span containment, and lexical class-name qualification.

use rigor_infer::Typer;
use rigor_parse::{LoweredAst, Node};
use rigor_types::Interner;

/// ADR-35 slice 1: map every `ClassDef`/`ModuleDef` arena id to its FULLY
/// LEXICALLY-QUALIFIED name (`module Outer; module Inner` -> `Inner` maps to
/// `Outer::Inner`), by a recursive walk from the program root tracking the
/// enclosing class/module prefix. This is the SAME qualification the source
/// index's override walk uses, so a subclass and its ancestors key consistently
/// — the zero-FP keystone against last-component name collisions. A declaration
/// whose name is itself a path (`class Foo::Bar`) qualifies head-first.
pub(crate) fn qualified_class_names(ast: &LoweredAst) -> std::collections::HashMap<rigor_parse::NodeId, String> {
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

/// Whether `inner` is contained within `outer` (`outer.start <= inner.start` and
/// `inner.end <= outer.end`). Half-open byte spans; equal spans count as within.
pub(crate) fn span_within(inner: rigor_parse::Span, outer: rigor_parse::Span) -> bool {
    outer.0 <= inner.0 && inner.1 <= outer.1
}

/// The file's top-level local env, plus the method-body spans it must NOT reach
/// into.
///
/// The rules walk types every use site against ONE flat env keyed by name, built
/// from the file's TOP-LEVEL writes (`build_toplevel_env` does not descend into
/// `def` bodies). A Ruby method body is an independent local scope, so a name
/// inside a `def` is that def's parameter or its own write — never the top-level
/// local of the same name. Reading the flat env there types the wrong value:
/// rigor-survey `Ruby/data_structures/hash_table/anagram_checker.rb` closes a
/// `s = 'a'` / `t = 'ab'` driver section and then reopens `def is_anagram(s, t)`,
/// and the parameters were typed as those two driver strings — two
/// `call.wrong-arity` and two `call.undefined-method` false positives.
///
/// [`Self::at`] therefore hands a use site inside a method body an EMPTY env.
/// That is a strict loss of information and so cannot add a diagnostic; the
/// bindings it withholds were all wrong. Method-body locals are not typed by
/// this walk at all today (they are absent from the flat env), so nothing that
/// used to fire correctly stops firing.
///
/// The top-level env widens every local a nested construct rebinds
/// (`Typer::build_toplevel_check_env`, rigor-rs#133): the flat binder cannot see
/// a rebind inside an `if`, a loop or a block, nor one on a `next` / `break`
/// path. Widening only ever declines a rule that needs a concrete receiver — but
/// the class-narrowing and collection-shape rules fire ONLY on a `Dynamic`
/// carrier, so for them a widened local would OPEN the gate the stale concrete
/// type closed. They read the unwidened env through [`Self::gate_at`], exactly
/// as before.
/// inside any method body.
///
/// At file scope [`Self::at`] replays the recorded rebinds/mutations in
/// evaluation order up to the site (`Typer::check_env_at`, rigor-rs#136 — the
/// port of the reference's per-node scope index): an operand typed earlier
/// than a same-statement `local.<mutator>` call or rebind still reads the
/// binding it saw, while a later operand sees the effect. Sites inside a
/// literal block / lambda / class / module body keep the flat env — a
/// closure may run at any later point.
///
/// rigor-rs#137 (upstream rigor#1245) then shadows whichever env the site
/// resolved: a block or lambda is a LEXICAL boundary — a local the literal
/// closure BINDS (every parameter form, a `;` block-local, a name first
/// assigned in the body) reads `Dynamic[top]` at any use site inside the
/// body, never the outer local of the same name it shadows
/// (`show([1, 2].map { |o| o + 1 })` must not type `o` as the outer
/// `{x: 1}`). The reference computes this from the scope index — an
/// unentered closure's fallback scope shadows every `locals` name to
/// `untyped`, and an entered block binds the real parameter types.
/// `Dynamic[top]` is the port's uniform FP-safe floor for both: the
/// value-position rows land exactly on the reference (silent), and in-place
/// rows decline where the reference would fire on the parameter's element
/// type. Membership is STRUCTURAL (`closure_shadow_scopes`, the #166 heredoc
/// lesson), not span containment.
pub(crate) struct ScopedEnv {
    flow: rigor_infer::CheckFlow,
    gate_top: rigor_infer::TypeEnv,
    empty: rigor_infer::TypeEnv,
    method_bodies: Vec<rigor_parse::Span>,
    /// `(body descendant ids, bound names)` per literal block/lambda.
    closures: Vec<(std::collections::HashSet<rigor_parse::NodeId>, Vec<String>)>,
    untyped: rigor_types::TypeId,
}

impl ScopedEnv {
    pub(crate) fn build(typer: &Typer, ast: &LoweredAst, interner: &mut Interner) -> Self {
        ScopedEnv {
            flow: typer.build_toplevel_check_flow(ast, interner),
            gate_top: typer.build_toplevel_env(ast, interner),
            empty: rigor_infer::TypeEnv::new(),
            method_bodies: rigor_infer::method_body_spans(ast),
            closures: rigor_infer::closure_shadow_scopes(ast),
            untyped: interner.untyped(),
        }
    }

    /// The env a use site at `site` (node `id`) may read: the entered-scope
    /// env at file scope — `Typer::check_env_at` replays this statement's
    /// recorded rebinds/mutations in evaluation order up to the site
    /// (rigor-rs#136), and a site inside a barrier scope (a block/lambda or
    /// `def`/`class`/`module` body) keeps the flat env — an empty env inside
    /// any method body. On top of whichever env the site resolves, a site
    /// inside a literal block/lambda body reads every name the enclosing
    /// closures bind as `Dynamic[top]` (the `Self::closures` shadow set,
    /// rigor-rs#137). `Cow` so an unshadowed site borrows the resolved env
    /// without a clone.
    pub(crate) fn at<'a>(
        &'a self,
        ast: &LoweredAst,
        typer: &Typer,
        site: rigor_parse::Span,
        id: rigor_parse::NodeId,
        interner: &mut Interner,
    ) -> std::borrow::Cow<'a, rigor_infer::TypeEnv> {
        use std::borrow::Cow;
        if self.in_method_body(site) {
            return Cow::Borrowed(&self.empty);
        }
        let env = typer.check_env_at(ast, &self.flow, site, interner);
        let mut shadowed: Option<rigor_infer::TypeEnv> = None;
        for (descendants, bound) in &self.closures {
            if !descendants.contains(&id) {
                continue;
            }
            for name in bound {
                if env.contains_key(name.as_str()) {
                    shadowed
                        .get_or_insert_with(|| env.as_ref().clone())
                        .insert(name.clone(), self.untyped);
                }
            }
        }
        match shadowed {
            Some(env) => Cow::Owned(env),
            None => env,
        }
    }

    /// [`Self::at`] for the `Dynamic`-only gates of the class-narrowing and
    /// collection-shape rules: the unwidened top-level env. Deliberately NOT
    /// closure-shadowed: the snapshot passes that mint those candidates record
    /// their facts against the outer bindings (the class-narrowing descent
    /// keeps them — shadowing there would narrow an untyped param past a
    /// guard the reference's concrete element type renders disjoint), so the
    /// gate must read the same env the facts were computed on. Declines are
    /// the safe side of a shadowed name either way.
    pub(crate) fn gate_at(&self, span: rigor_parse::Span) -> &rigor_infer::TypeEnv {
        if self.in_method_body(span) {
            &self.empty
        } else {
            &self.gate_top
        }
    }

    fn in_method_body(&self, span: rigor_parse::Span) -> bool {
        self.method_bodies.iter().any(|d| span_within(span, *d))
    }
}
