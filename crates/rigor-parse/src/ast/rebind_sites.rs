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
use super::definitions::{literal_symbol_or_string_name, visibility_call_parts};

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
    /// Upstream v0.4.6 (#2237 / issue #2013) — every instance-side
    /// `define_method(:x)` / `alias_method :x, …` / `alias x y` that replaces
    /// a method of a class body: `(owner's last constant segment, x,
    /// settled)`. `settled` holds when a LATER certain visibility write for
    /// the name in the same FILE clears the contest (the reference's
    /// `wrote` — a direct `private :x` / `protected :x` / `public :x` /
    /// `module_function :x`, a `private def`-family argument, an `attr_*`
    /// member, or a reinstating `def`).
    pub replacers: Vec<(String, String, bool)>,
    /// Issue #492 (upstream #2013, `supersede_def_nodes`) — the
    /// instance-side `(owner, name)` pairs whose DEF-NODE slot a non-`def`
    /// definer superseded after an earlier `def` of the name in the same
    /// file: `attr_*`, `define_method`, `alias_method`, `alias`, a
    /// Forwardable delegator. A later `def` of the name clears the mark.
    pub superseded: Vec<(String, String)>,
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
/// `class_eval` and the calls that rebind `self` to the receiver's class
/// (the reference's `SELF_REBINDING_EVAL_CALLS`).
const SELF_REBINDING_EVAL_CALLS: [&str; 4] = ["class_eval", "module_eval", "class_exec", "module_exec"];
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
    /// The class whose instance side a `define_method` / alias here replaces.
    vis_owner: Option<String>,
    /// The body's running visibility is not provable — a `private` /
    /// `protected` / `public` / `module_function` toggle that was itself
    /// uncertain (the reference's `VisibilityTable#uncertain`).
    uncertain: bool,
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
    /// `def` spans that are the argument of a direct `private` /
    /// `protected` / `public` / `module_function` — the modifier itself
    /// writes the def's visibility, so the def's own write is certain
    /// regardless of the body's running default.
    modifier_arg_defs: HashSet<Key>,
    /// Source position for ordering replacer events against settle events.
    pos: u32,
    /// `(pos, owner, name)` per replacer — `settled` is resolved in
    /// `collect` once every settle event is recorded.
    replacer_events: Vec<(u32, String, String)>,
    /// `(pos, owner, name)` per certain visibility write that clears a
    /// replacer's contest.
    settler_events: Vec<(u32, String, String)>,
    /// `(owner, name)` pairs an instance `def` recorded earlier in this
    /// file — the `slot.key?(name)` precondition of `supersede_def_nodes`.
    def_seen: HashSet<(String, String)>,
    /// Def-node slots a non-`def` definer superseded (`contested_def_nodes`).
    superseded: HashSet<(String, String)>,
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
                    let cn = name_of(&c);
                    if c.receiver().is_none() && VISIBILITY_MODIFIERS.contains(&cn.as_str()) {
                        if let Some(args) = c.arguments() {
                            for a in args.arguments().iter() {
                                if a.as_def_node().is_some() {
                                    self.direct.insert(span_of_node(&a));
                                }
                            }
                        }
                    }
                    // `private def x` / `module_function def x` — the
                    // modifier itself writes the def's visibility, so the
                    // def's own write is certain.
                    if c.receiver().is_none()
                        && (cn == "module_function" || VISIBILITY_MODIFIERS.contains(&cn.as_str()))
                    {
                        if let Some(args) = c.arguments() {
                            for a in args.arguments().iter() {
                                if a.as_def_node().is_some() {
                                    self.modifier_arg_defs.insert(span_of_node(&a));
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
        let vis_owner = if singleton { None } else { owner.clone() };
        self.frames.push(Frame { owner, singleton, module_function: false, vis_owner, uncertain: false });
        if let Some(b) = &body {
            self.visit(b);
        }
        self.frames.pop();
    }

    /// One certain visibility write — the events a replacer's `settled`
    /// resolves against in `collect`.
    fn push_settle(&mut self, owner: String, name: String) {
        self.pos += 1;
        self.settler_events.push((self.pos, owner, name));
    }

    /// A non-`def` definer superseded the def's own node (`supersede_def_nodes`):
    /// mark the def-node slot contested when the name's `def` was already
    /// seen in this file.
    fn record_supersede(&mut self, n: &PrismNode<'_>) {
        let Some(frame) = self.frames.last() else { return };
        if frame.singleton {
            return;
        }
        let Some(owner) = frame.vis_owner.clone() else { return };
        for x in superseded_names(n) {
            if self.def_seen.contains(&(owner.clone(), x.clone())) {
                self.superseded.insert((owner.clone(), x));
            }
        }
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

    fn record_replacer(&mut self, n: &PrismNode<'_>) {
        let Some(x) = replaced_name(n) else { return };
        let Some(owner) = self.frames.last().and_then(|f| f.vis_owner.clone()) else { return };
        self.pos += 1;
        self.replacer_events.push((self.pos, owner, x));
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
        // Instance-side `def` — the def-node slot bookkeeping
        // (`supersede_def_nodes`' `slot.key?` + a later `def`'s clear) and
        // the certain-write event that settles a replacer's contest. The
        // reference only reads the name itself, not the position.
        if node.receiver().is_none() {
            if let Some(owner) = self.frames.last().and_then(|f| f.vis_owner.clone()) {
                let dname = constant_string(node.name().as_slice());
                let key = span_of_node(&n);
                if self.direct.contains(&key) {
                    self.superseded.remove(&(owner.clone(), dname.clone()));
                } else {
                    self.superseded.insert((owner.clone(), dname.clone()));
                }
                self.def_seen.insert((owner.clone(), dname.clone()));
                let settled_def = self.modifier_arg_defs.contains(&key)
                    || (self.direct.contains(&key)
                        && !self.frames.last().is_some_and(|f| f.uncertain));
                if settled_def {
                    self.push_settle(owner, dname);
                }
            }
        }
        // A `def` opens a new local-variable scope, and nothing in its body
        // is a direct statement. The `uncertain` flag inherits — the
        // reference's `VisibilityTable` keeps it through a `def` body (only
        // `body` — a class, module or eval frame — resets it).
        let saved = std::mem::replace(&mut self.scope, n.location().start_offset());
        let (vis_owner, uncertain) = self
            .frames
            .last()
            .map(|f| (f.vis_owner.clone(), f.uncertain))
            .unwrap_or((None, false));
        self.frames.push(Frame { owner: None, singleton: false, module_function: false, vis_owner, uncertain });
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
        self.record_replacer(&node.as_node());
        // Visibility toggles set the body's `uncertain` (the reference's
        // `VisibilityTable#uncertain` — a `private` under `if` makes the
        // running default unprovable); a splat leaves it uncertain.
        if let Some((_, vargs)) = visibility_call_parts(node) {
            let direct = self.direct.contains(&span_of_node(&node.as_node()));
            if vargs.is_empty() {
                if let Some(f) = self.frames.last_mut() {
                    f.uncertain = !direct;
                }
            } else if vargs.iter().any(|a| a.as_splat_node().is_some()) {
                if let Some(f) = self.frames.last_mut() {
                    f.uncertain = true;
                }
            } else if direct {
                // A certain named visibility write settles a replacer's
                // contest for the name (`VisibilityTable#wrote`) — and a
                // `private attr_reader :x` writes its members certain too
                // (`record_attr_visibility` on the argument).
                if let Some(owner) = self.frames.last().and_then(|f| f.vis_owner.clone()) {
                    for a in &vargs {
                        if let Some(x) = literal_symbol_or_string_name(a) {
                            self.push_settle(owner.clone(), x);
                        } else if let Some(members) =
                            a.as_call_node().and_then(|c| attr_macro_names(&c))
                        {
                            for m in members {
                                self.push_settle(owner.clone(), m);
                            }
                        }
                    }
                }
            }
        }
        // A direct `attr_*` writes each member the running visibility — a
        // certain write that settles a replacer's contest.
        if self.direct.contains(&span_of_node(&node.as_node())) {
            if let Some(members) = attr_macro_names(node) {
                if let Some(owner) = self.frames.last().and_then(|f| f.vis_owner.clone()) {
                    for m in members {
                        self.push_settle(owner.clone(), m);
                    }
                }
            }
        }
        self.record_supersede(&node.as_node());
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
        // `walk_eval_visibilities`: a `K.class_eval { … }`-family block
        // outside a `def` re-anchors its replacers on the receiver (`A::K`
        // by its last segment), wherever the call is written.
        let eval_owner = if self.scope == usize::MAX
            && SELF_REBINDING_EVAL_CALLS.contains(&name.as_str())
            && node.block().is_some_and(|b| b.as_block_node().is_some())
        {
            node.receiver().and_then(|r| {
                if r.as_constant_read_node().is_some() || r.as_constant_path_node().is_some() {
                    last_segment(&r)
                } else {
                    None
                }
            })
        } else {
            None
        };
        if let Some(owner) = eval_owner {
            self.frames.push(Frame { owner: None, singleton: false, module_function: false, vis_owner: Some(owner), uncertain: false });
            ruby_prism::visit_call_node(self, node);
            self.frames.pop();
        } else {
            ruby_prism::visit_call_node(self, node);
        }
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
        self.record_replacer(&node.as_node());
        self.record_supersede(&node.as_node());
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

/// The name a `define_method(:x)` / `alias_method :x, :y` / `alias x y`
/// (on no receiver or `self`, also through a literal `send`) replaces.
/// Widened from the reference's `replaced_by_call`: an extra name declines.
fn replaced_name(n: &PrismNode<'_>) -> Option<String> {
    if let Some(a) = n.as_alias_method_node() {
        return literal_name(Some(&a.new_name()));
    }
    let call = n.as_call_node()?;
    if call.receiver().is_some_and(|r| r.as_self_node().is_none()) {
        return None;
    }
    let mut args: Vec<PrismNode<'_>> =
        call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    let mut name = name_of(&call);
    if SEND_CALLS.contains(&name.as_str()) {
        name = literal_name(args.first())?;
        args.remove(0);
    }
    if name == "define_method" || name == "alias_method" {
        literal_name(args.first())
    } else {
        None
    }
}

/// The member names an `attr_*` call declares (the reference's
/// `attr_macro` + `each_attr_macro_name`): receiverless or `self.` only,
/// named directly or as the literal first argument of `send` /
/// `__send__` / `public_send`.
fn attr_macro_names(call: &ruby_prism::CallNode<'_>) -> Option<Vec<String>> {
    if call.receiver().is_some_and(|r| r.as_self_node().is_none()) {
        return None;
    }
    let mut macro_name = name_of(call);
    let mut args: Vec<PrismNode<'_>> =
        call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    if !["attr_reader", "attr_writer", "attr_accessor"].contains(&macro_name.as_str()) {
        if !SEND_CALLS.contains(&macro_name.as_str()) {
            return None;
        }
        macro_name = args.first().and_then(|a| literal_symbol_or_string_name(a))?;
        args.remove(0);
        if !["attr_reader", "attr_writer", "attr_accessor"].contains(&macro_name.as_str()) {
            return None;
        }
    }
    let mut members = Vec::new();
    for arg in &args {
        let Some(base) = literal_symbol_or_string_name(arg) else { continue };
        if macro_name != "attr_writer" {
            members.push(base.clone());
        }
        if macro_name != "attr_reader" {
            members.push(format!("{base}="));
        }
    }
    Some(members)
}

/// The method names a Forwardable delegator call defines (`def_delegator`,
/// `def_delegators`, `delegate`, the `def_instance_*` / `instance_delegate`
/// forms). The ambiguous-side macros are read as instance — the port has
/// no `extend` census, and a `SingleForwardable` body wrongly declining is
/// safe.
fn forwardable_names(call: &ruby_prism::CallNode<'_>) -> Vec<String> {
    if call.receiver().is_some_and(|r| r.as_self_node().is_none()) {
        return Vec::new();
    }
    let name = name_of(call);
    let args: Vec<PrismNode<'_>> =
        call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    match name.as_str() {
        "def_delegator" | "def_instance_delegator" => {
            // `(accessor, method, alias = method)` — the alias is the name.
            if (2..=3).contains(&args.len()) {
                args.last().and_then(|a| literal_symbol_or_string_name(a)).into_iter().collect()
            } else {
                Vec::new()
            }
        }
        "def_delegators" | "def_instance_delegators" => {
            // `(accessor, *methods)`.
            if args.is_empty() {
                Vec::new()
            } else {
                args.iter().skip(1).filter_map(|a| literal_symbol_or_string_name(a)).collect()
            }
        }
        "delegate" | "instance_delegate" => {
            // `(method => accessor, …)` — the hash's literal keys.
            if args.len() != 1 {
                return Vec::new();
            }
            let elements = args[0]
                .as_hash_node()
                .map(|h| h.elements())
                .or_else(|| args[0].as_keyword_hash_node().map(|h| h.elements()));
            let Some(elements) = elements else { return Vec::new() };
            elements
                .iter()
                .filter_map(|e| e.as_assoc_node())
                .filter_map(|a| literal_symbol_or_string_name(&a.key()))
                .collect()
        }
        _ => Vec::new(),
    }
}

/// The instance-side names a node supersedes a `def` with (the reference's
/// `call_superseded_names` / the `alias` arm): `attr_*` members,
/// `define_method` / `alias_method` names (the port's widened
/// `replaced_name` surface), Forwardable delegator names.
fn superseded_names(n: &PrismNode<'_>) -> Vec<String> {
    if let Some(call) = n.as_call_node() {
        if let Some(members) = attr_macro_names(&call) {
            return members;
        }
        if let Some(x) = replaced_name(n) {
            return vec![x];
        }
        return forwardable_names(&call);
    }
    if n.as_alias_method_node().is_some() {
        return replaced_name(n).into_iter().collect();
    }
    Vec::new()
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
        frames: vec![Frame {
            owner: Some("<toplevel>".to_string()),
            singleton: false,
            module_function: false,
            vis_owner: None,
            uncertain: false,
        }],
        direct: HashSet::new(),
        modifier_arg_defs: HashSet::new(),
        pos: 0,
        replacer_events: Vec::new(),
        settler_events: Vec::new(),
        def_seen: HashSet::new(),
        superseded: HashSet::new(),
    };
    w.visit(root);
    // A replacer's `settled`: the last certain visibility write for the
    // name in the FILE that follows it clears the contest (`wrote`).
    let mut last_settle: HashMap<(String, String), u32> = HashMap::new();
    for (pos, owner, name) in &w.settler_events {
        last_settle.insert((owner.clone(), name.clone()), *pos);
    }
    for (pos, owner, name) in &w.replacer_events {
        let settled = last_settle
            .get(&(owner.clone(), name.clone()))
            .is_some_and(|p| *p > *pos);
        w.out.replacers.push((owner.clone(), name.clone(), settled));
    }
    w.out.superseded = w.superseded.iter().cloned().collect();

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
