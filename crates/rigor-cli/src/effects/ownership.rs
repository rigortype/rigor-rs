//! Who owns a mutated receiver — a port of upstream's
//! `lib/rigor/effects/local_ownership.rb` and the ownership half of
//! `mutation_classifier.rb` (ADR-0043 slice 2).
//!
//! Two independent questions, both conservative, and **both answered from
//! SYNTAX alone** — which is why slice 2 can port them whole while declining
//! the typer everywhere else:
//!
//! 1. **Is this a mutation?** [`MutationClassifier::mutating`]. `[]=` and an
//!    attribute writer are writes on every receiver, so they need no type; the
//!    bang family and `<<` are claimed only when the receiver's class is known,
//!    because `n << 2` is a bit shift and `io << "x"` is output. The collector
//!    passes a class only where the SYNTAX named it (a constant-path receiver),
//!    never a typer's projection.
//! 2. **Who owns the receiver?** [`MutationClassifier::label_for`]. `self` and
//!    its ivars are `mutate.self` (`mutate.static` in singleton context), a
//!    class variable is `mutate.static`, a parameter is `mutate.instance`, a
//!    frame-owned local is `mutate.local`, and so is a receiver that is itself
//!    an allocation (`{}.compare_by_identity`, `raw.dup.force_encoding(e)`) by
//!    the local rule's own witness ([`allocation`]). **Anything else answers
//!    None**, and the caller records nothing rather than a proven bare
//!    `mutate`: Ruby's ownership is a dataflow question, and a proven parent
//!    label on a fresh-but-unproven receiver would put findings on correct
//!    code.
//!
//! [`owned_locals`] is deliberately **flow-insensitive and whole-body**: a local
//! that escapes anywhere disqualifies, even if the escape happens after the
//! mutation. That is strictly more conservative than the "escaped before the
//! mutating call" reading, and the conservative direction is the safe one here.

use std::collections::{BTreeMap, BTreeSet};

use rigor_parse::ruby_prism::{CallNode, Node, Visit};

/// Assignment right-hand sides that witness a fresh allocation on ANY
/// receiver (`local_ownership.rb:29`, `COPYING_SELECTORS`). `.new` is NOT
/// here — only a class object's `new` is `Class#new`, which is
/// [`constructor`]'s question.
const COPYING_SELECTORS: &[&[u8]] = &[b"dup", b"clone"];

/// The `mutate.*` label an ownership answer earns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ownership {
    /// `self` or an `@ivar` in an instance unit.
    SelfState,
    /// A `@@cvar`, or any `self`-shaped receiver inside a singleton unit.
    Static,
    /// A parameter: the caller holds the same object.
    Instance,
    /// A frame-owned local: no caller can observe the mutation.
    Local,
}

impl Ownership {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::SelfState => "mutate.self",
            Self::Static => "mutate.static",
            Self::Instance => "mutate.instance",
            Self::Local => "mutate.local",
        }
    }
}

/// Upstream's `MutationClassifier`, minus the receiver-class projection it
/// takes from the typer (the collector supplies only a syntax-settled class).
pub(super) struct MutationClassifier {
    singleton: bool,
    parameters: BTreeSet<String>,
    owned_locals: BTreeSet<String>,
}

impl MutationClassifier {
    pub(super) fn new(
        singleton: bool,
        parameters: BTreeSet<String>,
        owned_locals: BTreeSet<String>,
    ) -> Self {
        Self { singleton, parameters, owned_locals }
    }

    /// Upstream's `ATTRIBUTE_WRITER` — `/\A[a-z_][A-Za-z0-9_]*=\z/`, and
    /// deliberately not `==` / `<=` / `!=` / `===`.
    pub(super) fn attribute_writer(selector: &str) -> bool {
        let Some(name) = selector.strip_suffix('=') else { return false };
        let mut chars = name.chars();
        let Some(head) = chars.next() else { return false };
        (head.is_ascii_lowercase() || head == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    }

    /// Whether `selector` mutates its receiver whatever that receiver turns out
    /// to be — upstream's `UNIVERSAL_MUTATORS` ∪ `ATTRIBUTE_WRITER`
    /// (`mutation_classifier.rb:58`).
    pub(super) fn universally_mutating(selector: &str) -> bool {
        selector == "[]=" || Self::attribute_writer(selector)
    }

    /// Whether this call mutates its receiver. `receiver_class` is a class the
    /// SYNTAX named, or None.
    pub(super) fn mutating(selector: &str, receiver_class: Option<&str>) -> bool {
        if Self::universally_mutating(selector) {
            return true;
        }
        let set = match receiver_class {
            Some("Array") => "array",
            Some("Hash") => "hash",
            Some("String") => "string",
            _ => return false,
        };
        rigor_effects::mutators().set(Some(set)).contains(selector)
    }

    /// The ownership a mutation of `receiver` earns, or None when it is not
    /// provable. A `None` receiver is the implicit-self case.
    pub(super) fn label_for(&self, receiver: Option<&Node<'_>>) -> Option<Ownership> {
        let Some(receiver) = receiver else { return Some(self.self_or_static()) };
        if receiver.as_self_node().is_some() || receiver.as_instance_variable_read_node().is_some()
        {
            return Some(self.self_or_static());
        }
        if receiver.as_class_variable_read_node().is_some() {
            return Some(Ownership::Static);
        }
        if let Some(local) = receiver.as_local_variable_read_node() {
            let name = name_of(local.name().as_slice());
            if self.parameters.contains(&name) {
                return Some(Ownership::Instance);
            }
            return self.owned_locals.contains(&name).then_some(Ownership::Local);
        }
        // Upstream's `else` arm (`mutation_classifier.rb:91`): a receiver that
        // is itself an allocation earns `mutate.local` — each evaluation is a
        // new object no caller holds yet (`{}.compare_by_identity`,
        // `raw.dup.force_encoding(e)`).
        allocation(receiver, self.singleton).then_some(Ownership::Local)
    }

    fn self_or_static(&self) -> Ownership {
        if self.singleton { Ownership::Static } else { Ownership::SelfState }
    }
}

/// The set of frame-owned local names in `body`, given the method's parameter
/// names (a parameter is never frame-owned — the caller holds the same object,
/// so mutating it is `mutate.instance`) and whether `self` is a class there
/// ([`constructor`] reads it, `local_ownership.rb:33`).
pub(super) fn owned_locals(
    body: Option<&Node<'_>>,
    parameters: &BTreeSet<String>,
    singleton: bool,
) -> BTreeSet<String> {
    let Some(body) = body else { return BTreeSet::new() };
    let mut scan = OwnershipScan { singleton, ..Default::default() };
    scan.visit(body);
    let OwnershipScan { assignments, mut escaped, .. } = scan;
    escaped.extend(trailing_read(body));
    assignments
        .into_iter()
        .filter(|(name, allocations)| {
            !escaped.contains(name)
                && !parameters.contains(name)
                && allocations.iter().all(|allocated| *allocated)
        })
        .map(|(name, _)| name)
        .collect()
}

/// `name -> [was each assignment an allocation?]`, plus the names that escaped.
#[derive(Default)]
struct OwnershipScan {
    assignments: BTreeMap<String, Vec<bool>>,
    escaped: BTreeSet<String>,
    /// Whether the enclosing unit's `self` is a class — [`constructor`]'s
    /// `singleton` (`scanner.rb:254`).
    singleton: bool,
}

impl OwnershipScan {
    fn record(&mut self, node: &Node<'_>) {
        self.record_assignment(node);
        self.record_escapes(node);
    }

    fn record_assignment(&mut self, node: &Node<'_>) {
        if let Some(write) = node.as_local_variable_write_node() {
            let value = write.value();
            let name = name_of(write.name().as_slice());
            self.assignments
                .entry(name)
                .or_default()
                .push(allocation(&value, self.singleton));
            // `y = x` hands the same object to a second name; neither can be
            // proven frame-private cheaply.
            self.note_read(Some(&value));
            return;
        }
        // Not an allocation, and a multi-assign target's value is not
        // statically one either: record a non-allocating assignment so the
        // all-allocations test fails.
        let name = if let Some(write) = node.as_local_variable_operator_write_node() {
            name_of(write.name().as_slice())
        } else if let Some(write) = node.as_local_variable_or_write_node() {
            name_of(write.name().as_slice())
        } else if let Some(write) = node.as_local_variable_and_write_node() {
            name_of(write.name().as_slice())
        } else if let Some(target) = node.as_local_variable_target_node() {
            name_of(target.name().as_slice())
        } else {
            return;
        };
        self.assignments.entry(name).or_default().push(false);
    }

    /// An escape is any position from which a caller could later reach the
    /// object: a call argument (the callee may store it), the right-hand side
    /// of a write to state that outlives the frame, an element of a constructed
    /// collection, or an explicit `return`.
    fn record_escapes(&mut self, node: &Node<'_>) {
        if let Some(call) = node.as_call_node() {
            if let Some(arguments) = call.arguments() {
                for argument in arguments.arguments().iter() {
                    self.note_read(Some(&argument));
                }
            }
            if let Some(block) = call.block().and_then(|block| block.as_block_argument_node()) {
                self.note_read(block.expression().as_ref());
            }
            return;
        }
        if let Some(returned) = node.as_return_node() {
            if let Some(arguments) = returned.arguments() {
                for argument in arguments.arguments().iter() {
                    self.note_read(Some(&argument));
                }
            }
            return;
        }
        if let Some(array) = node.as_array_node() {
            for element in array.elements().iter() {
                self.note_read(Some(&element));
            }
            return;
        }
        self.note_read(stored_value(node).as_ref());
    }

    fn note_read(&mut self, node: Option<&Node<'_>>) {
        if let Some(local) = node.and_then(Node::as_local_variable_read_node) {
            self.escaped.insert(name_of(local.name().as_slice()));
        }
    }
}

impl<'pr> Visit<'pr> for OwnershipScan {
    fn visit_branch_node_enter(&mut self, node: Node<'pr>) {
        self.record(&node);
    }

    fn visit_leaf_node_enter(&mut self, node: Node<'pr>) {
        self.record(&node);
    }
}

/// The value half of a write into state that outlives the frame, or of a hash
/// entry. None for every other node, which `note_read` ignores.
fn stored_value<'pr>(node: &Node<'pr>) -> Option<Node<'pr>> {
    if let Some(assoc) = node.as_assoc_node() {
        return Some(assoc.value());
    }
    if let Some(write) = node.as_instance_variable_write_node() {
        return Some(write.value());
    }
    if let Some(write) = node.as_class_variable_write_node() {
        return Some(write.value());
    }
    if let Some(write) = node.as_global_variable_write_node() {
        return Some(write.value());
    }
    node.as_constant_write_node().map(|write| write.value())
}

/// Whether `node` is an expression that allocates a fresh object this frame is
/// the sole holder of (`local_ownership.rb:51`). `singleton` is whether the
/// enclosing unit's `self` is a class, as [`constructor`] reads it.
fn allocation(node: &Node<'_>, singleton: bool) -> bool {
    if node.as_array_node().is_some()
        || node.as_hash_node().is_some()
        || node.as_string_node().is_some()
        || node.as_interpolated_string_node().is_some()
        || node.as_lambda_node().is_some()
    {
        return true;
    }
    let Some(call) = node.as_call_node() else { return false };
    COPYING_SELECTORS.contains(&call.name().as_slice())
        || constructor(&call, singleton)
        || unary_plus_string(&call)
}

/// A `new` whose receiver the author wrote as a class object: a constant path,
/// a `class` call (`self.class`, `other.class`), or `self` — explicit or
/// implicit — in a singleton-method body (`local_ownership.rb:67`). Any other
/// receiver may be an object whose `new` only shares the name: an ActiveRecord
/// association's `new` builds a record into the association's own target,
/// where the caller can reach it. `self` in an instance method is such a
/// receiver too — an association extension's `new` is the association's.
fn constructor(call: &CallNode<'_>, singleton: bool) -> bool {
    if call.name().as_slice() != b"new" {
        return false;
    }
    let Some(receiver) = call.receiver() else { return singleton };
    if receiver.as_self_node().is_some() {
        return singleton;
    }
    if receiver.as_constant_read_node().is_some()
        || receiver.as_constant_path_node().is_some()
    {
        return true;
    }
    match receiver.as_call_node() {
        Some(receiver_call) => class_call(&receiver_call),
        None => false,
    }
}

/// `x.class` — `Kernel#class` answers the receiver's class, unless the
/// receiver overrides it (`local_ownership.rb:80`).
fn class_call(call: &CallNode<'_>) -> bool {
    call.name().as_slice() == b"class" && call.arguments().is_none() && call.block().is_none()
}

/// `+""` — the frozen-string-literal era's spelling of "a fresh mutable String".
fn unary_plus_string(call: &CallNode<'_>) -> bool {
    call.name().as_slice() == b"+@"
        && call.receiver().is_some_and(|receiver| receiver.as_string_node().is_some())
}

/// A body whose value is a bare local read hands that local to the caller. Only
/// the tail matters — every other position is covered by `record_escapes`.
fn trailing_read(body: &Node<'_>) -> Option<String> {
    if let Some(statements) = body.as_statements_node() {
        let last = statements.body().iter().last()?;
        return last.as_local_variable_read_node().map(|local| name_of(local.name().as_slice()));
    }
    body.as_local_variable_read_node().map(|local| name_of(local.name().as_slice()))
}

pub(super) fn name_of(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Instance-method context (`singleton: false`) — most cases.
    fn owned(source: &str, parameters: &[&str]) -> Vec<String> {
        owned_as(source, parameters, false)
    }

    fn owned_as(source: &str, parameters: &[&str], singleton: bool) -> Vec<String> {
        let result = rigor_parse::parse(source.as_bytes());
        let params: BTreeSet<String> = parameters.iter().map(|p| (*p).to_string()).collect();
        // The real caller hands a method BODY, which is a `StatementsNode`; a
        // parse result's root is the enclosing `ProgramNode`, and `trailing_read`
        // reads the statements' tail.
        let program = result.node().as_program_node().expect("a program").statements();
        owned_locals(Some(&program.as_node()), &params, singleton)
            .into_iter()
            .collect()
    }

    #[test]
    fn a_fresh_unescaped_local_is_frame_owned() {
        // The probe's § 7a case: `s = +""; s.upcase!; nil` — a `; nil` tail is
        // the whole difference against the corpus fixture, whose trailing bare
        // read escapes the buffer.
        assert_eq!(owned("s = +\"\"\ns.upcase!\nnil", &[]), ["s"]);
        assert_eq!(owned("b = []\nb << 1\nnil", &[]), ["b"]);
        assert_eq!(owned("h = {}\nh[:k] = 1\nnil", &[]), ["h"]);
        assert_eq!(owned("o = Thing.new\no.x = 1\nnil", &[]), ["o"]);
        assert_eq!(owned("d = other.dup\nd.clear\nnil", &[]), ["d"]);
        // `x.class` receivers are class objects (`local_ownership.rb:80`).
        assert_eq!(owned("o = self.class.new\no.x = 1\nnil", &[]), ["o"]);
        assert_eq!(owned("o = other.class.new\no.x = 1\nnil", &[]), ["o"]);
    }

    #[test]
    fn new_allocates_only_on_a_class_object_receiver() {
        // `constructor?` (`local_ownership.rb:67`) — issue #614.
        // A non-class receiver's `new` only shares the name.
        assert!(owned("o = assoc.items.new\no.x = 1\nnil", &[]).is_empty());
        // Bare `new` on an instance body is `self.new`, and `self` is not a
        // class there; on a singleton body it is `Class#new`.
        assert!(owned("o = new\no.x = 1\nnil", &[]).is_empty());
        assert!(owned("o = self.new\no.x = 1\nnil", &[]).is_empty());
        assert_eq!(owned_as("o = new\no.x = 1\nnil", &[], true), ["o"]);
        assert_eq!(owned_as("o = self.new\no.x = 1\nnil", &[], true), ["o"]);
        // A constant receiver is always a class object.
        assert_eq!(owned("o = Thing.new\no.x = 1\nnil", &[]), ["o"]);
        assert_eq!(owned("o = Outer::Thing.new\no.x = 1\nnil", &[]), ["o"]);
        // `x.class` with arguments or a block is not `Kernel#class`.
        assert!(owned("o = other.class(:x).new\no.x = 1\nnil", &[]).is_empty());
    }

    #[test]
    fn the_trailing_bare_read_escapes() {
        // `harness/effects-corpus/01_core_origins`'s `owns_what_it_mutates`.
        assert!(owned("buffer = []\nbuffer << 1\nbuffer", &[]).is_empty());
    }

    #[test]
    fn every_escape_position_disqualifies() {
        assert!(owned("b = []\nsink(b)\nnil", &[]).is_empty(), "a call argument");
        assert!(owned("b = []\nreturn b", &[]).is_empty(), "an explicit return");
        assert!(owned("b = []\n[b]\nnil", &[]).is_empty(), "an array element");
        assert!(owned("b = []\n@held = b\nnil", &[]).is_empty(), "an ivar write");
        assert!(owned("b = []\n$g = b\nnil", &[]).is_empty(), "a global write");
        assert!(owned("b = []\n@@c = b\nnil", &[]).is_empty(), "a cvar write");
        assert!(owned("b = []\nC = b\nnil", &[]).is_empty(), "a constant write");
        assert!(owned("b = []\n{k: b}\nnil", &[]).is_empty(), "an assoc value");
        assert!(owned("b = []\nrun(&b)\nnil", &[]).is_empty(), "a block-pass");
        assert!(owned("b = []\ny = b\nnil", &[]).is_empty(), "aliased to a second name");
    }

    #[test]
    fn every_assignment_must_allocate_and_a_parameter_never_does() {
        assert!(owned("b = []\nb = other\nnil", &[]).is_empty(), "one non-allocation is enough");
        assert!(owned("b = []\nb ||= []\nnil", &[]).is_empty(), "an op-write is not one");
        assert!(owned("b = []\nnil", &["b"]).is_empty(), "a parameter is never frame-owned");
    }

    #[test]
    fn the_attribute_writer_shape_is_upstreams() {
        for selector in ["foo=", "_x=", "a1=", "some_name=", "a="] {
            assert!(MutationClassifier::attribute_writer(selector), "{selector}");
        }
        for selector in ["==", "<=", "!=", "===", "=", "Foo=", "a b=", "foo", "[]=", ""] {
            assert!(!MutationClassifier::attribute_writer(selector), "{selector}");
        }
    }

    #[test]
    fn a_typed_mutator_needs_the_class_and_a_universal_one_does_not() {
        assert!(MutationClassifier::mutating("[]=", None));
        assert!(MutationClassifier::mutating("name=", None));
        // `n << 2` is a bit shift and `io << "x"` is output.
        assert!(!MutationClassifier::mutating("<<", None));
        assert!(MutationClassifier::mutating("<<", Some("Array")));
        assert!(MutationClassifier::mutating("<<", Some("String")));
        assert!(!MutationClassifier::mutating("<<", Some("Hash")));
        assert!(MutationClassifier::mutating("store", Some("Hash")));
        assert!(!MutationClassifier::mutating("map", Some("Array")));
        assert!(!MutationClassifier::mutating("push", Some("Integer")));
    }
}
