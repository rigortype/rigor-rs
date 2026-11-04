//! Upstream v0.4.5 (#2086 / issue #1722, `LexicalMethodSites`) — the locals
//! whose ONE object this file gives singleton methods, keyed by the local
//! scope the opening sits in.
//!
//! The reference's `singleton_local_def?` declines `call.undefined-method` on
//! `o.m` when, in the scope the call is in (the enclosing `def`, `class` /
//! `module` / `class << x` body, or the file — a block shares its enclosing
//! scope's locals; a scope nested inside does not see the outer `o`):
//!
//! * `def o.m` is written (that name only);
//! * `o.extend(M)`, any call on `o.singleton_class`, or `class << o` (any name);
//! * `o.define_singleton_method(:m)` (the literal name, else any name);
//! * `o.instance_eval` / `o.instance_exec` given a String, or whose block
//!   writes a `def` or calls `extend` / `define_singleton_method` on its
//!   `self` (any name);
//! * or `o = s.clone` where `s` holds such an opening (four links at most;
//!   `dup` does not copy the singleton class).
//!
//! Where in the scope the opening sits, and whether it runs, are not read.

use crate::ruby_prism::{self, Node as PrismNode, Visit};

use super::constant_string;

type Span = (usize, usize);

/// How many `t = s.clone` links a lookup follows (`CLONE_DEPTH`).
const CLONE_DEPTH: usize = 4;

/// See the module docs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SingletonLocals {
    /// `(local, method — None for any name, scope span)`.
    opened: Vec<(String, Option<String>, Span)>,
    /// `(target, source, scope span)` for `target = source.clone`.
    clones: Vec<(String, String, Span)>,
    /// Every local scope a class / module / def / `class << x` opens.
    scopes: Vec<Span>,
}

impl SingletonLocals {
    /// Whether a call of `method` at byte `offset` on the local `local` may
    /// reach a singleton method the file gives that local's object.
    pub fn holds(&self, local: &str, method: &str, offset: usize) -> bool {
        self.holds_at(local, method, offset, CLONE_DEPTH)
    }

    fn holds_at(&self, local: &str, method: &str, offset: usize, depth: usize) -> bool {
        let opened = self.opened.iter().any(|(l, m, span)| {
            l == local && m.as_deref().is_none_or(|m| m == method) && self.in_scope(offset, *span)
        });
        if opened {
            return true;
        }
        if depth == 0 {
            return false;
        }
        self.clones.iter().any(|(target, source, span)| {
            target == local
                && self.in_scope(offset, *span)
                && self.holds_at(source, method, offset, depth - 1)
        })
    }

    fn in_scope(&self, offset: usize, (start, stop): Span) -> bool {
        offset >= start && offset < stop && !self.nested_scope_holds(offset, start, stop)
    }

    /// Is `offset` inside a local scope nested in the one `[start, stop)` spans?
    fn nested_scope_holds(&self, offset: usize, start: usize, stop: usize) -> bool {
        self.scopes.iter().any(|&(s, e)| {
            (s > start || e < stop) && s >= start && e <= stop && offset >= s && offset < e
        })
    }
}

fn span_of(n: &PrismNode<'_>) -> Span {
    let l = n.location();
    (l.start_offset(), l.end_offset())
}

fn local_name(n: &PrismNode<'_>) -> Option<String> {
    n.as_local_variable_read_node().map(|lv| constant_string(lv.name().as_slice()))
}

struct Walk {
    out: SingletonLocals,
    locals: Span,
}

impl Walk {
    fn scoped(&mut self, span: Span, body: Option<PrismNode<'_>>) {
        self.out.scopes.push(span);
        let saved = std::mem::replace(&mut self.locals, span);
        if let Some(b) = body {
            self.visit(&b);
        }
        self.locals = saved;
    }

    fn open(&mut self, n: &PrismNode<'_>, method: Option<String>) {
        if let Some(local) = local_name(n) {
            self.out.opened.push((local, method, self.locals));
        }
    }

    /// `opened_method_name`: `Some(None)` for any name, `Some(Some(m))` for a
    /// literal name, `None` when the call defines nothing.
    fn opened_method_name(call: &ruby_prism::CallNode<'_>) -> Option<Option<String>> {
        let args: Vec<PrismNode<'_>> =
            call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        match constant_string(call.name().as_slice()).as_str() {
            "extend" => Some(None),
            "define_singleton_method" => Some(args.first().and_then(literal_name)),
            "instance_eval" | "instance_exec" => eval_defines(call, &args).then_some(None),
            _ => None,
        }
    }
}

fn literal_name(n: &PrismNode<'_>) -> Option<String> {
    if let Some(s) = n.as_symbol_node() {
        return Some(String::from_utf8_lossy(s.unescaped()).into_owned());
    }
    n.as_string_node().map(|s| String::from_utf8_lossy(s.unescaped()).into_owned())
}

/// An `instance_eval` given code (any argument), or whose block writes a
/// `def` or calls `extend` / `define_singleton_method` on its `self`.
fn eval_defines(call: &ruby_prism::CallNode<'_>, args: &[PrismNode<'_>]) -> bool {
    if call.arguments().is_some() && !args.is_empty() {
        return true;
    }
    let Some(block) = call.block().and_then(|b| b.as_block_node()) else {
        return false;
    };
    block.body().is_some_and(|b| defines_on_self(&b))
}

fn defines_on_self(n: &PrismNode<'_>) -> bool {
    struct Finder {
        found: bool,
    }
    impl<'pr> Visit<'pr> for Finder {
        fn visit_def_node(&mut self, _node: &ruby_prism::DefNode<'pr>) {
            self.found = true;
        }
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            let name = constant_string(node.name().as_slice());
            if (name == "extend" || name == "define_singleton_method")
                && node.receiver().is_none_or(|r| r.as_self_node().is_some())
            {
                self.found = true;
                return;
            }
            ruby_prism::visit_call_node(self, node);
        }
    }
    let mut f = Finder { found: false };
    f.visit(n);
    f.found
}

impl<'pr> Visit<'pr> for Walk {
    fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
        self.scoped(span_of(&node.as_node()), node.body());
    }

    fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
        self.scoped(span_of(&node.as_node()), node.body());
    }

    fn visit_singleton_class_node(&mut self, node: &ruby_prism::SingletonClassNode<'pr>) {
        let expr = node.expression();
        self.open(&expr, None);
        self.visit(&expr);
        self.scoped(span_of(&node.as_node()), node.body());
    }

    fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
        if let Some(r) = node.receiver() {
            self.open(&r, Some(constant_string(node.name().as_slice())));
        }
        self.scoped(span_of(&node.as_node()), node.body());
    }

    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        if let Some(recv) = node.receiver() {
            let sc_local = recv.as_call_node().and_then(|sc| {
                (constant_string(sc.name().as_slice()) == "singleton_class"
                    && sc.arguments().is_none())
                .then(|| sc.receiver())
                .flatten()
            });
            if let Some(inner) = sc_local {
                self.open(&inner, None);
            } else if local_name(&recv).is_some() {
                if let Some(m) = Self::opened_method_name(node) {
                    self.open(&recv, m);
                }
            }
        }
        ruby_prism::visit_call_node(self, node);
    }

    fn visit_local_variable_write_node(&mut self, node: &ruby_prism::LocalVariableWriteNode<'pr>) {
        if let Some(call) = node.value().as_call_node() {
            if constant_string(call.name().as_slice()) == "clone" {
                if let Some(source) = call.receiver().as_ref().and_then(local_name) {
                    let target = constant_string(node.name().as_slice());
                    self.out.clones.push((target, source, self.locals));
                }
            }
        }
        ruby_prism::visit_local_variable_write_node(self, node);
    }
}

/// Collect the [`SingletonLocals`] of one file.
pub(crate) fn collect(root: &PrismNode<'_>) -> SingletonLocals {
    let span = span_of(root);
    let mut w = Walk { out: SingletonLocals::default(), locals: span };
    w.visit(root);
    w.out
}
