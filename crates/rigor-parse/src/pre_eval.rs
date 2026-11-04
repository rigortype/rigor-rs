//! ADR-17 — the syntax half of the `pre_eval:` pre-pass: the port of the
//! reference's `Inference::ProjectPatchedScanner` walk
//! (`project_patched_scanner.rb`) together with its three helpers,
//! `DefinednessGuard` (`project_patched_definedness_guard.rb`),
//! `Visibility` (`project_patched_visibility.rb`) and `Mixins`
//! (`project_patched_mixins.rb`).
//!
//! [`scan_pre_eval`] reads ONE `pre_eval:` file and returns, in source order,
//! the declarations the reference's walk collects from it: every `def` /
//! `def self.` in a class, module or `class << self` body, every
//! `alias` / `alias_method` there, every named visibility call
//! (`private :x`) and every `include` / `prepend` edge. Resolving them across
//! the configured files (aliases, visibility changes, last-declaration-wins,
//! mixin names) is `rigor-infer`'s `PreEvalPatches`, which mirrors the
//! scanner's `resolve_aliases` and the registry.
//!
//! The walk is a faithful port, so a declaration is read exactly where the
//! reference reads one. Every consumer of the result only DECLINES, so a
//! declaration recorded here that the reference would not record costs
//! coverage, never a diagnostic; the one reading that can ADD a diagnostic is
//! a non-public visibility (a private patch does not answer an explicit
//! receiver), and that one is read only where the reference reads it.

use std::collections::{HashMap, HashSet};

use crate::ast::{constant_path_string, rooted_constant_path, strict_constant_path_string};
use crate::ruby_prism::{self, Node as PrismNode, Visit};

/// The visibility a `pre_eval:` declaration leaves its method with
/// (`Entry#visibility`). `ModuleFunction` is the transient reading of a
/// `module_function` toggle; it never survives [`applied`] into a recorded
/// declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PatchVisibility {
    Public,
    Private,
    Protected,
    ModuleFunction,
}

/// One item of a `pre_eval:` file, in the order the reference's walk meets
/// it. Class names are the qualified lexical prefix the walk is in
/// (`"String"`, `"Outer::String"`), as written, with no constant resolution
/// — exactly the reference's `qualified_prefix.join("::")`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreEvalItem {
    /// A `def` (`Entry`). `guarded` marks the reference's `GuardedDef`: a
    /// `def foo` under a definedness guard naming `foo`, which binds only when
    /// nothing bound the name before it.
    Def {
        class_name: String,
        method: String,
        singleton: bool,
        visibility: PatchVisibility,
        guarded: bool,
        line: usize,
    },
    /// `alias new old` / `alias_method :new, :old` (`PendingAlias`).
    Alias { class_name: String, new_name: String, old_name: String, singleton: bool, line: usize },
    /// A named visibility call (`private :x`, `module_function :x`,
    /// `private_class_method :x`) — `Visibility::Pending`.
    VisibilityCall { class_name: String, names: Vec<String>, singleton: bool, visibility: PatchVisibility },
    /// An `include` / `prepend` edge (`Mixins::Pending`): `owner` gains
    /// `module_name` (as written; `prefix` is the lexical prefix the
    /// resolution reads).
    Mixin { owner: String, module_name: String, prepend: bool, prefix: Vec<String> },
}

/// Why a `pre_eval:` file yielded no items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreEvalScanError {
    /// Prism reported a syntax error. The reference skips the file with a
    /// `pre-eval.parse-error` warning (`scan_file`), so its declarations
    /// patch nothing there either.
    Syntax,
    /// The tree nests deeper than the analysis bound (rigor-rs#412). The
    /// reference reads such a file, so the port cannot know what it patches.
    TooDeep,
}

/// The reference's `Visibility::INSTANCE_CALLS` / `SINGLETON_CALLS`.
fn visibility_call(node: &ruby_prism::CallNode<'_>) -> Option<(PatchVisibility, bool)> {
    if let Some(r) = node.receiver() {
        r.as_self_node()?;
    }
    let vis = match node.name().as_slice() {
        b"private" => (PatchVisibility::Private, false),
        b"protected" => (PatchVisibility::Protected, false),
        b"public" => (PatchVisibility::Public, false),
        b"module_function" => (PatchVisibility::ModuleFunction, false),
        b"private_class_method" => (PatchVisibility::Private, true),
        b"public_class_method" => (PatchVisibility::Public, true),
        _ => return None,
    };
    Some(vis)
}

/// `ModuleFunctionState.bare?`: no arguments.
fn bare(node: &ruby_prism::CallNode<'_>) -> bool {
    node.arguments().is_none_or(|a| a.arguments().iter().next().is_none())
}

fn span_key(n: &PrismNode<'_>) -> (usize, usize) {
    let l = n.location();
    (l.start_offset(), l.end_offset())
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `Source::Literals.symbol_or_string` / `AliasNames.literal_name`.
fn symbol_or_string(node: &PrismNode<'_>) -> Option<String> {
    if let Some(s) = node.as_symbol_node() {
        return Some(text(s.unescaped()));
    }
    node.as_string_node().map(|s| text(s.unescaped()))
}

/// What `Visibility.defs` records per node: a def's non-public visibility, or
/// that a call is a direct statement (`STATEMENT`).
#[derive(Default)]
struct VisibilityTable {
    defs: HashMap<(usize, usize), PatchVisibility>,
    statements: HashSet<(usize, usize)>,
}

/// `Visibility.read_body` and friends.
fn read_body(body: Option<PrismNode<'_>>, table: &mut VisibilityTable, top_level: bool) {
    let stmts: Vec<PrismNode<'_>> = match body {
        Some(b) => {
            let b = match b.as_begin_node() {
                Some(begin) => begin.statements().map(|s| s.as_node()),
                None => Some(b),
            };
            match b.and_then(|b| b.as_statements_node()) {
                Some(s) => s.body().iter().collect(),
                None => Vec::new(),
            }
        }
        None => Vec::new(),
    };
    let mut default = PatchVisibility::Public;
    for stmt in stmts {
        let call = stmt.as_call_node();
        if call.is_some() {
            table.statements.insert(span_key(&stmt));
        }
        let vis = call.as_ref().and_then(visibility_call);
        if let (Some((v, singleton)), Some(c)) = (vis, call.as_ref()) {
            if bare(c) {
                if !singleton {
                    default = v;
                }
            } else {
                // `mark_arguments`.
                if let Some(args) = c.arguments() {
                    for arg in args.arguments().iter() {
                        if let Some(d) = arg.as_def_node() {
                            let self_recv = d.receiver().is_some_and(|r| r.as_self_node().is_some());
                            if singleton == self_recv {
                                mark(&arg, v, table);
                            }
                        }
                    }
                }
            }
        } else if let Some(d) = stmt.as_def_node() {
            if d.receiver().is_none() {
                mark(&stmt, default, table);
            }
        } else if let Some(c) = stmt.as_class_node() {
            read_body(c.body(), table, false);
        } else if let Some(m) = stmt.as_module_node() {
            read_body(m.body(), table, false);
        } else if let Some(s) = stmt.as_singleton_class_node() {
            read_body(s.body(), table, false);
        } else {
            if let Some(guarded) = guarded_mixin(&stmt, top_level) {
                table.statements.insert(guarded);
            }
            if bare_call_nested(&stmt) {
                default = PatchVisibility::Public;
            }
            let mut m = MarkNested { default, table };
            m.visit(&stmt);
        }
    }
}

fn mark(def: &PrismNode<'_>, visibility: PatchVisibility, table: &mut VisibilityTable) {
    if visibility != PatchVisibility::Public {
        table.defs.insert(span_key(def), visibility);
    }
}

/// `Visibility.mark_nested`: a `def` nested in a statement takes the body's
/// default, except across another body, a block or a lambda.
struct MarkNested<'a> {
    default: PatchVisibility,
    table: &'a mut VisibilityTable,
}

impl<'pr> Visit<'pr> for MarkNested<'_> {
    fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
        if node.receiver().is_none() {
            mark(&node.as_node(), self.default, self.table);
        }
    }
    fn visit_class_node(&mut self, _: &ruby_prism::ClassNode<'pr>) {}
    fn visit_module_node(&mut self, _: &ruby_prism::ModuleNode<'pr>) {}
    fn visit_singleton_class_node(&mut self, _: &ruby_prism::SingletonClassNode<'pr>) {}
    fn visit_block_node(&mut self, _: &ruby_prism::BlockNode<'pr>) {}
    fn visit_lambda_node(&mut self, _: &ruby_prism::LambdaNode<'pr>) {}
}

/// `Visibility.bare_call_nested?`.
fn bare_call_nested(stmt: &PrismNode<'_>) -> bool {
    struct Find {
        found: bool,
    }
    impl<'pr> Visit<'pr> for Find {
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            if visibility_call(node).is_some() && bare(node) {
                self.found = true;
                return;
            }
            ruby_prism::visit_call_node(self, node);
        }
        fn visit_def_node(&mut self, _: &ruby_prism::DefNode<'pr>) {}
        fn visit_class_node(&mut self, _: &ruby_prism::ClassNode<'pr>) {}
        fn visit_module_node(&mut self, _: &ruby_prism::ModuleNode<'pr>) {}
        fn visit_singleton_class_node(&mut self, _: &ruby_prism::SingletonClassNode<'pr>) {}
        fn visit_block_node(&mut self, _: &ruby_prism::BlockNode<'pr>) {}
        fn visit_lambda_node(&mut self, _: &ruby_prism::LambdaNode<'pr>) {}
    }
    let mut f = Find { found: false };
    f.visit(stmt);
    f.found
}

/// Unwraps a one-statement parenthesis (`Mixins.unwrap`).
fn unwrap_parens<'pr>(node: PrismNode<'pr>) -> PrismNode<'pr> {
    if let Some(p) = node.as_parentheses_node() {
        if let Some(stmts) = p.body().and_then(|b| b.as_statements_node()) {
            let body: Vec<PrismNode<'pr>> = stmts.body().iter().collect();
            if body.len() == 1 {
                return body.into_iter().next().expect("one statement");
            }
        }
    }
    node
}

/// `Mixins.guarded`: the `include` / `prepend` call an idempotence guard runs
/// (`X.include(M) unless X.include?(M)`), as its span.
fn guarded_mixin(node: &PrismNode<'_>, top_level: bool) -> Option<(usize, usize)> {
    let (body, guard) = if let Some(u) = node.as_unless_node() {
        if u.else_clause().is_some() {
            return None;
        }
        (u.statements(), Some(unwrap_parens(u.predicate())))
    } else if let Some(i) = node.as_if_node() {
        if i.subsequent().is_some() {
            return None;
        }
        let pred = unwrap_parens(i.predicate());
        let guard = pred
            .as_call_node()
            .filter(|c| c.name().as_slice() == b"!")
            .and_then(|c| c.receiver())
            .map(unwrap_parens);
        (i.statements(), guard)
    } else {
        return None;
    };
    let body: Vec<PrismNode<'_>> = body.map(|s| s.body().iter().collect()).unwrap_or_default();
    if body.len() != 1 {
        return None;
    }
    let call = body[0].as_call_node()?;
    let name = call.name().as_slice();
    if name != b"include" && name != b"prepend" {
        return None;
    }
    let args: Vec<PrismNode<'_>> = call.arguments()?.arguments().iter().collect();
    if args.len() != 1 || (top_level && call.receiver().is_none()) {
        return None;
    }
    let guard = guard?.as_call_node()?;
    let gargs: Vec<PrismNode<'_>> = guard.arguments()?.arguments().iter().collect();
    if gargs.len() != 1 {
        return None;
    }
    let subject = match guard.name().as_slice() {
        b"<" => Some(guard.receiver()?),
        b"include?" => match guard.receiver() {
            Some(r) => match r.as_call_node() {
                Some(rc) if rc.name().as_slice() == b"ancestors" && rc.arguments().is_none() => rc.receiver(),
                _ => Some(r),
            },
            None => None,
        },
        _ => return None,
    };
    let same = |left: Option<&PrismNode<'_>>, right: Option<&PrismNode<'_>>| match (left, right) {
        (None, None) => true,
        (Some(l), Some(r)) => {
            let ln = strict_constant_path_string(l);
            ln.is_some() && ln == strict_constant_path_string(r)
        }
        _ => false,
    };
    let receiver = call.receiver();
    (same(subject.as_ref(), receiver.as_ref()) && same(Some(&gargs[0]), Some(&args[0])))
        .then(|| span_key(&body[0]))
}

/// `DefinednessGuard.defs`: the `def`s on the "name not yet defined" side of a
/// definedness guard naming them.
struct GuardWalk {
    found: HashSet<(usize, usize)>,
}

const DEFINEDNESS_PREDICATES: &[&[u8]] = &[
    b"method_defined?",
    b"private_method_defined?",
    b"public_method_defined?",
    b"protected_method_defined?",
    b"respond_to?",
];
const INSTANCE_METHOD_LISTS: &[&[u8]] = &[
    b"instance_methods",
    b"public_instance_methods",
    b"private_instance_methods",
    b"protected_instance_methods",
];

/// `definedness_guard`: `(method name, negated)`.
fn definedness_guard(predicate: PrismNode<'_>) -> Option<(String, bool)> {
    let mut predicate = predicate;
    if let Some(p) = predicate.as_parentheses_node() {
        if let Some(b) = p.body() {
            predicate = b;
        }
    }
    if let Some(s) = predicate.as_statements_node() {
        let body: Vec<PrismNode<'_>> = s.body().iter().collect();
        if body.len() == 1 {
            predicate = body.into_iter().next().expect("one");
        }
    }
    if let Some(c) = predicate.as_call_node() {
        if c.name().as_slice() == b"!" {
            if let Some(r) = c.receiver() {
                let (name, negated) = definedness_guard(r)?;
                return Some((name, !negated));
            }
        }
    }
    let call = predicate.as_call_node()?;
    let name = call.name().as_slice();
    let listed = (name == b"include?" || name == b"member?")
        && call
            .receiver()
            .and_then(|r| r.as_call_node())
            .is_some_and(|r| INSTANCE_METHOD_LISTS.contains(&r.name().as_slice()));
    if !listed && !DEFINEDNESS_PREDICATES.contains(&name) {
        return None;
    }
    let arg = call.arguments()?.arguments().iter().next()?;
    let method = symbol_or_string(&arg)?;
    Some((method, false))
}

struct MarkGuarded<'a> {
    name: &'a str,
    found: &'a mut HashSet<(usize, usize)>,
}

impl<'pr> Visit<'pr> for MarkGuarded<'_> {
    fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
        if node.name().as_slice() == self.name.as_bytes() {
            self.found.insert(span_key(&node.as_node()));
        }
    }
}

impl GuardWalk {
    fn guard(&mut self, predicate: PrismNode<'_>, when_true: Option<PrismNode<'_>>, when_false: Option<PrismNode<'_>>) {
        if let Some((name, negated)) = definedness_guard(predicate) {
            let branch = if negated { when_true } else { when_false };
            if let Some(b) = branch {
                let mut m = MarkGuarded { name: &name, found: &mut self.found };
                m.visit(&b);
            }
        }
    }
}

impl<'pr> Visit<'pr> for GuardWalk {
    fn visit_if_node(&mut self, node: &ruby_prism::IfNode<'pr>) {
        self.guard(node.predicate(), node.statements().map(|s| s.as_node()), node.subsequent());
        ruby_prism::visit_if_node(self, node);
    }
    fn visit_unless_node(&mut self, node: &ruby_prism::UnlessNode<'pr>) {
        self.guard(node.predicate(), node.else_clause().map(|e| e.as_node()), node.statements().map(|s| s.as_node()));
        ruby_prism::visit_unless_node(self, node);
    }
}

/// The scanner's walk (`walk_node`).
struct Scan<'a> {
    prefix: Vec<String>,
    in_singleton: bool,
    vis: &'a VisibilityTable,
    guarded: &'a HashSet<(usize, usize)>,
    line_starts: Vec<usize>,
    items: Vec<PreEvalItem>,
}

impl Scan<'_> {
    fn line(&self, offset: usize) -> usize {
        self.line_starts.partition_point(|&s| s <= offset)
    }

    fn class_name(&self) -> Option<String> {
        (!self.prefix.is_empty()).then(|| self.prefix.join("::"))
    }

    /// `descend_class_or_module`.
    fn descend<'pr>(&mut self, node: &PrismNode<'pr>, path: PrismNode<'pr>, body: Option<PrismNode<'pr>>) {
        match (strict_constant_path_string(&path), body) {
            (Some(_), Some(body)) => {
                let name = constant_path_string(&path);
                let child = if rooted_constant_path(&path) {
                    vec![name]
                } else {
                    let mut p = self.prefix.clone();
                    p.push(name);
                    p
                };
                let saved = std::mem::replace(&mut self.prefix, child);
                self.visit(&body);
                self.prefix = saved;
            }
            _ => self.visit_children(node),
        }
    }

    fn visit_children(&mut self, node: &PrismNode<'_>) {
        if let Some(c) = node.as_class_node() {
            ruby_prism::visit_class_node(self, &c);
        } else if let Some(m) = node.as_module_node() {
            ruby_prism::visit_module_node(self, &m);
        } else if let Some(s) = node.as_singleton_class_node() {
            ruby_prism::visit_singleton_class_node(self, &s);
        }
    }

    /// `record_alias`.
    fn record_alias(&mut self, new_name: String, old_name: String, offset: usize) {
        let Some(class_name) = self.class_name() else { return };
        let line = self.line(offset);
        self.items.push(PreEvalItem::Alias {
            class_name,
            new_name,
            old_name,
            singleton: self.in_singleton,
            line,
        });
    }
}

impl<'pr> Visit<'pr> for Scan<'_> {
    fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
        self.descend(&node.as_node(), node.constant_path(), node.body());
    }

    fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
        self.descend(&node.as_node(), node.constant_path(), node.body());
    }

    fn visit_singleton_class_node(&mut self, node: &ruby_prism::SingletonClassNode<'pr>) {
        match (node.expression().as_self_node(), node.body()) {
            (Some(_), Some(body)) => {
                let saved = std::mem::replace(&mut self.in_singleton, true);
                self.visit(&body);
                self.in_singleton = saved;
            }
            _ => {
                let saved = std::mem::replace(&mut self.in_singleton, false);
                ruby_prism::visit_singleton_class_node(self, node);
                self.in_singleton = saved;
            }
        }
    }

    /// `record_def_node`. The walk does not descend into a `def` body.
    fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
        let Some(class_name) = self.class_name() else { return };
        let n = node.as_node();
        let key = span_key(&n);
        let singleton = node.receiver().is_some_and(|r| r.as_self_node().is_some()) || self.in_singleton;
        let method = text(node.name().as_slice());
        let line = self.line(key.0);
        let visibility = self.vis.defs.get(&key).copied().unwrap_or(PatchVisibility::Public);
        let guarded = self.guarded.contains(&key);
        // `Visibility.applied`: `module_function` makes the instance method
        // private and binds a public singleton copy.
        if visibility == PatchVisibility::ModuleFunction {
            self.items.push(PreEvalItem::Def {
                class_name: class_name.clone(),
                method: method.clone(),
                singleton,
                visibility: PatchVisibility::Private,
                guarded,
                line,
            });
            if !singleton {
                self.items.push(PreEvalItem::Def {
                    class_name,
                    method,
                    singleton: true,
                    visibility: PatchVisibility::Public,
                    guarded: false,
                    line,
                });
            }
        } else {
            self.items.push(PreEvalItem::Def { class_name, method, singleton, visibility, guarded, line });
        }
    }

    fn visit_alias_method_node(&mut self, node: &ruby_prism::AliasMethodNode<'pr>) {
        let sym = |n: PrismNode<'_>| n.as_symbol_node().map(|s| text(s.unescaped()));
        if let (Some(new_name), Some(old_name)) = (sym(node.new_name()), sym(node.old_name())) {
            self.record_alias(new_name, old_name, node.location().start_offset());
        }
    }

    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        // `AliasNames.alias_method_call_names`.
        if node.name().as_slice() == b"alias_method" && node.receiver().is_none() {
            if let Some(args) = node.arguments() {
                let args: Vec<PrismNode<'_>> = args.arguments().iter().collect();
                if args.len() == 2 {
                    if let (Some(new_name), Some(old_name)) =
                        (symbol_or_string(&args[0]), symbol_or_string(&args[1]))
                    {
                        self.record_alias(new_name, old_name, node.location().start_offset());
                    }
                }
            }
        }
        if self.vis.statements.contains(&span_key(&node.as_node())) {
            self.pending_visibility(node);
            self.pending_mixins(node);
        }
        ruby_prism::visit_call_node(self, node);
    }
}

impl Scan<'_> {
    /// `Visibility.pending`.
    fn pending_visibility(&mut self, node: &ruby_prism::CallNode<'_>) {
        let Some(class_name) = self.class_name() else { return };
        let Some((visibility, side)) = visibility_call(node) else { return };
        let Some(args) = node.arguments() else { return };
        let names: Vec<String> = args.arguments().iter().filter_map(|a| symbol_or_string(&a)).collect();
        if names.is_empty() {
            return;
        }
        self.items.push(PreEvalItem::VisibilityCall {
            class_name,
            names,
            singleton: side || self.in_singleton,
            visibility,
        });
    }

    /// `Mixins.pending`.
    fn pending_mixins(&mut self, node: &ruby_prism::CallNode<'_>) {
        let name = node.name().as_slice();
        let prepend = match name {
            b"include" => false,
            b"prepend" => true,
            _ => return,
        };
        let Some(args) = node.arguments() else { return };
        let owner = match node.receiver() {
            None => {
                if self.in_singleton {
                    return;
                }
                if self.prefix.is_empty() {
                    "Object".to_string()
                } else {
                    self.prefix.join("::")
                }
            }
            Some(r) if r.as_self_node().is_some() => {
                if self.in_singleton || self.prefix.is_empty() {
                    return;
                }
                self.prefix.join("::")
            }
            Some(r) => match strict_constant_path_string(&r) {
                Some(n) => n,
                None => return,
            },
        };
        for arg in args.arguments().iter() {
            let Some(module_name) = strict_constant_path_string(&arg) else { continue };
            let rooted = arg.as_constant_path_node().is_some_and(|cp| cp.parent().is_none());
            let prefix = if rooted { Vec::new() } else { self.prefix.clone() };
            self.items.push(PreEvalItem::Mixin { owner: owner.clone(), module_name, prepend, prefix });
        }
    }
}

/// Scan one `pre_eval:` file (`ProjectPatchedScanner.scan_file`).
pub fn scan_pre_eval(source: &[u8]) -> Result<Vec<PreEvalItem>, PreEvalScanError> {
    let result = match crate::depth::parse_bounded(source) {
        Ok(r) => r,
        Err(_) => return Err(PreEvalScanError::TooDeep),
    };
    if result.errors().next().is_some() {
        return Err(PreEvalScanError::Syntax);
    }
    let root = result.node();
    let mut table = VisibilityTable::default();
    if let Some(program) = root.as_program_node() {
        read_body(Some(program.statements().as_node()), &mut table, true);
    }
    let mut guards = GuardWalk { found: HashSet::new() };
    guards.visit(&root);
    let mut line_starts = vec![0];
    for (i, &b) in source.iter().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let mut scan = Scan {
        prefix: Vec::new(),
        in_singleton: false,
        vis: &table,
        guarded: &guards.found,
        line_starts,
        items: Vec::new(),
    };
    scan.visit(&root);
    Ok(scan.items)
}

#[cfg(test)]
mod tests;
