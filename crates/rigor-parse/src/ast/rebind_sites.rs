//! Upstream v0.4.6 (rigortype/rigor#1817 / #2183 / #2184 / #2186) — the
//! per-file facts `call.unresolved-toplevel` reads to decline a call inside a
//! block that some callee may run under another `self`
//! (`check_rules/rebinding_block_callees.rb` + `…/dispatch.rb`).
//!
//! Two halves, both computed from the Prism tree:
//!
//! * the file's block SITES — the reference's `RebindingBlockCallees::Sites`:
//!   one row per literal block / `&`-passed proc literal body (plus one per
//!   heredoc body that opens inside it), each naming the call that receives
//!   the block (its dispatched name and receiver) or [`RebindRow::site`]
//!   `None`, the reference's `Sites::UNKNOWN`;
//! * the file's DEFINITION census — every `def` of a name with the slot the
//!   reference's def tables would key it under (or none, when the tables
//!   cannot hold it) and whether its body may hand the block on
//!   (`escapes?`), plus every name a bodiless macro defines (`attr_*`,
//!   `alias_method`, `define_method`, …). The project-wide join is the
//!   source index's (`SourceIndex::block_callee_may_rebind`).
//!
//! Everything here can only make the rule DECLINE. Where the port cannot
//! say what the reference's tables would hold, it records the definition as
//! one the tables cannot read, which declines.

use std::collections::{HashMap, HashSet};

use crate::ruby_prism::{self, Node as PrismNode, Visit};

use super::constant_string;

/// The receiver of a block call as the rule needs it to ask whether the
/// callee resolves on it (the reference's `Dispatch.resolves_on_receiver?`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RebindReceiver {
    /// A receiverless call (`:implicit`).
    Implicit,
    /// A receiver Rigor cannot say (`instance_method(:x).bind_call`).
    Unsayable,
    /// A literal whose class is fixed: `"Array"`, `"String"`, ….
    Literal(&'static str),
    /// A bare constant read (`Foo`, `::Foo`).
    Constant(String),
    /// Any other expression; `span` is its byte range.
    Other { span: (usize, usize) },
}

/// One block-passing call: the name it dispatches to (`None` when a `send`
/// names its method with anything but a valid literal) and its receiver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebindSite {
    pub name: Option<String>,
    pub receiver: RebindReceiver,
}

/// `[from, to)` of a block body (or heredoc body) and the site whose block it
/// is; `None` is the reference's `Sites::UNKNOWN`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebindRow {
    pub from: usize,
    pub to: usize,
    pub site: Option<u32>,
}

/// One `def` of `name`. `slot` is `(owner's last constant segment or
/// "<toplevel>", singleton side)` when the def is one the reference's def
/// tables hold; `None` when it is not (a `def` inside a block or method
/// body, under a conditional, after a bare `module_function`, on another
/// receiver).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebindDef {
    pub name: String,
    pub slot: Option<(String, bool)>,
    pub escapes: bool,
}

/// See the module docs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RebindFacts {
    /// Sorted by `from`.
    pub rows: Vec<RebindRow>,
    pub sites: Vec<RebindSite>,
    pub defs: Vec<RebindDef>,
    /// Names a bodiless macro defines.
    pub bodiless: Vec<String>,
}

impl RebindFacts {
    /// The sites (or `None` = UNKNOWN) whose block contains `[start, stop)`.
    pub fn enclosing(&self, start: usize, stop: usize) -> Vec<Option<&RebindSite>> {
        let mut found = Vec::new();
        for row in &self.rows {
            if row.from > start {
                break;
            }
            if stop <= row.to {
                found.push(row.site.map(|i| &self.sites[i as usize]));
            }
        }
        found
    }
}

const SEND_CALLS: [&str; 3] = ["send", "__send__", "public_send"];
/// Reading the block through these runs it with its own `self`, or does not run it.
const PLAIN_PROC_CALLS: [&str; 9] =
    ["call", "yield", "[]", "===", "!", "arity", "lambda?", "parameters", "nil?"];
const BINDING_CALLS: [&str; 3] = ["binding", "eval", "local_variable_get"];
const STRING_EVAL_CALLS: [&str; 3] = ["instance_eval", "class_eval", "module_eval"];
/// `Dispatch::METHOD_OBJECT_CALLS`.
pub const METHOD_OBJECT_CALLS: [&str; 5] = ["call", "yield", "===", "[]", "bind_call"];
const METHOD_READERS: [&str; 5] =
    ["method", "public_method", "singleton_method", "instance_method", "public_instance_method"];
const BOUND_METHOD_READERS: [&str; 3] = ["method", "public_method", "singleton_method"];
const METHOD_WRAPPERS: [&str; 4] = ["bind", "unbind", "to_proc", "curry"];
const VISIBILITY_MODIFIERS: [&str; 3] = ["private", "protected", "public"];
/// Macros whose literal-name arguments name a method with no `def` body
/// (the reference's `record_call_node_methods` recorders, widened: an extra
/// name only declines).
const BODILESS_MACROS: [&str; 22] = [
    "attr",
    "attr_reader",
    "attr_writer",
    "attr_accessor",
    "mattr_reader",
    "mattr_writer",
    "mattr_accessor",
    "cattr_reader",
    "cattr_writer",
    "cattr_accessor",
    "class_attribute",
    "define_method",
    "define_singleton_method",
    "alias_method",
    "module_function",
    "private_class_method",
    "public_class_method",
    "delegate",
    "def_delegator",
    "def_delegators",
    "def_instance_delegator",
    "def_instance_delegators",
];

fn span_of_node(n: &PrismNode<'_>) -> (usize, usize) {
    let l = n.location();
    (l.start_offset(), l.end_offset())
}

fn name_of(call: &ruby_prism::CallNode<'_>) -> String {
    constant_string(call.name().as_slice())
}

/// `literal_name`: a Symbol / String literal's text, when valid UTF-8.
fn literal_name(node: Option<&PrismNode<'_>>) -> Option<String> {
    let node = node?;
    if let Some(s) = node.as_symbol_node() {
        return std::str::from_utf8(s.unescaped()).ok().map(str::to_string);
    }
    let s = node.as_string_node()?;
    std::str::from_utf8(s.unescaped()).ok().map(str::to_string)
}

fn first_arg<'pr>(call: &ruby_prism::CallNode<'pr>) -> Option<PrismNode<'pr>> {
    call.arguments().and_then(|a| a.arguments().iter().next())
}

/// `send_target`: the call's own name, or the literal name a `send` family
/// call dispatches to (`None` when it is not one).
fn send_target(call: &ruby_prism::CallNode<'_>) -> Option<String> {
    let name = name_of(call);
    if !SEND_CALLS.contains(&name.as_str()) {
        return Some(name);
    }
    literal_name(first_arg(call).as_ref())
}

/// `proc_literal_call?`: `proc { }` / `lambda { }` / `Proc.new { }`.
fn proc_literal_call(call: &ruby_prism::CallNode<'_>) -> bool {
    let name = name_of(call);
    match call.receiver() {
        None => name == "proc" || name == "lambda",
        Some(r) => {
            name == "new"
                && r.as_constant_read_node()
                    .is_some_and(|c| constant_string(c.name().as_slice()) == "Proc")
        }
    }
}

/// `proc_literal(node)`'s body span: a lambda node itself, or the block of a
/// proc literal call.
fn proc_literal_body(node: &PrismNode<'_>) -> Option<(usize, usize)> {
    if node.as_lambda_node().is_some() {
        return Some(span_of_node(node));
    }
    let call = node.as_call_node()?;
    let block = call.block()?;
    if block.as_block_node().is_some() && proc_literal_call(&call) {
        Some(span_of_node(&block))
    } else {
        None
    }
}

fn receiver_of(node: Option<PrismNode<'_>>) -> RebindReceiver {
    let Some(n) = node else {
        return RebindReceiver::Implicit;
    };
    if n.as_array_node().is_some() {
        RebindReceiver::Literal("Array")
    } else if n.as_hash_node().is_some() {
        RebindReceiver::Literal("Hash")
    } else if n.as_string_node().is_some() || n.as_interpolated_string_node().is_some() {
        RebindReceiver::Literal("String")
    } else if n.as_symbol_node().is_some() || n.as_interpolated_symbol_node().is_some() {
        RebindReceiver::Literal("Symbol")
    } else if n.as_integer_node().is_some() {
        RebindReceiver::Literal("Integer")
    } else if n.as_float_node().is_some() {
        RebindReceiver::Literal("Float")
    } else if n.as_nil_node().is_some() {
        RebindReceiver::Literal("NilClass")
    } else if n.as_true_node().is_some() {
        RebindReceiver::Literal("TrueClass")
    } else if n.as_false_node().is_some() {
        RebindReceiver::Literal("FalseClass")
    } else if let Some(c) = n.as_constant_read_node() {
        RebindReceiver::Constant(constant_string(c.name().as_slice()))
    } else {
        RebindReceiver::Other { span: span_of_node(&n) }
    }
}

/// `Dispatch.dispatch`.
fn dispatch(call: &ruby_prism::CallNode<'_>) -> RebindSite {
    let name = name_of(call);
    if METHOD_OBJECT_CALLS.contains(&name.as_str()) {
        let mut recv = call.receiver();
        while let Some(c) = recv.as_ref().and_then(|r| r.as_call_node()) {
            if METHOD_WRAPPERS.contains(&name_of(&c).as_str()) {
                recv = c.receiver();
            } else {
                break;
            }
        }
        if let Some(reader) = recv.as_ref().and_then(|r| r.as_call_node()) {
            let rname = name_of(&reader);
            if METHOD_READERS.contains(&rname.as_str()) {
                let receiver = if BOUND_METHOD_READERS.contains(&rname.as_str()) {
                    receiver_of(reader.receiver())
                } else {
                    RebindReceiver::Unsayable
                };
                return RebindSite { name: literal_name(first_arg(&reader).as_ref()), receiver };
            }
        }
    }
    RebindSite { name: send_target(call), receiver: receiver_of(call.receiver()) }
}

/// The node's own span key, used as identity in the parent-relative sets.
type Key = (usize, usize);

/// A proc literal that is a value: its body, how its parent reads it, and the
/// local scope it sits in.
struct ProcValue {
    body: (usize, usize),
    lvw: Option<String>,
    scope: usize,
}

/// The owner frame for the definition census.
struct Frame {
    /// The owner's last constant segment, `None` for an owner the walk
    /// cannot name (`class << obj`), `Some("<toplevel>")` at the top.
    owner: Option<String>,
    singleton: bool,
    /// A bare `module_function` was seen in this body.
    module_function: bool,
}

struct Walk {
    out: RebindFacts,
    bodies: Vec<((usize, usize), Option<u32>)>,
    heredocs: Vec<(usize, usize, usize)>,
    proc_values: Vec<ProcValue>,
    local_reads: HashMap<String, Vec<bool>>,
    block_passes: HashMap<String, Vec<u32>>,
    scopes_reading_locals: HashSet<usize>,
    /// Spans whose parent reads them plainly (`plain_read?`).
    plain: HashSet<Key>,
    /// Spans that are a `BlockArgumentNode`'s expression.
    block_arg_exprs: HashSet<Key>,
    /// Spans that are a `LocalVariableWriteNode`'s value, with the name.
    lvw_values: HashMap<Key, String>,
    /// Current local scope: the enclosing `def`'s start offset, or `usize::MAX`.
    scope: usize,
    frames: Vec<Frame>,
    /// The direct statements (program statements and statements of direct
    /// class bodies) and the def arguments of a direct visibility modifier.
    direct: HashSet<Key>,
}

impl Walk {
    fn add_site(&mut self, site: RebindSite) -> u32 {
        self.out.sites.push(site);
        (self.out.sites.len() - 1) as u32
    }

    fn mark_direct_body(&mut self, body: Option<&PrismNode<'_>>) {
        if let Some(stmts) = body.and_then(|b| b.as_statements_node()) {
            for s in stmts.body().iter() {
                self.direct.insert(span_of_node(&s));
                // `private def x` / `public def x` written as a direct statement.
                if let Some(c) = s.as_call_node() {
                    if c.receiver().is_none() && VISIBILITY_MODIFIERS.contains(&name_of(&c).as_str()) {
                        if let Some(args) = c.arguments() {
                            for a in args.arguments().iter() {
                                if a.as_def_node().is_some() {
                                    self.direct.insert(span_of_node(&a));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn declaration(&mut self, n: &PrismNode<'_>, owner: Option<String>, singleton: bool, body: Option<PrismNode<'_>>) {
        let direct = self.direct.contains(&span_of_node(n));
        if direct && owner.is_some() {
            self.mark_direct_body(body.as_ref());
        }
        self.frames.push(Frame { owner, singleton, module_function: false });
        if let Some(b) = &body {
            self.visit(b);
        }
        self.frames.pop();
    }

    fn note_value(&mut self, literal: Key, body: (usize, usize)) {
        if self.block_arg_exprs.contains(&literal) || self.plain.contains(&literal) {
            return;
        }
        let lvw = self.lvw_values.get(&literal).cloned();
        self.proc_values.push(ProcValue { body, lvw, scope: self.scope });
    }

    fn record_bodiless(&mut self, call: &ruby_prism::CallNode<'_>) {
        let name = name_of(call);
        let mut args: Vec<PrismNode<'_>> =
            call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        let macro_name = if SEND_CALLS.contains(&name.as_str()) {
            let Some(m) = literal_name(args.first()) else { return };
            args.remove(0);
            m
        } else {
            name
        };
        let meta = (macro_name == "new"
            && call
                .receiver()
                .and_then(|r| r.as_constant_read_node())
                .is_some_and(|c| constant_string(c.name().as_slice()) == "Struct"))
            || (macro_name == "define"
                && call
                    .receiver()
                    .and_then(|r| r.as_constant_read_node())
                    .is_some_and(|c| constant_string(c.name().as_slice()) == "Data"));
        if !meta && !BODILESS_MACROS.contains(&macro_name.as_str()) {
            return;
        }
        let only_first = macro_name == "alias_method";
        for (i, a) in args.iter().enumerate() {
            if only_first && i > 0 {
                break;
            }
            if let Some(base) = literal_name(Some(a)) {
                self.out.bodiless.push(format!("{base}="));
                self.out.bodiless.push(format!("{base}?"));
                self.out.bodiless.push(base);
            }
        }
    }

    fn def_slot(&self, n: &PrismNode<'_>, def: &ruby_prism::DefNode<'_>) -> Option<(String, bool)> {
        if !self.direct.contains(&span_of_node(n)) {
            return None;
        }
        let frame = self.frames.last()?;
        if frame.module_function {
            return None;
        }
        let owner = frame.owner.clone()?;
        match def.receiver() {
            None => Some((owner, frame.singleton)),
            Some(r) if r.as_self_node().is_some() && !frame.singleton && owner != "<toplevel>" => {
                Some((owner, true))
            }
            Some(_) => None,
        }
    }
}

impl<'pr> Visit<'pr> for Walk {
    fn visit_program_node(&mut self, node: &ruby_prism::ProgramNode<'pr>) {
        for s in node.statements().body().iter() {
            self.direct.insert(span_of_node(&s));
        }
        self.mark_direct_body(Some(&node.statements().as_node()));
        ruby_prism::visit_program_node(self, node);
    }

    fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
        let n = node.as_node();
        let owner = last_segment(&node.constant_path());
        if let Some(s) = node.superclass() {
            self.visit(&s);
        }
        self.declaration(&n, owner, false, node.body());
    }

    fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
        let n = node.as_node();
        let owner = last_segment(&node.constant_path());
        self.declaration(&n, owner, false, node.body());
    }

    fn visit_singleton_class_node(&mut self, node: &ruby_prism::SingletonClassNode<'pr>) {
        let n = node.as_node();
        let expr = node.expression();
        self.visit(&expr);
        let frame = self.frames.last().expect("root frame");
        let owner = if expr.as_self_node().is_some()
            && !frame.singleton
            && frame.owner.as_deref().is_some_and(|o| o != "<toplevel>")
        {
            frame.owner.clone()
        } else {
            None
        };
        self.declaration(&n, owner, true, node.body());
    }

    fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
        let n = node.as_node();
        let name = constant_string(node.name().as_slice());
        let slot = self.def_slot(&n, node);
        let escapes = match node.body() {
            None => false,
            Some(body) => {
                let block_name = node
                    .parameters()
                    .and_then(|p| p.block())
                    .and_then(|b| b.name())
                    .map(|c| constant_string(c.as_slice()));
                let mut e = Escape { block_name, plain: HashSet::new(), found: false };
                e.visit(&body);
                e.found
            }
        };
        self.out.defs.push(RebindDef { name, slot, escapes });
        // A `def` opens a new local-variable scope, and nothing in its body
        // is a direct statement.
        let saved = std::mem::replace(&mut self.scope, n.location().start_offset());
        self.frames.push(Frame { owner: None, singleton: false, module_function: false });
        ruby_prism::visit_def_node(self, node);
        self.frames.pop();
        self.scope = saved;
    }

    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        let name = name_of(node);
        if let Some(r) = node.receiver() {
            if PLAIN_PROC_CALLS.contains(&name.as_str()) {
                self.plain.insert(span_of_node(&r));
            }
        }
        if name == "module_function" && node.receiver().is_none() && node.arguments().is_none() {
            if let Some(f) = self.frames.last_mut() {
                f.module_function = true;
            }
        }
        self.record_bodiless(node);
        // `note_call`.
        let block = node.block();
        let body = match &block {
            Some(b) if b.as_block_node().is_some() => Some(span_of_node(b)),
            Some(b) => b
                .as_block_argument_node()
                .and_then(|ba| ba.expression())
                .and_then(|e| proc_literal_body(&e)),
            None => None,
        };
        let mut site_id: Option<u32> = None;
        if let Some(body) = body {
            let id = self.add_site(dispatch(node));
            site_id = Some(id);
            self.bodies.push((body, Some(id)));
        }
        if let Some(ba) = block.as_ref().and_then(|b| b.as_block_argument_node()) {
            if let Some(lv) = ba.expression().and_then(|e| e.as_local_variable_read_node()) {
                let id = match site_id {
                    Some(id) => id,
                    None => {
                        let id = self.add_site(dispatch(node));
                        site_id = Some(id);
                        id
                    }
                };
                self.block_passes.entry(constant_string(lv.name().as_slice())).or_default().push(id);
            }
        }
        let _ = site_id;
        let n = node.as_node();
        if let Some(value_body) = proc_literal_body(&n) {
            self.note_value(span_of_node(&n), value_body);
        }
        let target = send_target(node);
        if target.as_deref().is_none_or(|t| BINDING_CALLS.contains(&t)) {
            self.scopes_reading_locals.insert(self.scope);
        }
        ruby_prism::visit_call_node(self, node);
    }

    fn visit_lambda_node(&mut self, node: &ruby_prism::LambdaNode<'pr>) {
        let n = node.as_node();
        let span = span_of_node(&n);
        self.note_value(span, span);
        ruby_prism::visit_lambda_node(self, node);
    }

    fn visit_local_variable_read_node(&mut self, node: &ruby_prism::LocalVariableReadNode<'pr>) {
        let key = span_of_node(&node.as_node());
        if !self.block_arg_exprs.contains(&key) {
            let plain = self.plain.contains(&key);
            self.local_reads.entry(constant_string(node.name().as_slice())).or_default().push(plain);
        }
    }

    fn visit_local_variable_write_node(&mut self, node: &ruby_prism::LocalVariableWriteNode<'pr>) {
        self.lvw_values
            .insert(span_of_node(&node.value()), constant_string(node.name().as_slice()));
        ruby_prism::visit_local_variable_write_node(self, node);
    }

    fn visit_block_argument_node(&mut self, node: &ruby_prism::BlockArgumentNode<'pr>) {
        if let Some(e) = node.expression() {
            self.block_arg_exprs.insert(span_of_node(&e));
        }
        ruby_prism::visit_block_argument_node(self, node);
    }

    fn visit_if_node(&mut self, node: &ruby_prism::IfNode<'pr>) {
        self.plain.insert(span_of_node(&node.predicate()));
        ruby_prism::visit_if_node(self, node);
    }

    fn visit_unless_node(&mut self, node: &ruby_prism::UnlessNode<'pr>) {
        self.plain.insert(span_of_node(&node.predicate()));
        ruby_prism::visit_unless_node(self, node);
    }

    fn visit_while_node(&mut self, node: &ruby_prism::WhileNode<'pr>) {
        self.plain.insert(span_of_node(&node.predicate()));
        ruby_prism::visit_while_node(self, node);
    }

    fn visit_until_node(&mut self, node: &ruby_prism::UntilNode<'pr>) {
        self.plain.insert(span_of_node(&node.predicate()));
        ruby_prism::visit_until_node(self, node);
    }

    fn visit_and_node(&mut self, node: &ruby_prism::AndNode<'pr>) {
        self.plain.insert(span_of_node(&node.left()));
        ruby_prism::visit_and_node(self, node);
    }

    fn visit_alias_method_node(&mut self, node: &ruby_prism::AliasMethodNode<'pr>) {
        if let Some(name) = literal_name(Some(&node.new_name())) {
            self.out.bodiless.push(name);
        }
        ruby_prism::visit_alias_method_node(self, node);
    }

    fn visit_string_node(&mut self, node: &ruby_prism::StringNode<'pr>) {
        if let (Some(o), Some(c)) = (node.opening_loc(), node.closing_loc()) {
            if o.as_slice().starts_with(b"<<") {
                self.heredocs.push((o.start_offset(), node.content_loc().start_offset(), c.end_offset()));
            }
        }
        ruby_prism::visit_string_node(self, node);
    }

    fn visit_x_string_node(&mut self, node: &ruby_prism::XStringNode<'pr>) {
        let (o, c) = (node.opening_loc(), node.closing_loc());
        if o.as_slice().starts_with(b"<<") {
            self.heredocs.push((o.start_offset(), node.content_loc().start_offset(), c.end_offset()));
        }
        ruby_prism::visit_x_string_node(self, node);
    }

    fn visit_interpolated_string_node(&mut self, node: &ruby_prism::InterpolatedStringNode<'pr>) {
        if let (Some(o), Some(c)) = (node.opening_loc(), node.closing_loc()) {
            if o.as_slice().starts_with(b"<<") {
                let start = node.parts().iter().next().map_or(c.start_offset(), |p| p.location().start_offset());
                self.heredocs.push((o.start_offset(), start, c.end_offset()));
            }
        }
        ruby_prism::visit_interpolated_string_node(self, node);
    }

    fn visit_interpolated_x_string_node(&mut self, node: &ruby_prism::InterpolatedXStringNode<'pr>) {
        let (o, c) = (node.opening_loc(), node.closing_loc());
        if o.as_slice().starts_with(b"<<") {
            let start = node.parts().iter().next().map_or(c.start_offset(), |p| p.location().start_offset());
            self.heredocs.push((o.start_offset(), start, c.end_offset()));
        }
        ruby_prism::visit_interpolated_x_string_node(self, node);
    }
}

fn last_segment(path: &PrismNode<'_>) -> Option<String> {
    if let Some(c) = path.as_constant_read_node() {
        return Some(constant_string(c.name().as_slice()));
    }
    path.as_constant_path_node()
        .and_then(|cp| cp.name())
        .map(|n| constant_string(n.as_slice()))
}

/// `escapes?` over one `def` body: does it hand its block on?
struct Escape {
    block_name: Option<String>,
    plain: HashSet<Key>,
    found: bool,
}

impl<'pr> Visit<'pr> for Escape {
    fn visit_forwarding_super_node(&mut self, node: &ruby_prism::ForwardingSuperNode<'pr>) {
        if node.block().is_none() {
            self.found = true;
        }
        ruby_prism::visit_forwarding_super_node(self, node);
    }

    fn visit_super_node(&mut self, node: &ruby_prism::SuperNode<'pr>) {
        if node.block().is_none() {
            self.found = true;
        }
        ruby_prism::visit_super_node(self, node);
    }

    fn visit_block_argument_node(&mut self, node: &ruby_prism::BlockArgumentNode<'pr>) {
        if node.expression().is_none() {
            self.found = true;
        }
        ruby_prism::visit_block_argument_node(self, node);
    }

    fn visit_forwarding_arguments_node(&mut self, _node: &ruby_prism::ForwardingArgumentsNode<'pr>) {
        self.found = true;
    }

    fn visit_local_variable_read_node(&mut self, node: &ruby_prism::LocalVariableReadNode<'pr>) {
        if self.block_name.as_deref() == Some(constant_string(node.name().as_slice()).as_str())
            && !self.plain.contains(&span_of_node(&node.as_node()))
        {
            self.found = true;
        }
    }

    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        let name = name_of(node);
        if let Some(r) = node.receiver() {
            if PLAIN_PROC_CALLS.contains(&name.as_str()) {
                self.plain.insert(span_of_node(&r));
            }
        }
        // `reaches_locals?`.
        match send_target(node) {
            None => self.found = true,
            Some(t) if BINDING_CALLS.contains(&t.as_str()) => self.found = true,
            Some(t) if STRING_EVAL_CALLS.contains(&t.as_str()) && node.arguments().is_some() => {
                self.found = true
            }
            _ => {}
        }
        ruby_prism::visit_call_node(self, node);
    }

    fn visit_if_node(&mut self, node: &ruby_prism::IfNode<'pr>) {
        self.plain.insert(span_of_node(&node.predicate()));
        ruby_prism::visit_if_node(self, node);
    }

    fn visit_unless_node(&mut self, node: &ruby_prism::UnlessNode<'pr>) {
        self.plain.insert(span_of_node(&node.predicate()));
        ruby_prism::visit_unless_node(self, node);
    }

    fn visit_while_node(&mut self, node: &ruby_prism::WhileNode<'pr>) {
        self.plain.insert(span_of_node(&node.predicate()));
        ruby_prism::visit_while_node(self, node);
    }

    fn visit_until_node(&mut self, node: &ruby_prism::UntilNode<'pr>) {
        self.plain.insert(span_of_node(&node.predicate()));
        ruby_prism::visit_until_node(self, node);
    }

    fn visit_and_node(&mut self, node: &ruby_prism::AndNode<'pr>) {
        self.plain.insert(span_of_node(&node.left()));
        ruby_prism::visit_and_node(self, node);
    }
}

pub(crate) fn collect(root: &PrismNode<'_>) -> RebindFacts {
    let mut w = Walk {
        out: RebindFacts::default(),
        bodies: Vec::new(),
        heredocs: Vec::new(),
        proc_values: Vec::new(),
        local_reads: HashMap::new(),
        block_passes: HashMap::new(),
        scopes_reading_locals: HashSet::new(),
        plain: HashSet::new(),
        block_arg_exprs: HashSet::new(),
        lvw_values: HashMap::new(),
        scope: usize::MAX,
        frames: vec![Frame { owner: Some("<toplevel>".to_string()), singleton: false, module_function: false }],
        direct: HashSet::new(),
    };
    w.visit(root);
    // `value_targets`: a proc value assigned to a local reaches the calls
    // that receive it as `&local` when every other read only calls or tests
    // it; anything else is UNKNOWN.
    for pv in std::mem::take(&mut w.proc_values) {
        let targets: Vec<Option<u32>> = if w.scopes_reading_locals.contains(&pv.scope) {
            vec![None]
        } else if let Some(name) = &pv.lvw {
            let followed = w.local_reads.get(name).is_none_or(|reads| reads.iter().all(|p| *p));
            if followed {
                w.block_passes.get(name).map(|v| v.iter().map(|&i| Some(i)).collect()).unwrap_or_default()
            } else {
                vec![None]
            }
        } else {
            vec![None]
        };
        for t in targets {
            w.bodies.push((pv.body, t));
        }
    }
    w.heredocs.sort_unstable();
    for &((from, to), target) in &w.bodies {
        w.out.rows.push(RebindRow { from, to, site: target });
        let idx = w.heredocs.partition_point(|h| h.0 < from);
        for &(opening, start, stop) in &w.heredocs[idx..] {
            if opening >= to {
                break;
            }
            w.out.rows.push(RebindRow { from: start, to: stop, site: target });
        }
    }
    w.out.rows.sort_by_key(|r| r.from);
    w.out
}
