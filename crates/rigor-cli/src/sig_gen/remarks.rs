//! The visibility a visibility CALL gives a method slot after its `def`
//! (reference `ScopeIndexer#walk_method_visibilities`, rigor-rs#535).
//!
//! The reference's table is keyed by `(class, name)` per FILE and is written by
//! a full-depth walk of every class body (a reopening shares the slot; the walk
//! stops at a `def` body and a `class << self` body). A `def` writes the slot at
//! its running default, and a literal-name visibility call (`private :b`,
//! `private "b"`, `private %i[b c]`, `send(:private, :b)`, `module_function
//! :b` — read as `private`) writes it wherever it sits: after the def, inside
//! another `if`, in a block, in a later reopening. `sig-gen` then reads the
//! slot by NAME, so the LAST write in source order decides every `def` of that
//! name.
//!
//! The lowered class tables only see direct statements, so a conditional def
//! (`class K; if c; def b; end; end; private :b; end`) never learned of its
//! re-mark. This pass reads the whole file once and answers, per instance `def`
//! start offset, the visibility the LAST write to its slot was a call's — or
//! nothing when the last write was a `def` (its running default is already read
//! by the walk) or an unreadable call (`private(*names)`, a name computed at
//! run time: the reference does not apply those either).

use std::collections::{HashMap, HashSet};

use rigor_parse::ruby_prism::{
    self, CallNode, ClassNode, DefNode, ModuleNode, Node as PrismNode, ParseResult,
    SingletonClassNode, Visit,
};
use rigor_parse::Visibility;

use super::module_function::meta_new_call;
use super::superclasses::declared_path;

/// `def` start offset → the visibility a later (or earlier-reopened) visibility
/// call left on its slot, for each instance `def` whose slot's last write was
/// such a call.
pub fn def_slot_visibilities(result: &ParseResult<'_>) -> HashMap<usize, Visibility> {
    let mut walker = Walker {
        prefix: Vec::new(),
        skip: HashSet::new(),
        writes: HashMap::new(),
        defs: Vec::new(),
    };
    walker.visit(&result.node());
    let mut out = HashMap::new();
    for (slot, offset) in &walker.defs {
        let Some(writes) = walker.writes.get(slot) else { continue };
        // The walk is top-to-bottom, so the vector is already in source order;
        // sort anyway — the call offset of `private def b` precedes the def's.
        let last = writes.iter().max_by_key(|(at, _)| *at);
        if let Some((_, Some(visibility))) = last {
            out.insert(*offset, *visibility);
        }
    }
    out
}

/// A slot's write: the offset it happens at, and the visibility a call gave it
/// (`None` for a `def` or an `attr_*` member, which write the running default).
type Write = (usize, Option<Visibility>);

struct Walker {
    /// The declared path of the class or module body being walked.
    prefix: Vec<String>,
    /// `attr_*` calls already read as an argument of a visibility call.
    skip: HashSet<usize>,
    /// Every write to a `(class, name)` slot.
    writes: HashMap<(String, String), Vec<Write>>,
    /// Every instance `def` the walk met: its slot and start offset.
    defs: Vec<((String, String), usize)>,
}

impl Walker {
    fn slot(&self, name: String) -> (String, String) {
        (self.prefix.join("::"), name)
    }

    fn write(&mut self, name: String, at: usize, visibility: Option<Visibility>) {
        let slot = self.slot(name);
        self.writes.entry(slot).or_default().push((at, visibility));
    }

    /// Run `body` under `path` — a class or module header opens its body
    /// there; a header the walk cannot name opens one shared bucket that no
    /// named class reads.
    fn within(&mut self, path: Option<Vec<String>>, body: impl FnOnce(&mut Self)) {
        let child = path.unwrap_or_else(|| vec!["<unnameable>".to_string()]);
        let saved = std::mem::replace(&mut self.prefix, child);
        body(self);
        self.prefix = saved;
    }

    /// One argument of a visibility call: a literal name (or each literal
    /// element of an array) takes the call's visibility, as does each member
    /// of a wrapped `attr_*` call.
    fn mark(&mut self, element: &PrismNode<'_>, at: usize, visibility: Visibility) {
        if let Some(name) = symbol_or_string(element) {
            self.write(name, at, Some(visibility));
        } else if let Some(attr) = element.as_call_node() {
            for member in attr_members(&attr) {
                self.write(member, at, Some(visibility));
            }
            self.skip.insert(attr.location().start_offset());
        }
    }
}

fn name_of(call: &CallNode<'_>) -> String {
    String::from_utf8_lossy(call.name().as_slice()).into_owned()
}

fn arguments<'pr>(call: &CallNode<'pr>) -> Vec<PrismNode<'pr>> {
    call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default()
}

/// `Source::Literals.symbol_or_string`.
fn symbol_or_string(node: &PrismNode<'_>) -> Option<String> {
    if let Some(sym) = node.as_symbol_node() {
        return Some(String::from_utf8_lossy(sym.unescaped()).into_owned());
    }
    node.as_string_node().map(|s| String::from_utf8_lossy(s.unescaped()).into_owned())
}

/// `visibility_call_parts`: `(visibility, arguments)` for a call Ruby runs as
/// `private` / `protected` / `public` / `module_function` (applied as `private`,
/// #1569) on the body's self — receiverless or on `self`, or sent with a
/// literal name. `None` for any other call.
fn visibility_call<'pr>(call: &CallNode<'pr>) -> Option<(Visibility, Vec<PrismNode<'pr>>)> {
    if call.receiver().is_some_and(|r| r.as_self_node().is_none()) {
        return None;
    }
    let mut name = name_of(call);
    let mut args = arguments(call);
    if matches!(name.as_str(), "send" | "__send__") {
        name = args.first().and_then(symbol_or_string)?;
        args.remove(0);
    }
    let visibility = match name.as_str() {
        "public" => Visibility::Public,
        "protected" => Visibility::Protected,
        "private" | "module_function" => Visibility::Private,
        _ => return None,
    };
    Some((visibility, args))
}

/// The members a receiverless `attr_reader` / `attr_writer` / `attr_accessor`
/// call defines (`x`, `x=`).
fn attr_members(call: &CallNode<'_>) -> Vec<String> {
    if call.receiver().is_some() {
        return Vec::new();
    }
    let macro_name = name_of(call);
    if !matches!(macro_name.as_str(), "attr_reader" | "attr_writer" | "attr_accessor") {
        return Vec::new();
    }
    let mut members = Vec::new();
    for arg in arguments(call) {
        let Some(base) = symbol_or_string(&arg) else { continue };
        if macro_name != "attr_writer" {
            members.push(base.clone());
        }
        if macro_name != "attr_reader" {
            members.push(format!("{base}="));
        }
    }
    members
}

impl<'pr> Visit<'pr> for Walker {
    fn visit_class_node(&mut self, node: &ClassNode<'pr>) {
        let path = declared_path(&self.prefix, &node.constant_path());
        self.within(path, |w| {
            if let Some(body) = node.body() {
                w.visit(&body);
            }
        });
    }

    fn visit_module_node(&mut self, node: &ModuleNode<'pr>) {
        let path = declared_path(&self.prefix, &node.constant_path());
        self.within(path, |w| {
            if let Some(body) = node.body() {
                w.visit(&body);
            }
        });
    }

    /// `class << self` marks the singleton side, which `sig-gen` never
    /// visibility-skips.
    fn visit_singleton_class_node(&mut self, _node: &SingletonClassNode<'pr>) {}

    /// The walk stops at a `def`: its body is not a class body.
    fn visit_def_node(&mut self, node: &DefNode<'pr>) {
        if node.receiver().is_some() {
            return;
        }
        let name = String::from_utf8_lossy(node.name().as_slice()).into_owned();
        let at = node.location().start_offset();
        self.write(name.clone(), at, None);
        let slot = self.slot(name);
        self.defs.push((slot, at));
    }

    fn visit_call_node(&mut self, node: &CallNode<'pr>) {
        // `Class.new { }` / `Struct.new(:a) do end`: the block is another
        // class's body, which the reference walks as its own.
        if node.block().is_some() && meta_new_call(node) {
            return;
        }
        let at = node.location().start_offset();
        if let Some((visibility, args)) = visibility_call(node) {
            for arg in &args {
                match arg.as_array_node() {
                    Some(array) => {
                        for element in array.elements().iter() {
                            self.mark(&element, at, visibility);
                        }
                    }
                    None => self.mark(arg, at, visibility),
                }
            }
        } else if !self.skip.contains(&at) {
            for member in attr_members(node) {
                self.write(member, at, None);
            }
        }
        ruby_prism::visit_call_node(self, node);
    }
}
