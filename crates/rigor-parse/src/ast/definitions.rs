//! What the `def`/`class`/`module` arms read off the Prism tree before lowering
//! erases it: parameter shapes, the ADR-35 visibility table, and the tier-4b
//! method-body harvest.

use crate::ruby_prism::{self, Node as PrismNode};

use super::{constant_path_string, constant_string, Builder, Node, NodeId};

/// A direct instance method harvested for ADR-0023 tier-4b RETURN inference:
/// the method `name`, the lowered `body` statement ids (so the return
/// expression can be typed), and `has_explicit_return` (any `return` in the
/// Prism body ⇒ the inference declines). Carried on [`Node::ClassDef`] /
/// [`Node::ModuleDef`] alongside the method-name list.
///
/// `params` records the method's PLAIN-POSITIONAL parameter names in order
/// (`def full(x, y)` -> `Some(["x", "y"])`), enabling ADR-0023 tier-4b call-site
/// PARAMETER BINDING: a method whose tail reads / chains off a bare positional
/// param can have its return re-derived from the ARGUMENT type at the call site.
/// It is `None` — meaning "decline this method for param binding" — whenever the
/// signature has ANYTHING that breaks positional index<->arg alignment: a splat
/// (`*args`), a post-splat positional, a keyword / double-splat (`**`), a block
/// param (`&blk`), or a default-valued (optional) param (`def f(x = 1)`). The
/// param-INDEPENDENT inference (a tail that types to a concrete core class under
/// an empty env) is unaffected by `params` — it never reads a param.
#[derive(Clone, Debug)]
pub struct MethodBody {
    pub name: String,
    pub body: Vec<NodeId>,
    pub has_explicit_return: bool,
    /// `Some(plain positional param names in order)`, or `None` to DECLINE
    /// param binding for this method (splat/post/kwargs/block/optional present).
    pub params: Option<Vec<String>>,
}

/// The RBS-relevant STRUCTURE of a method's parameter list — the counts + flags
/// `sig-gen`'s `initialize` stub renders (`(untyped, ?untyped, *untyped, name:
/// untyped, ?opt: untyped, **untyped, ?{ (?) -> void })`). Distinct from
/// [`MethodBody::params`], which captures only plain-positional NAMES and
/// declines any complex shape. Posts (`def f(a, *rest, b)`'s `b`) are
/// deliberately omitted — the reference's stub renderer drops them too.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParamShape {
    /// Number of required positionals (each → `untyped`).
    pub required: usize,
    /// Number of optional positionals (each → `?untyped`).
    pub optional: usize,
    /// A rest param `*args` is present (→ `*untyped`).
    pub has_rest: bool,
    /// Keyword params in source order: `(name, is_optional)` (→ `name: untyped`
    /// / `?name: untyped`).
    pub keywords: Vec<(String, bool)>,
    /// A keyword-rest `**opts` is present (→ `**untyped`).
    pub has_kwrest: bool,
    /// A block param `&blk` is present (→ `?{ (?) -> void }`).
    pub has_block: bool,
}

impl ParamShape {
    /// A trivial parameter list (all-empty) — the reference EXCLUDES a trivial
    /// `initialize` from the stub (the `Object#initialize` RBS covers it).
    pub fn is_trivial(&self) -> bool {
        self.required == 0
            && self.optional == 0
            && !self.has_rest
            && self.keywords.is_empty()
            && !self.has_kwrest
            && !self.has_block
    }
}

/// Instance-method visibility as discovered at lowering time (ADR-35 slice 1,
/// the `def.override-visibility-reduced` rule). Mirrors the reference's
/// `scope_indexer.rb` visibility table semantics exactly:
///   * a class/module body is walked left-to-right with a running default that
///     starts [`Visibility::Public`];
///   * a bare `private` / `protected` / `public` call (no args) FLIPS the
///     running default for subsequent `def`s;
///   * `private :foo, :bar` / `private "foo"` (literal symbol/string args)
///     BACK-PATCHES those named methods to that visibility;
///   * a plain `def foo` records `foo` at the current running default;
///   * `private def foo` (the modifier-wrapping-a-def form) is NOT tracked — it
///     records as the running default — matching the reference gap exactly so
///     the witness set stays ⊆ the reference's;
///   * dynamic forms (`send(:private, …)`, `private(*names)`) are NOT recognised;
///   * singleton defs (`def self.x`, inside `class << self`) are EXCLUDED.
///
/// The `Ord`-by-rank comparison (public > protected > private) lives in the rule
/// layer; this enum only carries the discovered atom.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Visibility {
    Public,
    Protected,
    Private,
}

impl<'src> Builder<'src> {
    /// Harvest the `(name, body, has_explicit_return)` of every DIRECT instance
    /// method from the already-lowered class/module body's direct-child ids. A
    /// direct child that is a `Definition` with a `name` (i.e. an instance `def`
    /// — `def self.x` lowered to a name-less Definition) is a direct instance
    /// method; its lowered body and explicit-return flag are recorded for
    /// ADR-0023 tier-4b. Reads the arena read-only (the nodes already exist).
    pub(crate) fn harvest_method_bodies(&self, direct_children: &[NodeId]) -> Vec<MethodBody> {
        direct_children
            .iter()
            .filter_map(|&id| match &self.nodes[id.0 as usize] {
                Node::Definition {
                    name: Some(name),
                    has_explicit_return,
                    params,
                    body,
                    ..
                } => Some(MethodBody {
                    name: name.clone(),
                    body: body.clone(),
                    has_explicit_return: *has_explicit_return,
                    params: params.clone(),
                }),
                _ => None,
            })
            .collect()
    }
}

/// The instance-method names defined *directly* in a class/module body via
/// `def name`. Reads the Prism body before lowering (lowering drops a def's
/// name). Only direct, top-of-body defs are collected — methods inside nested
/// classes/conditionals are out of scope for this slice (and would be unsound to
/// attribute to the outer class). A singleton/`self.` def is excluded: it is not
/// an instance method, so it must not count toward instance-method existence.
pub(crate) fn direct_method_names(body: &PrismNode<'_>) -> Vec<String> {
    let mut names = Vec::new();
    let collect = |stmt: &PrismNode<'_>, names: &mut Vec<String>| {
        if let Some(def) = stmt.as_def_node() {
            // A def with a receiver (`def self.foo` / `def obj.foo`) is a
            // singleton method, NOT an instance method — exclude it.
            if def.receiver().is_none() {
                names.push(constant_string(def.name().as_slice()));
            }
        }
    };
    if let Some(stmts) = body.as_statements_node() {
        for stmt in stmts.body().iter() {
            collect(&stmt, &mut names);
        }
    } else {
        collect(body, &mut names);
    }
    names
}

/// ADR-35 slice 1 (`def.override-visibility-reduced`): discover a class/module
/// body's instance-method VISIBILITY table + its `include`/`prepend` ancestor
/// names, reading the Prism body BEFORE lowering. Mirrors
/// `scope_indexer.rb#build_discovered_method_visibilities` /
/// `collect_includes` EXACTLY (the witness set must stay ⊆ the reference's):
///
///   * the body is walked left-to-right with a running default that starts
///     [`Visibility::Public`];
///   * a bare `private` / `protected` / `public` call (receiver-less, NO args)
///     FLIPS the running default for subsequent `def`s;
///   * `private :foo, :bar` / `private "foo"` (literal symbol/string args ONLY)
///     BACK-PATCHES those named methods to that visibility WITHOUT changing the
///     running default; a non-literal arg in the list is ignored;
///   * a plain `def foo` (receiver-less) records `foo` at the running default;
///   * a `def foo` nested as an ARGUMENT to a modifier call (`private def foo`)
///     is recorded at the (unchanged) running default — NOT at the modifier's
///     visibility — exactly mirroring the reference's tracking gap;
///   * `include X` / `prepend X` collect `X`'s last path component (mirroring how
///     `superclass` is captured) for the MRO ancestor walk;
///   * a singleton def (`def self.x`) is EXCLUDED from the visibility table.
///
/// Back-patches are applied to the LAST recorded entry for a name (a reopened
/// `def` then `private :name` re-marks it), matching the reference's
/// last-write-wins accumulator.
pub(crate) fn discover_visibilities_and_includes(
    body: &PrismNode<'_>,
) -> (Vec<(String, Visibility)>, Vec<String>) {
    let mut vis: Vec<(String, Visibility)> = Vec::new();
    let mut includes: Vec<String> = Vec::new();
    let mut current = Visibility::Public;

    // A class/module body is a `StatementsNode` (or absent). Walk its direct
    // statements left-to-right; the order is what makes the running-default flow
    // correct. A bare (non-Statements) single-statement body is handled as one.
    if let Some(stmts) = body.as_statements_node() {
        for stmt in stmts.body().iter() {
            process_visibility_stmt(&stmt, &mut current, &mut vis, &mut includes);
        }
    } else {
        process_visibility_stmt(body, &mut current, &mut vis, &mut includes);
    }
    (vis, includes)
}

/// Apply one direct body statement to the running visibility default + the
/// discovered tables. See [`discover_visibilities_and_includes`] for the rules.
fn process_visibility_stmt(
    stmt: &PrismNode<'_>,
    current: &mut Visibility,
    vis: &mut Vec<(String, Visibility)>,
    includes: &mut Vec<String>,
) {
    // A receiver-less `def name` records at the running default.
    if let Some(def) = stmt.as_def_node() {
        if def.receiver().is_none() {
            vis.push((constant_string(def.name().as_slice()), *current));
        }
        return;
    }
    let Some(call) = stmt.as_call_node() else {
        return;
    };
    // Modifier / mixin calls only ever have an implicit-self receiver.
    if call.receiver().is_some() {
        return;
    }
    let name = constant_string(call.name().as_slice());
    if let Some(modifier) = visibility_of_modifier(&name) {
        let args = collect_call_args(&call);
        if args.is_empty() {
            // Bare modifier ⇒ flip the running default.
            *current = modifier;
        } else {
            // `private :foo, …` ⇒ back-patch the named methods (running default
            // unchanged for this form).
            for arg in &args {
                if let Some(target) = literal_symbol_or_string_name(arg) {
                    back_patch_visibility(vis, &target, modifier);
                }
            }
            // A `private def foo` arg records the nested def at the UNCHANGED
            // running default (the reference's tracking gap).
            record_nested_defs(&args, *current, vis);
        }
        return;
    }
    if name == "include" || name == "prepend" {
        for arg in &collect_call_args(&call) {
            // Capture the FULL written constant path (`Foo::Bar`, not just `Bar`)
            // so the override-visibility ancestor walk can resolve it against the
            // subclass's lexical nesting WITHOUT the name-collision merge that a
            // last-component-only name would cause (the gitlab-foss FP cluster).
            // A non-constant include arg yields an empty string ⇒ skipped.
            let path = constant_path_string(arg);
            if !path.is_empty() {
                includes.push(path);
            }
        }
    }
}

/// Map a receiver-less call name to its visibility, or `None` if it is not one
/// of the three modifiers.
fn visibility_of_modifier(name: &str) -> Option<Visibility> {
    match name {
        "public" => Some(Visibility::Public),
        "protected" => Some(Visibility::Protected),
        "private" => Some(Visibility::Private),
        _ => None,
    }
}

/// The positional arguments of a call as Prism nodes (empty if none).
fn collect_call_args<'pr>(call: &ruby_prism::CallNode<'pr>) -> Vec<PrismNode<'pr>> {
    call.arguments()
        .map(|a| a.arguments().iter().collect())
        .unwrap_or_default()
}

/// The literal method name a `private :foo` / `private "foo"` argument names, or
/// `None` for any non-literal (dynamic) argument — which the reference ignores.
fn literal_symbol_or_string_name(arg: &PrismNode<'_>) -> Option<String> {
    if let Some(sym) = arg.as_symbol_node() {
        return Some(String::from_utf8_lossy(sym.unescaped()).into_owned());
    }
    if let Some(s) = arg.as_string_node() {
        return Some(String::from_utf8_lossy(s.unescaped()).into_owned());
    }
    None
}

/// Re-mark the LAST recorded entry for `name` to `visibility` (last-write-wins,
/// matching the reference accumulator). No-op if the name was never recorded.
fn back_patch_visibility(vis: &mut [(String, Visibility)], name: &str, visibility: Visibility) {
    if let Some(slot) = vis.iter_mut().rev().find(|(n, _)| n == name) {
        slot.1 = visibility;
    }
}

/// Record any `def`s nested directly inside a modifier call's argument list
/// (`private def foo`) at the supplied running default. Mirrors the reference's
/// full-subtree recursion landing the inner def at the unchanged default.
fn record_nested_defs(
    args: &[PrismNode<'_>],
    current: Visibility,
    vis: &mut Vec<(String, Visibility)>,
) {
    for arg in args {
        if let Some(def) = arg.as_def_node() {
            if def.receiver().is_none() {
                vis.push((constant_string(def.name().as_slice()), current));
            }
        }
    }
}

/// The PLAIN-POSITIONAL parameter names of a `def` for ADR-0023 tier-4b
/// call-site PARAMETER BINDING, or `None` to DECLINE the method when its
/// signature has anything that breaks positional index<->argument alignment.
///
/// A method with NO parameters (`def f; ...; end`, Prism `parameters() == None`)
/// returns `Some([])` — there is nothing to bind, and the param-INDEPENDENT
/// inference still applies; the call-site binder just never reads an arg.
///
/// We accept ONLY `requireds` (the leading `x, y` positionals). Any of the
/// following makes the method decline (return `None`), because the call-site
/// binder maps positional ARG index -> positional PARAM index 1:1 and these
/// break that alignment:
///   * `optionals` — `def f(x = 1)`: a defaulted param may be filled by the
///     default (no arg) so arg index N need not be param N.
///   * `rest` — `*args`: a splat absorbs a variable arg count.
///   * `posts` — a positional AFTER a splat (`def f(*a, z)`): its arg index
///     depends on the splat length.
///   * `keywords` / `keyword_rest` — `k:`, `**opts`: keyword args are not
///     positional.
///   * `block` — `&blk`: a block param is not a positional arg.
pub(crate) fn plain_positional_params(params: Option<&ruby_prism::ParametersNode<'_>>) -> Option<Vec<String>> {
    let Some(params) = params else {
        // No parameter list at all ⇒ zero plain positionals (bindable, no-op).
        return Some(Vec::new());
    };
    // Decline on ANY non-plain-positional construct (conservative; a decline is
    // never a false positive — only a missed witness).
    if params.optionals().iter().next().is_some()
        || params.rest().is_some()
        || params.posts().iter().next().is_some()
        || params.keywords().iter().next().is_some()
        || params.keyword_rest().is_some()
        || params.block().is_some()
    {
        return None;
    }
    // Every required must be a simple named `RequiredParameterNode`. A
    // destructuring positional (`def f((a, b))`) is a `MultiTargetNode`, which
    // has no single name and breaks the 1:1 mapping ⇒ decline.
    let mut names = Vec::new();
    for req in params.requireds().iter() {
        let rp = req.as_required_parameter_node()?;
        names.push(constant_string(rp.name().as_slice()));
    }
    Some(names)
}

/// Capture the full RBS-relevant [`ParamShape`] from a Prism `ParametersNode`
/// (for `sig-gen`'s `initialize` stub). Mirrors the inputs the reference's
/// `render_initialize_param_list` reads — requireds/optionals counts, rest,
/// keyword `(name, optional)` in order, keyword-rest, block. POSTS are omitted
/// because the reference's renderer drops them.
pub(crate) fn param_shape_of(params: Option<&ruby_prism::ParametersNode<'_>>) -> ParamShape {
    let Some(p) = params else {
        return ParamShape::default();
    };
    let mut keywords = Vec::new();
    for kw in p.keywords().iter() {
        if let Some(req) = kw.as_required_keyword_parameter_node() {
            keywords.push((constant_string(req.name().as_slice()), false));
        } else if let Some(opt) = kw.as_optional_keyword_parameter_node() {
            keywords.push((constant_string(opt.name().as_slice()), true));
        }
    }
    ParamShape {
        required: p.requireds().iter().count(),
        optional: p.optionals().iter().count(),
        has_rest: p.rest().is_some(),
        keywords,
        has_kwrest: p.keyword_rest().is_some(),
        has_block: p.block().is_some(),
    }
}

/// Every name a `def`'s parameter list binds, for [`Node::Definition`]'s
/// `param_names`. A full subtree walk rather than a per-slot read, so a
/// destructured positional (`def f((a, *b))`) and every parameter kind are
/// covered without enumerating them twice; a default-value expression's own
/// nested parameters (a `->(x) {}` default) are collected too, which only
/// over-counts — the safe direction for the consumer.
pub(crate) fn all_param_names(params: Option<&ruby_prism::ParametersNode<'_>>) -> Vec<String> {
    use ruby_prism::Visit;
    struct Names(Vec<String>);
    impl Names {
        fn add(&mut self, name: Option<ruby_prism::ConstantId<'_>>) {
            if let Some(n) = name {
                self.0.push(constant_string(n.as_slice()));
            }
        }
    }
    impl<'pr> Visit<'pr> for Names {
        fn visit_required_parameter_node(&mut self, n: &ruby_prism::RequiredParameterNode<'pr>) {
            self.add(Some(n.name()));
        }
        fn visit_optional_parameter_node(&mut self, n: &ruby_prism::OptionalParameterNode<'pr>) {
            self.add(Some(n.name()));
            ruby_prism::visit_optional_parameter_node(self, n);
        }
        fn visit_rest_parameter_node(&mut self, n: &ruby_prism::RestParameterNode<'pr>) {
            self.add(n.name());
        }
        fn visit_required_keyword_parameter_node(
            &mut self,
            n: &ruby_prism::RequiredKeywordParameterNode<'pr>,
        ) {
            self.add(Some(n.name()));
        }
        fn visit_optional_keyword_parameter_node(
            &mut self,
            n: &ruby_prism::OptionalKeywordParameterNode<'pr>,
        ) {
            self.add(Some(n.name()));
            ruby_prism::visit_optional_keyword_parameter_node(self, n);
        }
        fn visit_keyword_rest_parameter_node(
            &mut self,
            n: &ruby_prism::KeywordRestParameterNode<'pr>,
        ) {
            self.add(n.name());
        }
        fn visit_block_parameter_node(&mut self, n: &ruby_prism::BlockParameterNode<'pr>) {
            self.add(n.name());
        }
    }
    let Some(p) = params else { return Vec::new() };
    let mut names = Names(Vec::new());
    names.visit_parameters_node(p);
    names.0
}

/// Whether a Prism `def` body contains an explicit `return` statement ANYWHERE
/// (ADR-0023 tier-4b decline gate). We only infer a return type from the body's
/// TAIL expression; an explicit `return` could carry a different type on another
/// path (the reference unions explicit returns + the tail — we take only the
/// tail), so the presence of ANY `return` makes us decline. A `ReturnVisitor`
/// walks the whole subtree (the default `Visit` recursion) and trips on the
/// first `ReturnNode`. A return nested inside a block/lambda/inner def also trips
/// it — conservatively safe (decline is never a false positive).
pub(crate) fn body_has_explicit_return(body: &PrismNode<'_>) -> bool {
    use ruby_prism::Visit;
    struct ReturnVisitor {
        found: bool,
    }
    impl<'pr> Visit<'pr> for ReturnVisitor {
        fn visit_return_node(&mut self, _node: &ruby_prism::ReturnNode<'pr>) {
            self.found = true;
            // No need to recurse further once found.
        }
    }
    let mut v = ReturnVisitor { found: false };
    v.visit(body);
    v.found
}
