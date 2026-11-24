//! Upstream v0.4.1 (ADR-119; rigortype/rigor#1594, #1606, #1623, #1631) — the
//! per-file ancestry FACTS whose order or presence the port's direct-statement
//! harvest cannot vouch for.
//!
//! The port's class tables (`Node::ClassDef::{methods, method_visibilities,
//! includes}` and the override index built from them) hold only what a DIRECT
//! statement wrote: a statement of the program, or of the body of a class or
//! module declaration that is itself direct. The reference (since v0.4.1)
//! declines an ancestry-order read wherever something else could reshape the
//! answer, and this census names those things so the port can decline too:
//!
//! * [`AncestryFacts::unpositioned`] — a mixin edge written anywhere but a
//!   direct statement (a conditional, a block, a method, a hook, a `begin`,
//!   a receiver form `X.include(M)`, a `send(:include, M)`, `self.include`, a
//!   computed argument), the reference's `DiscoveryIndex#unpositioned_mixins`
//!   (`MixinAccumulator`). A name the port cannot hold is the WILDCARD `"*"`.
//!   A declaration that is not itself direct (`class C … end if X`, a class
//!   opened inside a block or method) marks none of its body's statements
//!   direct, so its edges are listed and its defs read `possible`.
//! * [`AncestryFacts::possible_methods`] — an instance `def` the port's harvest
//!   did not record (anything but a direct statement or the argument of a
//!   direct visibility modifier). The reference reads such a definer as
//!   `possible` (`Certainty`) and asks past it; the port never sees it, so a
//!   reader must decline on it instead of walking past it to a farther
//!   definer.
//! * [`AncestryFacts::contested_visibilities`] — a visibility slot that may
//!   not hold: a `def` written after a bare `private`/`protected`/`public`/
//!   `module_function` that is not a direct statement (`VisibilityTable`'s
//!   uncertain toggle), or a named `private :x` that is not a direct
//!   statement.
//! * [`AncestryFacts::dirty_owners`] — an owner defining a mixin hook
//!   (`self.included` & co.) or `method_missing`: `Relevance` never reads its
//!   closure as unable to answer a name.
//! * [`AncestryFacts::def_owners`] — owners with any `def`, so a module that
//!   holds only a hook or only conditional defs still counts as a project
//!   entry on a chain.
//! * [`AncestryFacts::direct_mixins`] — the direct `include` / `prepend`
//!   statements (`true` = prepend), each with its argument GROUP, in
//!   statement order: `include B, A` is not `include B; include A` (Ruby
//!   appends a group's arguments last-first, so the first argument ends up
//!   nearest), and the port's flat `includes` list cannot tell them apart.
//!
//! Owners are lists of CANDIDATE qualified names (a declaration has one; a
//! receiver constant `X.include(M)` has one per enclosing lexical scope,
//! longest first); a consumer applies a fact to every candidate, which only
//! ever declines more.
//!
//! Everything here can only make a reader DECLINE; nothing here makes one fire.

use std::collections::HashSet;

use crate::ruby_prism::{self, Node as PrismNode, Visit};

use super::{constant_path_string, constant_string, strict_constant_path_string};

/// The name an owner side lists when an edge was written that the port cannot
/// hold (the reference's `MixinAccumulator::WILDCARD`).
pub const WILDCARD: &str = "*";

/// Upstream v0.4.8 (#2991, `MixinAccumulator::UNNAMED`): the name an owner side
/// also lists when such a call mixes in a module the walk cannot name
/// (`include(*MODS)`, `include m`, `include M, h`, a computed `send` that may be
/// a mixin, a hook's `base.include M`) — listed beside [`WILDCARD`] as the
/// reference's `UNNAMED_LIST` (`["*", "?"]`). An order-only taint (`include M;
/// prepend M`, a `Module.new` argument, an edge whose names are all recorded)
/// lists the wildcard alone and opens nothing.
pub const UNNAMED: &str = "?";

/// Which side of an owner's ancestry a mixin edge reshapes: `include` /
/// `prepend` the instance side, `extend` (and `include` inside
/// `class << self`) the singleton side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MixinSide {
    Include,
    Extend,
}

/// One unpositioned mixin note: `owners` (candidate qualified names), the side,
/// and the module names written (or [`WILDCARD`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnpositionedMixin {
    pub owners: Vec<String>,
    pub side: MixinSide,
    pub names: Vec<String>,
}

/// One written-order instance-side mixin edge. `owners` is the edge's owner
/// resolution input: a resolved `[qualified]` for declaration-frame and
/// receiver-form edges the census could anchor to one name, or the lexical
/// candidates an eval-owner (`X.class_eval { include M }`) resolves at query
/// time against the project's known namespaces. `names` holds only the
/// constant targets — no [`WILDCARD`] / [`UNNAMED`] markers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrittenMixin {
    pub owners: Vec<String>,
    pub prepend: bool,
    pub names: Vec<String>,
    /// `true` for a `Self::<constant>` receiver-form call (`X.include(M)`):
    /// `prepend_call_receiver` resolves the receiver against the FILE's
    /// declared names (`eval_constant_receiver_prefix`), not the project
    /// index — the ingest resolves `owners` to one name with that oracle.
    /// `false` entries resolve via `resolved_write_namespace` /
    /// `known_namespace?` — first project-known candidate else the bare
    /// spelling — which the query side performs on `owners`.
    pub receiver_form: bool,
}

/// See the module docs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AncestryFacts {
    pub unpositioned: Vec<UnpositionedMixin>,
    pub possible_methods: Vec<(String, String)>,
    pub contested_visibilities: Vec<(String, String)>,
    pub dirty_owners: Vec<String>,
    pub direct_mixins: Vec<(String, bool, Vec<String>)>,
    /// rigor-rs#905 — every instance-side `include`/`prepend` edge the
    /// census records, in write order: `direct_mixins`, the named half of
    /// [`AncestryFacts::unpositioned`], and [`AncestryFacts::receiver_mixins`]
    /// interleaved as written. The reference's `MixinAccumulator` writes all
    /// three into ONE per-owner list (`#write_mixin_targets`), and master's
    /// `discovered_includes` reads that merged order — the three separate
    /// tables above each keep their own order and lose the interleave.
    pub written_mixins: Vec<WrittenMixin>,
    /// Owners with any `def` in their body (instance or singleton, recorded or
    /// not) — the reference's `known_user_class?` reads a `def` of either
    /// kind as making a name a project class.
    pub def_owners: Vec<String>,
    /// Upstream v0.4.8 (#2991, `DiscoveryIndex#extend_reshapes_instance?`):
    /// owners defining a hook `extend` runs on the extended module —
    /// `extended` / `extend_object` on its singleton (`SINGLETON_EXTEND_HOOKS`)
    /// or `included` / `append_features` / `prepended` / `prepend_features` /
    /// `inherited` on its instances (`INSTANCE_EXTEND_HOOKS`) — read against
    /// `discovered_def_nodes` / `discovered_singleton_def_nodes`, the def NODE
    /// tables, not a fold's.
    pub extend_hook_owners: Vec<String>,
    /// Upstream v0.4.2 (#1706 / issue #1697) — the module names of every
    /// receiverless `include` written as a DIRECT program statement: `main.include`,
    /// which mixes into `Object` (the reference's `ObjectMixins::TOPLEVEL_INCLUDE_KEY`
    /// edges). A non-constant argument is [`WILDCARD`]. An `include` in a block, a
    /// method, a conditional, a `send(:include, …)` and a top-level `extend` are not
    /// recorded, exactly as the reference's `toplevel_include_owner` leaves them.
    pub toplevel_includes: Vec<String>,
    /// Upstream v0.4.2 (#1700/#1701, `Environment::RequiredFeatures`): the
    /// gated vendored features (only `"prime"`) this file's TEXT requires,
    /// scanned exactly as the reference's `PATTERN` + `commented?` do — see
    /// [`required_vendored_features`].
    pub required_features: Vec<String>,
    /// Upstream v0.4.6 (#2264 / issue #2174, `RequiredFeatures.features_of`'s
    /// `ANY_FEATURE` scan): the FIRST path segment of every feature this
    /// file's text requires — see [`required_feature_segments`].
    pub required_segments: Vec<String>,
    /// Upstream v0.4.5 (#2086 / issue #1722) — every RECEIVER-form instance
    /// mixin `X.include(M)` / `X.prepend(M)` / `X.send(:include, M)`, wherever
    /// it is written: `(owner candidates, names)`, a non-constant argument as
    /// [`WILDCARD`]. The reference records such an edge (unpositioned) under
    /// the class the receiver names and resolves the names from that OWNER,
    /// not from the call site (probed: `Integer.include(Ext)` inside `module
    /// Foo` with `Foo::Ext` reads as an unknown mixin and silences `1.nope`).
    /// The port keeps its wildcard taint for the order readers and lists the
    /// names here for the set-shaped `call.undefined-method` decline only.
    pub receiver_mixins: Vec<(Vec<String>, Vec<String>)>,
    /// Upstream v0.4.6 (#2094 / #2269) — every SINGLETON-side mixin of an
    /// owner, wherever written: `(owner candidates, names)`, where a
    /// spelling the reference cannot record is [`WILDCARD`] (its
    /// `taint(owner, :extend)`). Named: `X.extend(M)`, a
    /// `X.singleton_class.include(M)` / `X.singleton_class.send(:prepend, M)`
    /// with constant arguments, a class-body `extend M` and its `self.` /
    /// send spellings (v0.4.8 #2995). Wildcard: a non-constant argument, a
    /// `send` spelling on a constant receiver, any `send` / `*_eval` /
    /// computed-name naming call on `X.singleton_class` (or a literal-named
    /// one that is not a direct statement), and `singleton_class.include`
    /// inside `class << self`.
    /// Read only by the set-shaped `call.undefined-method` singleton decline,
    /// so the order readers keep their own taints.
    pub singleton_mixins: Vec<(Vec<String>, Vec<String>)>,
    /// Upstream v0.4.6 (#2094, `ObjectMixins.macro_named?`) — `(owner,
    /// name)` for each literal method name (a Symbol / String argument, or a
    /// `def` handed as one) a receiverless or `self.` class-body call that is
    /// not name-neutral passes (`dsl_method :x`, `def_delegators :@t, :x`):
    /// the opaque instance envelope the reference files for it.
    pub macro_names: Vec<(String, String)>,
    /// Upstream v0.4.5 (#2086 / issue #1722) — the reference's
    /// `LexicalMethodSites` singleton-local table for this file.
    pub singleton_locals: super::singleton_locals::SingletonLocals,
    /// Upstream v0.4.6 (#1817 / #2183) — this file's block sites and
    /// definition census for `call.unresolved-toplevel`'s rebinding-block
    /// decline ([`super::rebind_sites`]). Carried here so the AST keeps one
    /// boxed side table.
    pub rebind: super::rebind_sites::RebindFacts,
}

/// `RequiredFeatures::VENDORED_DIRS`' keys: the features whose vendored gem
/// signatures the reference loads only when some analysed file requires them.
pub const GATED_FEATURES: &[&str] = &["prime"];

/// The reference's `RequiredFeatures.features_of`: every [`GATED_FEATURES`]
/// entry `source` names in a `require "x"` / `require('x')` /
/// `Kernel.require "x"` (not `obj.require`, not `require_relative`, not a
/// computed name), skipping a match on a whole-line comment. Textual, so a
/// heredoc, `=begin` block or trailing comment counts, as it does upstream.
pub fn required_vendored_features(source: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for feature in GATED_FEATURES {
        if !source.windows(feature.len()).any(|w| w == feature.as_bytes()) {
            continue;
        }
        let word = b"require";
        let mut i = 0;
        let mut found = false;
        while !found && i + word.len() <= source.len() {
            if &source[i..i + word.len()] != word {
                i += 1;
                continue;
            }
            let at = i;
            i += 1;
            // `(?:(?<![\w.$@])|(?<=Kernel\.))` — the reference scans raw bytes
            // (`.b`), where `\w` is ASCII-only: a byte >= 0x80 is NOT a word
            // character (`p 1 # érequire "prime"` still activates, rigor-rs#463).
            let ok_prefix = at == 0 || {
                let b = source[at - 1];
                !(b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'$' || b == b'@')
                    || source[..at].ends_with(b"Kernel.")
            };
            if !ok_prefix {
                continue;
            }
            let mut j = at + word.len();
            while j < source.len() && (source[j] == b' ' || source[j] == b'\t') {
                j += 1;
            }
            if j < source.len() && source[j] == b'(' {
                j += 1;
                while j < source.len() && (source[j] == b' ' || source[j] == b'\t') {
                    j += 1;
                }
            }
            let Some(&q) = source.get(j) else { continue };
            if q != b'"' && q != b'\'' {
                continue;
            }
            let name_end = j + 1 + feature.len();
            if source.get(j + 1..name_end) != Some(feature.as_bytes()) || source.get(name_end) != Some(&q) {
                continue;
            }
            // `commented?`: the line's first non-blank byte is `#`.
            let line_start = source[..at].iter().rposition(|&b| b == b'\n').map_or(0, |p| p + 1);
            let lead = &source[line_start..at];
            let first = lead.iter().find(|b| !b.is_ascii_whitespace());
            if first != Some(&b'#') {
                found = true;
            }
        }
        if found {
            out.push((*feature).to_string());
        }
    }
    out
}

/// Upstream v0.4.6 (#2264 / issue #2174) — the reference's
/// `RequiredFeatures.features_of` `ANY_FEATURE` scan, reduced to each match's
/// first path segment (`"digest/sha2"` → `"digest"`): `require "x"` /
/// `require('x/y')` / `Kernel.require "x"`, the name `[\w-]+(?:/[\w-]+)*`
/// between matching quotes, skipping a match on a whole-line comment. The
/// regex runs over the source's BYTES, so `\w` is ASCII only and a non-ASCII
/// byte before `require` does not block it. The reference then keeps only a
/// segment Ruby ships (`SHIPPED_SEGMENTS`); the reader of this list
/// intersects with a table that already holds only shipped segments.
pub fn required_feature_segments(source: &[u8]) -> Vec<String> {
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let name = |b: u8| word(b) || b == b'-';
    let kw = b"require";
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i + kw.len() <= source.len() {
        if &source[i..i + kw.len()] != kw {
            i += 1;
            continue;
        }
        let at = i;
        // `(?:(?<![\w.$@])|(?<=Kernel\.))` — the reference scans raw bytes
            // (`.b`), where `\w` is ASCII-only: a byte >= 0x80 is NOT a word
            // character (`p 1 # érequire "prime"` still activates, rigor-rs#463).
        let ok_prefix = at == 0 || {
            let b = source[at - 1];
            !(word(b) || b == b'.' || b == b'$' || b == b'@') || source[..at].ends_with(b"Kernel.")
        };
        if !ok_prefix {
            i += 1;
            continue;
        }
        let mut j = at + kw.len();
        while j < source.len() && (source[j] == b' ' || source[j] == b'\t') {
            j += 1;
        }
        if j < source.len() && source[j] == b'(' {
            j += 1;
            while j < source.len() && (source[j] == b' ' || source[j] == b'\t') {
                j += 1;
            }
        }
        let Some(&q) = source.get(j) else { break };
        if q != b'"' && q != b'\'' {
            i += 1;
            continue;
        }
        let start = j + 1;
        let mut k = start;
        while k < source.len() && name(source[k]) {
            k += 1;
        }
        let first_end = k;
        let mut matched = k > start;
        while matched && source.get(k) == Some(&b'/') {
            let seg = k + 1;
            let mut m = seg;
            while m < source.len() && name(source[m]) {
                m += 1;
            }
            if m == seg {
                matched = false;
            } else {
                k = m;
            }
        }
        if !matched || source.get(k) != Some(&q) {
            i += 1;
            continue;
        }
        // `commented?`: the line's first non-blank byte is `#`.
        let line_start = source[..at].iter().rposition(|&b| b == b'\n').map_or(0, |p| p + 1);
        let first = source[line_start..at].iter().find(|b| !b.is_ascii_whitespace());
        if first != Some(&b'#') {
            let seg = String::from_utf8_lossy(&source[start..first_end]).into_owned();
            if !out.contains(&seg) {
                out.push(seg);
            }
        }
        i = k + 1;
    }
    out
}

/// `ScopeIndexer::NAME_NEUTRAL_MACROS`.
const NAME_NEUTRAL_MACROS: [&str; 13] = [
    "private",
    "public",
    "protected",
    "module_function",
    "private_class_method",
    "public_class_method",
    "private_constant",
    "public_constant",
    "require",
    "require_relative",
    "autoload",
    "autoload_relative",
    // `include` / `prepend` / `extend` take constants, never literal names.
    "include",
];

/// `surface_rewriting_call?`: `SURFACE_EVAL_CALLS`, `SURFACE_NAMING_CALLS`,
/// `SURFACE_SEND_CALLS`, `ATTR_MACROS`, `SURFACE_MIXIN_CALLS` and the
/// `(?:\A|_)(?:include|prepend|extend)(?:_|\z)` helper pattern.
fn surface_rewriting_call(name: &str) -> bool {
    const NAMING: [&str; 5] =
        ["define_method", "define_singleton_method", "alias_method", "remove_method", "undef_method"];
    const ATTR: [&str; 3] = ["attr_reader", "attr_writer", "attr_accessor"];
    EVAL_CALLS.contains(&name)
        || NAMING.contains(&name)
        || SEND_NAMES.contains(&name)
        || ATTR.contains(&name)
        || name.split('_').any(|seg| MIXIN_KINDS.contains(&seg))
}

/// `singleton_class_named_call?`: `alias_method` / `define_method` (first
/// argument), `remove_method` / `attr_*` (every argument), every rewritten
/// argument a literal Symbol or String (a `define_method` body argument
/// aside).
fn singleton_class_named_call(name: &str, args: &[PrismNode<'_>]) -> bool {
    let naming = matches!(
        name,
        "alias_method" | "define_method" | "remove_method" | "attr_reader" | "attr_writer" | "attr_accessor"
    );
    naming
        && !args.is_empty()
        && args.iter().enumerate().all(|(i, a)| {
            (name == "define_method" && i > 0)
                || super::definitions::literal_symbol_or_string_name(a).is_some()
        })
}

const MODIFIERS: [&str; 3] = ["private", "protected", "public"];
const MIXIN_KINDS: [&str; 3] = ["include", "prepend", "extend"];
const SEND_NAMES: [&str; 3] = ["send", "public_send", "__send__"];
const EVAL_CALLS: [&str; 6] =
    ["class_eval", "module_eval", "class_exec", "module_exec", "instance_eval", "instance_exec"];

/// Upstream v0.4.8 (#3007, `MainSelfCalls::SELF_KEEPING_ITERATORS`): the core
/// iterators whose block runs with the caller's `self`, entered on a literal
/// receiver.
const SELF_KEEPING_ITERATORS: [&str; 10] = [
    "each",
    "each_with_index",
    "each_with_object",
    "map",
    "times",
    "upto",
    "downto",
    "tap",
    "then",
    "loop",
];
/// The iterators entered without a receiver: `Kernel#loop`, and `tap` /
/// `then` on `main` itself.
const RECEIVERLESS_ITERATORS: [&str; 3] = ["tap", "then", "loop"];

/// `MainSelfCalls::visit` — the node kinds its descent reaches through: a
/// call nested only in these keeps `main` as its `self`. Any other node
/// (`class`, a lambda, a `return`, …) ends the descent.
fn main_self_control_flow(n: &PrismNode<'_>) -> bool {
    n.as_statements_node().is_some()
        || n.as_begin_node().is_some()
        || n.as_rescue_node().is_some()
        || n.as_else_node().is_some()
        || n.as_ensure_node().is_some()
        || n.as_if_node().is_some()
        || n.as_unless_node().is_some()
        || n.as_case_node().is_some()
        || n.as_case_match_node().is_some()
        || n.as_when_node().is_some()
        || n.as_in_node().is_some()
        || n.as_while_node().is_some()
        || n.as_until_node().is_some()
        || n.as_for_node().is_some()
        || n.as_parentheses_node().is_some()
        || n.as_and_node().is_some()
        || n.as_or_node().is_some()
        || n.as_rescue_modifier_node().is_some()
        || n.as_local_variable_write_node().is_some()
        || n.as_instance_variable_write_node().is_some()
        || n.as_global_variable_write_node().is_some()
        || n.as_constant_write_node().is_some()
        || n.as_multi_write_node().is_some()
        || n.as_arguments_node().is_some()
        || n.as_array_node().is_some()
}

/// `MainSelfCalls::self_keeping_block?` — the call's block runs with `main`:
/// a self-keeping iterator spelled on a literal receiver
/// (`[1].each { … }`, `3.times { … }`), or a receiverless `tap` / `then` /
/// `loop`.
fn main_self_keeping_block(call: &ruby_prism::CallNode<'_>) -> bool {
    let Some(blk) = call.block() else { return false };
    if blk.as_block_node().is_none() {
        return false;
    }
    let name = constant_string(call.name().as_slice());
    if !SELF_KEEPING_ITERATORS.contains(&name.as_str()) {
        return false;
    }
    match call.receiver() {
        None => RECEIVERLESS_ITERATORS.contains(&name.as_str()),
        Some(r) => main_self_literal(&r),
    }
}

/// `MainSelfCalls::literal` — a `(1..2)` parenthesised range counts as its
/// inner literal when the parentheses hold a single statement.
fn main_self_literal(n: &PrismNode<'_>) -> bool {
    let literal = |m: &PrismNode<'_>| {
        m.as_array_node().is_some()
            || m.as_range_node().is_some()
            || m.as_integer_node().is_some()
    };
    if literal(n) {
        return true;
    }
    let Some(p) = n.as_parentheses_node() else { return false };
    let Some(stmts) = p.body().and_then(|b| b.as_statements_node()) else { return false };
    let body: Vec<PrismNode<'_>> = stmts.body().iter().collect();
    body.len() == 1 && literal(&body[0])
}

/// Direct children of `n`, in visit order.
///
/// Control-flow kinds ([`main_self_control_flow`]) use a type-specific
/// child walk that does not descend: a modifier-`if` chain otherwise
/// re-walked each remaining suffix (rigor-rs#424). Prism's `visit_if_node`
/// (and while/begin) flatten the `StatementsNode` into those children;
/// that is sound here because the wrapper is itself a control-flow kind.
/// Every other kind keeps the depth==1 visitor — `CallNode` / `DefNode`
/// / `ParametersNode` are not on the control-flow list, and emptying
/// them dropped mixin-taint writes inside blocks (`send(m, …)`).
fn compact_children<'pr>(n: &PrismNode<'pr>) -> Vec<PrismNode<'pr>> {
    if main_self_control_flow(n) {
        struct Direct<'pr> {
            out: Vec<PrismNode<'pr>>,
        }
        impl<'pr> Visit<'pr> for Direct<'pr> {
            fn visit(&mut self, node: &PrismNode<'pr>) {
                self.out.push(node_copy(node));
            }
        }
        let mut c = Direct { out: Vec::new() };
        walk_compact_root(&mut c, n);
        c.out
    } else {
        compact_children_visit(n)
    }
}

/// Master's depth==1 visitor: walk the whole subtree, keep only the
/// nodes entered at depth 1. Used for kinds [`walk_compact_root`] does
/// not list.
fn compact_children_visit<'pr>(n: &PrismNode<'pr>) -> Vec<PrismNode<'pr>> {
    struct Children<'pr> {
        depth: usize,
        out: Vec<PrismNode<'pr>>,
    }
    impl<'pr> Visit<'pr> for Children<'pr> {
        fn visit_branch_node_enter(&mut self, node: PrismNode<'pr>) {
            if self.depth == 1 {
                self.out.push(node);
            }
            self.depth += 1;
        }
        fn visit_branch_node_leave(&mut self) {
            self.depth -= 1;
        }
        fn visit_leaf_node_enter(&mut self, node: PrismNode<'pr>) {
            if self.depth == 1 {
                self.out.push(node);
            }
            self.depth += 1;
        }
        fn visit_leaf_node_leave(&mut self) {
            self.depth -= 1;
        }
    }
    let mut c = Children {
        depth: 0,
        out: Vec::new(),
    };
    c.visit(n);
    c.out
}

/// Drive Prism's per-type child visitor on `n` so [`compact_children`]'s
/// `visit` sees only the compact (direct) children.
fn walk_compact_root<'pr>(v: &mut impl Visit<'pr>, n: &PrismNode<'pr>) {
    if let Some(node) = n.as_statements_node() {
        ruby_prism::visit_statements_node(v, &node);
    } else if let Some(node) = n.as_begin_node() {
        ruby_prism::visit_begin_node(v, &node);
    } else if let Some(node) = n.as_rescue_node() {
        ruby_prism::visit_rescue_node(v, &node);
    } else if let Some(node) = n.as_else_node() {
        ruby_prism::visit_else_node(v, &node);
    } else if let Some(node) = n.as_ensure_node() {
        ruby_prism::visit_ensure_node(v, &node);
    } else if let Some(node) = n.as_if_node() {
        ruby_prism::visit_if_node(v, &node);
    } else if let Some(node) = n.as_unless_node() {
        ruby_prism::visit_unless_node(v, &node);
    } else if let Some(node) = n.as_case_node() {
        ruby_prism::visit_case_node(v, &node);
    } else if let Some(node) = n.as_case_match_node() {
        ruby_prism::visit_case_match_node(v, &node);
    } else if let Some(node) = n.as_when_node() {
        ruby_prism::visit_when_node(v, &node);
    } else if let Some(node) = n.as_in_node() {
        ruby_prism::visit_in_node(v, &node);
    } else if let Some(node) = n.as_while_node() {
        ruby_prism::visit_while_node(v, &node);
    } else if let Some(node) = n.as_until_node() {
        ruby_prism::visit_until_node(v, &node);
    } else if let Some(node) = n.as_for_node() {
        ruby_prism::visit_for_node(v, &node);
    } else if let Some(node) = n.as_parentheses_node() {
        ruby_prism::visit_parentheses_node(v, &node);
    } else if let Some(node) = n.as_and_node() {
        ruby_prism::visit_and_node(v, &node);
    } else if let Some(node) = n.as_or_node() {
        ruby_prism::visit_or_node(v, &node);
    } else if let Some(node) = n.as_rescue_modifier_node() {
        ruby_prism::visit_rescue_modifier_node(v, &node);
    } else if let Some(node) = n.as_local_variable_write_node() {
        ruby_prism::visit_local_variable_write_node(v, &node);
    } else if let Some(node) = n.as_instance_variable_write_node() {
        ruby_prism::visit_instance_variable_write_node(v, &node);
    } else if let Some(node) = n.as_global_variable_write_node() {
        ruby_prism::visit_global_variable_write_node(v, &node);
    } else if let Some(node) = n.as_constant_write_node() {
        ruby_prism::visit_constant_write_node(v, &node);
    } else if let Some(node) = n.as_multi_write_node() {
        ruby_prism::visit_multi_write_node(v, &node);
    } else if let Some(node) = n.as_arguments_node() {
        ruby_prism::visit_arguments_node(v, &node);
    } else if let Some(node) = n.as_array_node() {
        ruby_prism::visit_array_node(v, &node);
    }
}

/// `MainSelfCalls.collect(body)` (upstream v0.4.8 #3007) — the span keys of
/// every call whose `self` is `main` as far as the mixin walk can tell: a
/// top-level statement's call, or one reached through control flow or a
/// value position, or through the body of a top-level `def` / `def self.`,
/// or the block of a self-keeping iterator on a literal receiver. A
/// `def obj.f` or `class` body, a non-entered block and everything under
/// them stay out.
fn collect_main_self_calls(body: &PrismNode<'_>) -> HashSet<(usize, usize)> {
    let mut out = HashSet::new();
    main_self_visit(body, &mut out);
    out
}

fn main_self_visit(n: &PrismNode<'_>, out: &mut HashSet<(usize, usize)>) {
    if let Some(call) = n.as_call_node() {
        out.insert(span_key(n));
        if let Some(r) = call.receiver() {
            main_self_visit(&r, out);
        }
        if let Some(a) = call.arguments() {
            main_self_visit(&a.as_node(), out);
        }
        if main_self_keeping_block(&call) {
            if let Some(b) = call
                .block()
                .and_then(|b| b.as_block_node())
                .and_then(|b| b.body())
            {
                main_self_visit(&b, out);
            }
        }
        return;
    }
    if let Some(def) = n.as_def_node() {
        if def.receiver().is_none_or(|r| r.as_self_node().is_some()) {
            if let Some(b) = def.body() {
                main_self_visit(&b, out);
            }
        }
        return;
    }
    if !main_self_control_flow(n) {
        return;
    }
    for c in compact_children(n) {
        main_self_visit(&c, out);
    }
}
const HOOK_DEFS: [&str; 4] = ["included", "extended", "prepended", "inherited"];
/// `Relevance::HOOKS` — every singleton hook Ruby calls while a module enters
/// another's ancestry.
const ALL_HOOKS: [&str; 7] = [
    "included",
    "extended",
    "prepended",
    "inherited",
    "append_features",
    "extend_object",
    "prepend_features",
];

fn span_key(n: &PrismNode<'_>) -> (usize, usize) {
    let l = n.location();
    (l.start_offset(), l.end_offset())
}

/// A fresh handle to `n`: the `Node` enum is a plain `{parser, pointer,
/// marker}` triple — POD, no `Drop` and no `Copy`/`Clone` impl — so a bitwise
/// copy is a second handle to the same parse-owned node, exactly what
/// `Node::as_node()` returns for the typed nodes.
fn node_copy<'pr>(n: &PrismNode<'pr>) -> PrismNode<'pr> {
    // SAFETY: `Node` is an enum of raw pointers and `PhantomData` only; it
    // owns nothing (the parse result owns the arena), so duplicating it
    // bitwise cannot double-free or alias mutably.
    unsafe { std::ptr::read(n) }
}

/// `opaque_mixin_argument?` — an argument that names no module: not a
/// constant path, and not the `Module.new` argument that keeps its #2975
/// reading (`include Module.new { def helper; end; end }` is positioned).
fn opaque_mixin_argument(a: &PrismNode<'_>) -> bool {
    constant_path_string(a).is_empty() && !module_new_call(a)
}

/// `module_new_call?` — `Module.new` / `Module.new { … }` (any `new` receiver
/// spelling resolves to the `Module` constant).
fn module_new_call(n: &PrismNode<'_>) -> bool {
    let Some(c) = n.as_call_node() else { return false };
    constant_string(c.name().as_slice()) == "new"
        && c.receiver().is_some_and(|r| constant_path_string(&r) == "Module")
}

/// `literal_send_name` — the method name a `send` argument spells: a name
/// literal's, or `Some("")` — no mixin's — for an interpolation with a literal
/// part no mixin name contains. `None` when it may spell any name.
fn literal_send_name(n: &PrismNode<'_>) -> Option<String> {
    if let Some(s) = n.as_symbol_node() {
        return Some(String::from_utf8_lossy(s.unescaped()).into_owned());
    }
    if let Some(s) = n.as_string_node() {
        return Some(String::from_utf8_lossy(s.unescaped()).into_owned());
    }
    let parts = if let Some(s) = n.as_interpolated_string_node() {
        s.parts().iter().collect::<Vec<_>>()
    } else if let Some(s) = n.as_interpolated_symbol_node() {
        s.parts().iter().collect::<Vec<_>>()
    } else {
        return None;
    };
    let excluded = parts.iter().any(|p| {
        p.as_string_node().is_some_and(|s| {
            let lit = String::from_utf8_lossy(s.unescaped()).into_owned();
            MIXIN_KINDS.iter().all(|k| !k.contains(lit.as_str()))
        })
    });
    if excluded {
        Some(String::new())
    } else {
        None
    }
}

/// `LOCAL_SCOPE_BOUNDARIES` — a nested `def` or declaration opens a scope of
/// its own: its writes never bind the outer local.
fn local_scope_boundary(n: &PrismNode<'_>) -> bool {
    n.as_def_node().is_some()
        || n.as_class_node().is_some()
        || n.as_module_node().is_some()
        || n.as_singleton_class_node().is_some()
}

/// The bare name a parameter / local-variable write / block-local node binds,
/// when it has one — `local_binding?`'s `node.respond_to?(:name)`.
fn binding_name(n: &PrismNode<'_>) -> Option<String> {
    // *ParameterNode (an anonymous `*`, `**` or `&` has none).
    if let Some(x) = n.as_required_parameter_node() {
        return Some(constant_string(x.name().as_slice()));
    }
    if let Some(x) = n.as_optional_parameter_node() {
        return Some(constant_string(x.name().as_slice()));
    }
    if let Some(x) = n.as_rest_parameter_node() {
        return x.name().map(|nm| constant_string(nm.as_slice()));
    }
    if let Some(x) = n.as_required_keyword_parameter_node() {
        return Some(constant_string(x.name().as_slice()));
    }
    if let Some(x) = n.as_optional_keyword_parameter_node() {
        return Some(constant_string(x.name().as_slice()));
    }
    if let Some(x) = n.as_keyword_rest_parameter_node() {
        return x.name().map(|nm| constant_string(nm.as_slice()));
    }
    if let Some(x) = n.as_block_parameter_node() {
        return x.name().map(|nm| constant_string(nm.as_slice()));
    }
    // LocalVariable* (the read is a `local_binding?` early return, not here).
    if let Some(x) = n.as_local_variable_write_node() {
        return Some(constant_string(x.name().as_slice()));
    }
    if let Some(x) = n.as_local_variable_target_node() {
        return Some(constant_string(x.name().as_slice()));
    }
    if let Some(x) = n.as_local_variable_operator_write_node() {
        return Some(constant_string(x.name().as_slice()));
    }
    if let Some(x) = n.as_local_variable_and_write_node() {
        return Some(constant_string(x.name().as_slice()));
    }
    if let Some(x) = n.as_local_variable_or_write_node() {
        return Some(constant_string(x.name().as_slice()));
    }
    // `Prism::BlockLocalVariableNode` — the `|k; shadow|` list's names.
    if let Some(x) = n.as_block_local_variable_node() {
        return Some(constant_string(x.name().as_slice()));
    }
    None
}

/// `local_binding?(node, local)` — a write, pattern target, parameter or
/// block-local named `local`.
fn local_binding(n: &PrismNode<'_>, local: &str) -> bool {
    if n.as_local_variable_read_node().is_some() {
        return false;
    }
    binding_name(n).is_some_and(|name| name == local)
}

/// `local_send_name_literal(node, local)` — `Some(name)` when `node` binds
/// `local` to a name literal, `Err(())` for a binding the walk cannot read,
/// `None` when `node` is no binding of `local`.
fn local_send_name_literal(n: &PrismNode<'_>, local: &str) -> Option<Result<String, ()>> {
    if let Some(w) = n.as_local_variable_write_node() {
        if constant_string(w.name().as_slice()) == local {
            return Some(match literal_send_name(&w.value()) {
                Some(name) => Ok(name),
                None => Err(()),
            });
        }
        return None;
    }
    if local_binding(n, local) {
        return Some(Err(()));
    }
    None
}

/// `iterated_element_names(node, local, scope_nodes)` — the literal names the
/// literal elements of the receiver of an `each` / `each_with_index` / `map`
/// iterator spell, when `node` is such a call whose first block parameter is
/// `local`: `Some(names)`, `Some(err)` when the receiver or an element names
/// nothing readable (`:unknown`), `None` when `node` is not that iterator
/// (#3024).
fn iterated_element_names<'pr>(
    n: &PrismNode<'pr>,
    local: &str,
    scope_nodes: &[PrismNode<'pr>],
) -> Option<Result<Vec<String>, ()>> {
    let elements = iterated_element_source(n, local, scope_nodes)?;
    let Some(elements) = elements else {
        return Some(Err(()));
    };
    let mut names = Vec::with_capacity(elements.len());
    for e in &elements {
        match literal_send_name(e) {
            Some(name) => names.push(name),
            None => return Some(Err(())),
        }
    }
    Some(Ok(names))
}

/// `ARRAY_ITERATORS` — the iterators whose first block parameter #3024 reads
/// as the elements: `each`, `each_with_index`, `map`.
const ARRAY_ITERATORS: [&str; 3] = ["each", "each_with_index", "map"];

/// `iterated_element_source(node, local, scope_nodes)` — `Some(Some(elements))`
/// for the literal-element receiver, `Some(None)` (`:unknown`) for an
/// iterator whose receiver names nothing readable, `None` when `node` is not
/// the `[…].each { |local| … }` shape at all.
fn iterated_element_source<'pr>(
    n: &PrismNode<'pr>,
    local: &str,
    scope_nodes: &[PrismNode<'pr>],
) -> Option<Option<Vec<PrismNode<'pr>>>> {
    let call = n.as_call_node()?;
    let name = constant_string(call.name().as_slice());
    if !ARRAY_ITERATORS.contains(&name.as_str()) {
        return None;
    }
    let block = call.block().and_then(|b| b.as_block_node())?;
    if !iterated_first_parameter(&block, local) {
        return None;
    }
    Some(iterated_receiver_elements(&call, scope_nodes))
}

/// `iterated_first_parameter?(block, local)` — the block's first required
/// parameter is `local` and no other parameter binds it.
fn iterated_first_parameter(block: &ruby_prism::BlockNode<'_>, local: &str) -> bool {
    let Some(params) = block
        .parameters()
        .and_then(|p| p.as_block_parameters_node())
        .and_then(|p| p.parameters())
    else {
        return false;
    };
    let Some(first) = params
        .requireds()
        .iter()
        .next()
        .and_then(|r| r.as_required_parameter_node())
    else {
        return false;
    };
    if constant_string(first.name().as_slice()) != local {
        return false;
    }
    // `Source::NodeWalker.each(parameters)` — every descendant of the
    // ParametersNode — must bind no other `local`.
    !descendants(&params.as_node())
        .iter()
        .any(|p| span_key(p) != span_key(&first.as_node()) && local_binding(p, local))
}

/// `iterated_receiver_elements` — the literal elements of the receiver: an
/// `[…]` literal's, or the elements a plain `KINDS = […]` constant write in
/// scope gives a `ConstantReadNode` receiver. `None` (`:unknown`) otherwise.
fn iterated_receiver_elements<'pr>(
    call: &ruby_prism::CallNode<'pr>,
    scope_nodes: &[PrismNode<'pr>],
) -> Option<Vec<PrismNode<'pr>>> {
    let recv = call.receiver()?;
    if let Some(arr) = recv.as_array_node() {
        return Some(arr.elements().iter().collect());
    }
    let c = recv.as_constant_read_node()?;
    constant_array_elements(&constant_string(c.name().as_slice()), scope_nodes)
}

/// `constant_array_elements(name, nodes)` — the `[…]` elements of the single
/// plain `NAME = […]` write in scope.
fn constant_array_elements<'pr>(name: &str, nodes: &[PrismNode<'pr>]) -> Option<Vec<PrismNode<'pr>>> {
    let mut writes: Vec<PrismNode<'pr>> = Vec::new();
    for n in nodes {
        constant_write_nodes(n, name, &mut writes);
    }
    if writes.len() != 1 {
        return None;
    }
    let value = writes[0].as_constant_write_node()?.value();
    Some(value.as_array_node()?.elements().iter().collect())
}

/// `constant_write_nodes(nodes, name)` — every constant binding of `name`
/// (write / pattern target / op-assignment) below `nodes`, skipping nested
/// scopes (`LOCAL_SCOPE_BOUNDARIES`).
fn constant_write_nodes<'pr>(n: &PrismNode<'pr>, name: &str, out: &mut Vec<PrismNode<'pr>>) {
    if local_scope_boundary(n) {
        return;
    }
    if constant_binding(n, name) {
        out.push(node_copy(n));
    }
    for c in compact_children(n) {
        constant_write_nodes(&c, name, out);
    }
}

/// `constant_binding?(node, name)` — a `Prism::Constant*` node named `name`,
/// minus `ConstantPath*` and `ConstantReadNode`: write, target and
/// op-assignment forms.
fn constant_binding(n: &PrismNode<'_>, name: &str) -> bool {
    if let Some(x) = n.as_constant_write_node() {
        return constant_string(x.name().as_slice()) == name;
    }
    if let Some(x) = n.as_constant_target_node() {
        return constant_string(x.name().as_slice()) == name;
    }
    if let Some(x) = n.as_constant_operator_write_node() {
        return constant_string(x.name().as_slice()) == name;
    }
    if let Some(x) = n.as_constant_and_write_node() {
        return constant_string(x.name().as_slice()) == name;
    }
    if let Some(x) = n.as_constant_or_write_node() {
        return constant_string(x.name().as_slice()) == name;
    }
    false
}

/// Every descendant of `n` in visit order (`Source::NodeWalker.each`).
fn descendants<'pr>(n: &PrismNode<'pr>) -> Vec<PrismNode<'pr>> {
    let mut out = Vec::new();
    for c in compact_children(n) {
        out.push(node_copy(&c));
        out.extend(descendants(&c));
    }
    out
}

/// `iterated_element_children` — the receiver, the arguments and the block's
/// body: everything but the block's parameters (the first is `local` bound to
/// the elements; the rest bind other names).
fn iterated_element_children<'pr>(call: &ruby_prism::CallNode<'pr>) -> Vec<PrismNode<'pr>> {
    let mut out = Vec::new();
    if let Some(r) = call.receiver() {
        out.push(r);
    }
    if let Some(a) = call.arguments() {
        out.push(a.as_node());
    }
    if let Some(b) = call.block().and_then(|b| b.as_block_node()).and_then(|b| b.body()) {
        out.push(b);
    }
    out
}

/// `local_send_names` — the names `local`'s writes in `scope` bind it to, or
/// `None` when the argument is not a plain local read, a binding in scope is
/// not a readable literal write, or no write names it at all — any of which
/// leaves the call's name computed. Iterated `local` elements resolve through
/// literal element names (#3024).
fn local_send_names<'pr>(arg: &PrismNode<'pr>, scope: &PrismNode<'pr>) -> Option<Vec<String>> {
    let local = constant_string(arg.as_local_variable_read_node()?.name().as_slice());
    // `root` — a `def` contributes its parameter bindings too; its
    // `compact_child_nodes` are the parameters, the body's statements and the
    // locals written among them.
    let root: Vec<PrismNode<'pr>> = if scope.as_def_node().is_some() {
        compact_children(scope)
    } else {
        vec![node_copy(scope)]
    };
    let mut names = Vec::new();
    if !collect_local_send_names(&root, &local, &mut names, &root) {
        return None;
    }
    if names.is_empty() {
        None
    } else {
        Some(names)
    }
}

/// `collect_local_send_names` — gathers the literal names `local` is bound to
/// across `nodes`; `false` on a binding or element the walk cannot read.
fn collect_local_send_names<'pr>(
    nodes: &[PrismNode<'pr>],
    local: &str,
    names: &mut Vec<String>,
    scope_nodes: &[PrismNode<'pr>],
) -> bool {
    nodes.iter().all(|n| {
        if local_scope_boundary(n) {
            return true;
        }
        match iterated_element_names(n, local, scope_nodes) {
            Some(Ok(iter_names)) => {
                names.extend(iter_names);
                if let Some(call) = n.as_call_node() {
                    return collect_local_send_names(
                        &iterated_element_children(&call),
                        local,
                        names,
                        scope_nodes,
                    );
                }
                return true;
            }
            Some(Err(())) => return false,
            None => {}
        }
        match local_send_name_literal(n, local) {
            Some(Ok(name)) => names.push(name),
            Some(Err(())) => return false,
            None => {}
        }
        collect_local_send_names(&compact_children(n), local, names, scope_nodes)
    })
}

/// `MainSelfCalls.collect(body, instance_defs: false)` — the `{call => scope
/// node}` map of one class body: every call the body's own `self` answers —
/// a call reached through `main_self_control_flow`, any method call's
/// `{…}` block, and `def self.` bodies (their locals are the scope). Other
/// `def`s open a scope the map does not enter; a lambda, `class`/`module`,
/// `return`, `defined?` or `rescue` body is not descended into.
fn collect_class_body_calls<'pr>(
    body: &PrismNode<'pr>,
) -> std::collections::HashMap<(usize, usize), PrismNode<'pr>> {
    let mut out = std::collections::HashMap::new();
    class_body_visit(body, body, &mut out);
    out
}

fn class_body_visit<'pr>(
    n: &PrismNode<'pr>,
    scope: &PrismNode<'pr>,
    out: &mut std::collections::HashMap<(usize, usize), PrismNode<'pr>>,
) {
    if let Some(call) = n.as_call_node() {
        out.insert(span_key(n), node_copy(scope));
        if let Some(r) = call.receiver() {
            class_body_visit(&r, scope, out);
        }
        if let Some(a) = call.arguments() {
            class_body_visit(&a.as_node(), scope, out);
        }
        // `any_block?` — every method-call block keeps the body's `self` (a
        // `->` lambda is a LambdaNode, not a BlockNode).
        if let Some(b) = call.block().and_then(|b| b.as_block_node()).and_then(|b| b.body()) {
            class_body_visit(&b, scope, out);
        }
        return;
    }
    if let Some(def) = n.as_def_node() {
        // `instance_defs: false` — only `def self.` keeps the class's `self`;
        // its locals are the scope.
        if def.receiver().is_some_and(|r| r.as_self_node().is_some()) {
            if let Some(b) = def.body() {
                class_body_visit(&b, n, out);
            }
        }
        return;
    }
    if !main_self_control_flow(n) {
        return;
    }
    for c in compact_children(n) {
        class_body_visit(&c, scope, out);
    }
}

/// One owner frame: the candidate names (`None` = an owner the walk cannot
/// name, which records nothing), whether `self` is the singleton, and the
/// running visibility default's uncertainty.
struct Frame {
    owner: Option<Vec<String>>,
    singleton: bool,
    uncertain: bool,
    /// Upstream v0.4.6 (#2269 / #2094, `walk_eval_extends_call`): the class
    /// an `instance_eval` / `instance_exec` block's `self` names, where
    /// `owner` stays `None` (its `def`s bind on the singleton). The
    /// reference's extends walk opens that block as the receiver's own body,
    /// exactly as `class_eval`, so `X.instance_eval { extend M }` files `M`
    /// on `X`'s singleton side. Read only by [`Census::self_owner`].
    eval_owner: Option<Vec<String>>,
}

struct Census<'pr> {
    out: AncestryFacts,
    /// The lexical class/module prefix (constant resolution only).
    prefix: Vec<String>,
    frames: Vec<Frame>,
    /// Spans of the DIRECT statements: program statements and statements of
    /// direct declaration bodies.
    direct: HashSet<(usize, usize)>,
    /// Spans of the `def` arguments of a direct visibility modifier
    /// (`private def x`), which the port's harvest records.
    recorded_arg_defs: HashSet<(usize, usize)>,
    /// `def` nesting: inside a method body nothing is recorded but mixin calls.
    in_def: usize,
    /// The parameter names of the enclosing `self.included(base)`-style hooks.
    hook_params: Vec<String>,
    /// Per owner (first candidate), the direct include / prepend targets — a
    /// target written both ways is one body the tables cannot represent.
    direct_includes: std::collections::HashMap<String, (Vec<String>, Vec<String>)>,
    /// Upstream v0.4.8 (#3007, `MixinAccumulator#program_statement?` —
    /// `MainSelfCalls.collect`): the call spans whose `self` is `main`, so a
    /// lexical-self `include` among them is `main.include` (`Object.include`).
    main_self: HashSet<(usize, usize)>,
    /// Upstream v0.4.8 (#3006, `MixinAccumulator#class_bodies`): the class /
    /// module / `class << self` bodies whose calls [`Census::class_body_scope`]
    /// answers for.
    class_bodies: Vec<PrismNode<'pr>>,
    /// `MixinAccumulator#class_body_calls` — `{call span => scope node}` for
    /// each registered body, extended one body per question.
    class_body_calls: std::collections::HashMap<(usize, usize), PrismNode<'pr>>,
    class_bodies_read: usize,
}

impl<'pr> Census<'pr> {
    fn frame(&self) -> &Frame {
        self.frames.last().expect("root frame")
    }

    fn owner(&self) -> Option<Vec<String>> {
        self.frame().owner.clone()
    }

    /// The class this frame's `self` names: the declaration/eval owner,
    /// else the `instance_eval` receiver ([`Frame::eval_owner`]) —
    /// `X.instance_eval { include(*MODS) }` opens `X`'s instance side. The
    /// reference's `current_class` carries that receiver through the same
    /// walks; the port splits it into [`Frame::owner`] + [`Frame::eval_owner`]
    /// and re-joins them here.
    fn self_owner(&self) -> Option<Vec<String>> {
        let f = self.frame();
        f.owner.clone().or_else(|| f.eval_owner.clone())
    }

    fn is_direct(&self, n: &PrismNode<'_>) -> bool {
        self.in_def == 0 && self.direct.contains(&span_key(n))
    }

    fn mark_direct_body(&mut self, body: Option<&PrismNode<'_>>) {
        if let Some(stmts) = body.and_then(|b| b.as_statements_node()) {
            for s in stmts.body().iter() {
                self.direct.insert(span_key(&s));
            }
        }
    }

    fn unpositioned(&mut self, owners: Vec<String>, side: MixinSide, names: Vec<String>) {
        if owners.is_empty() {
            return;
        }
        self.out.unpositioned.push(UnpositionedMixin { owners, side, names });
    }

    fn wildcard(&mut self, owners: Vec<String>, side: MixinSide) {
        self.unpositioned(owners, side, vec![WILDCARD.to_string()]);
    }

    /// `MixinAccumulator#taint(owner, side, unnamed: true)` — the side's order
    /// AND its module set are unknown: the [`UNNAMED`] marker rides beside the
    /// [`WILDCARD`] exactly as the reference's `UNNAMED_LIST` lists both.
    fn unnamed(&mut self, owners: Vec<String>, side: MixinSide) {
        self.unpositioned(
            owners,
            side,
            vec![WILDCARD.to_string(), UNNAMED.to_string()],
        );
    }

    /// The candidate qualified names a constant `raw` denotes from the current
    /// lexical prefix, longest first.
    fn candidates(&self, raw: &str) -> Vec<String> {
        let raw = raw.strip_prefix("::").unwrap_or(raw);
        let mut out = Vec::new();
        for i in (0..=self.prefix.len()).rev() {
            if i == 0 {
                out.push(raw.to_string());
            } else {
                out.push(format!("{}::{}", self.prefix[..i].join("::"), raw));
            }
        }
        out
    }

    fn qualify(&self, name: &str) -> String {
        if self.prefix.is_empty() {
            name.to_string()
        } else {
            format!("{}::{}", self.prefix.join("::"), name)
        }
    }

    fn hook_receiver(&self, recv: &PrismNode<'_>) -> bool {
        if let Some(lv) = recv.as_local_variable_read_node() {
            let name = constant_string(lv.name().as_slice());
            return self.hook_params.contains(&name);
        }
        if let Some(call) = recv.as_call_node() {
            if call.arguments().is_none() {
                if let Some(r) = call.receiver() {
                    return self.hook_receiver(&r);
                }
            }
        }
        false
    }

    fn declaration(&mut self, n: &PrismNode<'_>, name: String, body: Option<PrismNode<'pr>>) {
        let direct = self.is_direct(n);
        if name.is_empty() {
            // An unnameable declaration owns nothing the port can hold.
            let f = Frame { owner: None, singleton: false, uncertain: false, eval_owner: None };
            self.frames.push(f);
            if let Some(b) = &body {
                self.visit(b);
            }
            self.frames.pop();
            return;
        }
        // `MixinAccumulator#class_body(node.body)` — the body's locals are the
        // scope a computed `send` in it resolves through.
        if let Some(b) = &body {
            self.class_bodies.push(node_copy(b));
        }
        let qualified = self.qualify(&name);
        // A declaration that is not itself direct (`class C … end if X`) marks
        // no statement of its body direct: its edges are listed unpositioned
        // and its defs read `possible`, as the reference reads them.
        if direct {
            self.mark_direct_body(body.as_ref());
        }
        let saved_prefix = std::mem::replace(
            &mut self.prefix,
            qualified.split("::").map(str::to_string).collect(),
        );
        let saved_def = std::mem::replace(&mut self.in_def, 0);
        let saved_hooks = std::mem::take(&mut self.hook_params);
        self.frames.push(Frame {
            owner: Some(vec![qualified]),
            singleton: false,
            uncertain: false,
            eval_owner: None,
        });
        if let Some(b) = &body {
            self.visit(b);
        }
        self.frames.pop();
        self.hook_params = saved_hooks;
        self.in_def = saved_def;
        self.prefix = saved_prefix;
    }

    /// `[kind, arguments, via_send]` of a mixin call (the reference's
    /// `mixin_call_view`).
    fn mixin_view(
        &self,
        call: &ruby_prism::CallNode<'pr>,
    ) -> Option<(&'static str, Vec<PrismNode<'pr>>, bool)> {
        let name = constant_string(call.name().as_slice());
        let mut args: Vec<PrismNode<'pr>> =
            call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        if let Some(k) = MIXIN_KINDS.iter().find(|k| **k == name) {
            return Some((k, args, false));
        }
        if !SEND_NAMES.contains(&name.as_str()) {
            return None;
        }
        let first = args.first()?;
        let lit = if let Some(s) = first.as_symbol_node() {
            String::from_utf8_lossy(s.unescaped()).into_owned()
        } else if let Some(s) = first.as_string_node() {
            String::from_utf8_lossy(s.unescaped()).into_owned()
        } else {
            return None;
        };
        let k = MIXIN_KINDS.iter().find(|k| **k == lit)?;
        args.remove(0);
        Some((k, args, true))
    }

    fn record_mixin(&mut self, n: &PrismNode<'pr>, call: &ruby_prism::CallNode<'pr>) {
        let Some((kind, args, via_send)) = self.mixin_view(call) else {
            self.taint_computed_send(n, call);
            return;
        };
        let names: Vec<Option<String>> =
            args.iter().map(strict_constant_path_string).collect();
        let complete = !names.is_empty() && names.iter().all(Option::is_some);
        let names: Vec<String> = names.into_iter().flatten().collect();
        let recv = call.receiver();
        let call_name = constant_string(call.name().as_slice());
        let self_recv = recv.as_ref().is_some_and(|r| r.as_self_node().is_some());
        let lexical_self = recv.is_none() || self_recv;
        // Upstream v0.4.8 `toplevel_include_owner` (#2995 / #3004 / #3007): a
        // lexical-self `include` — every spelling but `public_send`, which
        // cannot reach `main`'s private `include` — that `MainSelfCalls`
        // reaches is `main.include`, filed under the toplevel owner as
        // `Object.include`.
        if lexical_self
            && kind == "include"
            && call_name != "public_send"
            && self.frames.len() == 1
            && self.main_self.contains(&span_key(n))
        {
            // Upstream `instance_mixin_recorded?` writes the constant
            // targets BEFORE the partial-name `note(complete: false)` —
            // `include Mk, h` records `Mk` (so `1.mk` declines). The
            // unnamed half taints only the edge's ORDER — it lives on
            // `unpositioned`, never in the include set, so it suppresses
            // nothing: `2.unknown_thing` still fires, and `helper`'s own
            // `unresolved-toplevel` is not excused by the include
            // (oracle-measured). No WILDCARD is pushed.
            self.out.toplevel_includes.extend(names.iter().cloned());
            return;
        }
        if lexical_self {
            let singleton = self.frame().singleton;
            let side = match (singleton, kind) {
                (true, "extend") => return,
                (true, _) | (false, "extend") => MixinSide::Extend,
                _ => MixinSide::Include,
            };
            if side == MixinSide::Extend {
                if let Some(owner) = self.self_owner() {
                    // `self` as an argument names the owner itself.
                    // Upstream v0.4.8 `singleton_mixin_recorded?` +
                    // `record_extend_targets` (#2995): a lexical-self call in
                    // ANY spelling records its constant targets; an argument
                    // the walk cannot name leaves them UNNAMED — the port's
                    // WILDCARD, plus `?` whenever the argument list is not
                    // fully named (`record_extend_targets`'s
                    // `note(complete: false)` → `taint(unnamed: true)`).
                    let all_named = !args.is_empty()
                        && args.iter().all(|a| {
                            strict_constant_path_string(a).is_some() || a.as_self_node().is_some()
                        });
                    let mut listed: Vec<String> = args
                        .iter()
                        .filter_map(|a| {
                            if a.as_self_node().is_some() {
                                Some(owner[0].clone())
                            } else {
                                strict_constant_path_string(a)
                            }
                        })
                        .collect();
                    if !all_named {
                        listed.push(WILDCARD.to_string());
                        if !args.is_empty() {
                            listed.push(UNNAMED.to_string());
                        }
                    }
                    self.out.singleton_mixins.push((owner, listed));
                }
            }
            let Some(owner) = self.self_owner() else { return };
            // Upstream v0.4.8 `instance_mixin_recorded?` (#2995): a
            // lexical-self call — a bare `include M`, `self.include M`, or a
            // `send` / `public_send` / `__send__` spelling — records its
            // constant targets wherever it is written; a call naming none
            // taints the side, a partially named one records what it can and
            // taints the rest (UNNAMED, the port's `?`).
            if names.is_empty() {
                // `taint_unrecorded_mixin`: a lexical `self` is never a hook
                // parameter or `singleton_class`, so `unnamed` is `opaque` on
                // either side — an argument that names no module
                // (`opaque_mixin_argument?`: not a constant, and not the
                // `Module.new` argument that keeps its #2975 reading).
                if args.iter().any(|a| opaque_mixin_argument(a)) {
                    self.unnamed(owner, side);
                } else {
                    self.wildcard(owner, side);
                }
                return;
            }
            if self.is_direct(n) && !singleton {
                if kind != "extend" {
                    let key = owner[0].clone();
                    let entry = self.direct_includes.entry(key.clone()).or_default();
                    let (mine, other) = if kind == "prepend" {
                        (&mut entry.1, &entry.0)
                    } else {
                        (&mut entry.0, &entry.1)
                    };
                    let clash = names.iter().any(|t| other.contains(t));
                    mine.extend(names.iter().cloned());
                    if clash {
                        self.wildcard(owner.clone(), MixinSide::Include);
                    }
                    self.out.direct_mixins.push((key.clone(), kind == "prepend", names.clone()));
                    self.out.written_mixins.push(WrittenMixin {
                        owners: vec![key],
                        prepend: kind == "prepend",
                        names: names.clone(),
                        receiver_form: false,
                    });
                }
            } else {
                if side == MixinSide::Include {
                    self.out.written_mixins.push(WrittenMixin {
                        owners: owner.clone(),
                        prepend: kind == "prepend",
                        names: names.clone(),
                        receiver_form: false,
                    });
                }
                self.unpositioned(owner.clone(), side, names);
            }
            if !complete {
                // `note(complete: false)` → `taint(unnamed: true)`: the
                // recorded names' neighbour is a module no name spells.
                self.unnamed(owner, side);
            }
            return;
        }
        let recv = recv.expect("receiver");
        if let Some(sc) = recv.as_call_node() {
            let sc_name = constant_string(sc.name().as_slice());
            let plain = sc.receiver().is_none_or(|r| r.as_self_node().is_some());
            if sc_name == "singleton_class" && sc.arguments().is_none() && plain {
                if let Some(owner) = self.self_owner() {
                    // `unnamed_singleton_include?` / `taint_unrecorded_mixin`:
                    // an argument that names no module leaves the singleton
                    // SET unknown (`singleton_class.include(*MODS)`).
                    if args.iter().any(|a| opaque_mixin_argument(a)) {
                        self.unnamed(owner, MixinSide::Extend);
                    } else {
                        self.wildcard(owner, MixinSide::Extend);
                    }
                }
                return;
            }
        }
        if let Some(raw) = strict_constant_path_string(&recv) {
            let side = if kind == "extend" { MixinSide::Extend } else { MixinSide::Include };
            let owners = self.candidates(&raw);
            // The side's UNNAMED taint: an argument the walk cannot name
            // (`include M, h` — `note(complete: false)`, or `include(*M)` /
            // `include m` — `taint_unrecorded_mixin`'s `opaque`).
            let set_unknown =
                args.iter().any(|a| opaque_mixin_argument(a)) || (!complete && !names.is_empty());
            if kind == "extend" {
                // `receiver_extend_recorded?`: constant arguments name edges;
                // a `self` or other argument, or a `send`, taints.
                let mut listed = names.clone();
                if via_send || !complete || args.iter().any(|a| a.as_self_node().is_some()) {
                    listed.push(WILDCARD.to_string());
                    if set_unknown {
                        listed.push(UNNAMED.to_string());
                    }
                }
                self.out.singleton_mixins.push((owners.clone(), listed));
            }
            if kind != "extend" && !args.is_empty() {
                let mut listed = names.clone();
                if !complete {
                    listed.push(WILDCARD.to_string());
                    if set_unknown {
                        listed.push(UNNAMED.to_string());
                    }
                }
                self.out.written_mixins.push(WrittenMixin {
                    owners: owners.clone(),
                    prepend: kind == "prepend",
                    names: names.clone(),
                    receiver_form: true,
                });
                self.out.receiver_mixins.push((owners.clone(), listed));
            }
            if set_unknown {
                self.unnamed(owners, side);
            } else {
                self.wildcard(owners, side);
            }
            return;
        }
        if self.hook_receiver(&recv) {
            if let Some(owner) = self.self_owner() {
                // `taint_unrecorded_mixin` — `unnamed` on the instance side
                // reads `kind ∈ {include, prepend}` AND a receiver that is not
                // a `singleton_class` call AND (`hook_parameter_receiver?` —
                // the BARE local, not the chain — or an opaque argument);
                // the extend side's is `opaque && !hook_parameter_receiver?`.
                // A chained receiver (`base.singleton_class`, `base.class`)
                // still taints both sides — `hook_receiver?` — but keeps the
                // side it is not opaque to order-only.
                let sc_eval = recv
                    .as_call_node()
                    .is_some_and(|c| constant_string(c.name().as_slice()) == "singleton_class");
                let hook_param = self.hook_parameter_receiver(&recv);
                let opaque = args.iter().any(|a| opaque_mixin_argument(a));
                if kind != "extend" && !sc_eval && (hook_param || opaque) {
                    self.unnamed(owner.clone(), MixinSide::Include);
                } else {
                    self.wildcard(owner.clone(), MixinSide::Include);
                }
                if opaque && !hook_param {
                    self.unnamed(owner, MixinSide::Extend);
                } else {
                    self.wildcard(owner, MixinSide::Extend);
                }
            }
        }
    }

    /// `hook_parameter_receiver?` — the receiver IS a hook parameter (`base`
    /// in `def self.included(base)`), as distinct from `hook_receiver?` which
    /// also follows a no-argument call chain off one (`base.singleton_class`,
    /// `base.class`).
    fn hook_parameter_receiver(&self, recv: &PrismNode<'_>) -> bool {
        recv.as_local_variable_read_node()
            .is_some_and(|lv| self.hook_params.contains(&constant_string(lv.name().as_slice())))
    }

    /// Upstream v0.4.8 (#3006, `taint_computed_mixin_send` +
    /// `taint_computed_hook_send`): a receiverless or `self.` `send` /
    /// `public_send` / `__send__` written in a class body (or a hook's
    /// `base.send`) whose name is computed and which passes at least one
    /// further argument may be a mixin call the walk cannot record — it lists
    /// the [`UNNAMED`] marker on the side(s) the name may reach. A name bound
    /// only to non-mixin literals in the call's class-body scope taints
    /// nothing ([`local_send_names`]).
    fn taint_computed_send(&mut self, n: &PrismNode<'pr>, call: &ruby_prism::CallNode<'pr>) {
        // `computed_send_name`: a `send`-family call on the lexical `self` or
        // a hook parameter, with a non-literal first argument and at least one
        // further argument.
        let call_name = constant_string(call.name().as_slice());
        if !SEND_NAMES.contains(&call_name.as_str()) {
            return;
        }
        let recv = call.receiver();
        let lexical_self = recv.is_none() || recv.as_ref().is_some_and(|r| r.as_self_node().is_some());
        // `computed_send_name` accepts the hook PARAMETER — the bare local —
        // not `hook_receiver?`'s call chain: `base.singleton_class.send(m, M)`
        // names nothing mixin.
        let hook = recv.as_ref().is_some_and(|r| self.hook_parameter_receiver(r));
        if !lexical_self && !hook {
            return;
        }
        let args: Vec<PrismNode> =
            call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        if args.len() < 2 || literal_send_name(&args[0]).is_some() {
            return;
        }
        // `current_class` — the owner the walk names for the call's `self`:
        // [`Census::self_owner`], the declaration/eval owner or the
        // `instance_eval` receiver — none for `class << obj` or an ownerless
        // frame.
        let in_singleton = self.frame().singleton;
        let owner = self.self_owner();
        let Some(owner) = owner else { return };
        let scope = self.class_body_scope(n);
        if scope.is_none() && !hook {
            return;
        }
        let names = scope.and_then(|s| local_send_names(&args[0], &s));
        if hook {
            // `taint_computed_hook_send`: no taint when the name resolves and
            // none of its literals is a mixin kind; the include side marks
            // UNNAMED only when the name may be `include` or `prepend`.
            if names.as_ref().is_some_and(|ns| !ns.iter().any(|l| MIXIN_KINDS.contains(&l.as_str()))) {
                return;
            }
            let instance =
                names.as_ref().is_none_or(|ns| ns.iter().any(|l| l == "include" || l == "prepend"));
            self.wildcard(owner.clone(), MixinSide::Extend);
            if instance {
                self.unnamed(owner, MixinSide::Include);
            } else {
                self.wildcard(owner, MixinSide::Include);
            }
            return;
        }
        // `names` nil — the argument may spell any name — taints every side
        // the name may reach; a resolved name list taints only the kinds it
        // spells. The include walk reads `{include, prepend}` on `:include`;
        // the extend walk reads `{extend}` on `:extend`, and inside
        // `class << self` — where a computed `include` / `prepend` is the
        // singleton mixin — `{include, prepend}` on `:extend` alone.
        let may = |kinds: &[&str]| -> bool {
            names.as_ref().is_none_or(|ns| ns.iter().any(|l| kinds.contains(&l.as_str())))
        };
        if in_singleton {
            if may(&["include", "prepend"]) {
                self.unnamed(owner, MixinSide::Extend);
            }
        } else {
            if may(&["include", "prepend"]) {
                self.unnamed(owner.clone(), MixinSide::Include);
            }
            if may(&["extend"]) {
                self.unnamed(owner, MixinSide::Extend);
            }
        }
    }

    /// `MixinAccumulator#class_body_scope` — the node whose locals a call
    /// written in a registered class body reads (the body itself, or the
    /// `def self.` it sits in), or `None` when the call is not one whose
    /// `self` is that body's class as far as [`collect_class_body_calls`] can
    /// tell. Collected lazily, a body at a time, exactly as the accumulator
    /// extends its map on the first question.
    fn class_body_scope(&mut self, n: &PrismNode<'pr>) -> Option<PrismNode<'pr>> {
        while self.class_bodies_read < self.class_bodies.len() {
            let body = node_copy(&self.class_bodies[self.class_bodies_read]);
            for (key, scope) in collect_class_body_calls(&body) {
                self.class_body_calls.insert(key, scope);
            }
            self.class_bodies_read += 1;
        }
        self.class_body_calls.get(&span_key(n)).map(node_copy)
    }

    /// `taint_opaque_eval`'s `block_mixes_into_self?` — a receiverless or
    /// `self.` `include` / `prepend` (or its `send` spelling) anywhere in
    /// `node`, which may mix into the class the block's `self` is.
    fn block_mixes_into_self(&self, n: &PrismNode<'_>) -> bool {
        struct Finder<'a, 'pr> {
            census: &'a Census<'pr>,
            found: bool,
        }
        impl<'pr> Visit<'pr> for Finder<'_, 'pr> {
            fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
                let lexical_self =
                    node.receiver().is_none_or(|r| r.as_self_node().is_some());
                if lexical_self
                    && self
                        .census
                        .mixin_view(node)
                        .is_some_and(|(k, _, _)| k == "include" || k == "prepend")
                {
                    self.found = true;
                    return;
                }
                ruby_prism::visit_call_node(self, node);
            }
        }
        let mut f = Finder { census: self, found: false };
        f.visit(n);
        f.found
    }

    /// Upstream v0.4.6 (#2269, `singleton_class_rewrite_tainted?` +
    /// `singleton_class_include_recorded?`): a surface-rewriting call on
    /// `singleton_class` (bare, `self.` or a constant's) records the owner's
    /// singleton side — named for a constant-argument `include` / `prepend`,
    /// nothing for a literal-named naming call written as a direct statement,
    /// the wildcard otherwise.
    fn record_singleton_class_rewrite(&mut self, n: &PrismNode<'_>, call: &ruby_prism::CallNode<'_>) {
        let Some(recv) = call.receiver() else { return };
        let Some(sc) = recv.as_call_node() else { return };
        if constant_string(sc.name().as_slice()) != "singleton_class" || sc.arguments().is_some() {
            return;
        }
        let name = constant_string(call.name().as_slice());
        if !surface_rewriting_call(&name) {
            return;
        }
        let object = sc.receiver();
        let lexical_self = object.as_ref().is_none_or(|o| o.as_self_node().is_some());
        let owner = if lexical_self {
            self.self_owner()
        } else {
            object.as_ref().and_then(strict_constant_path_string).map(|raw| self.candidates(&raw))
        };
        let Some(owner) = owner else { return };
        let args: Vec<PrismNode<'_>> =
            call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        if self.is_direct(n) && singleton_class_named_call(&name, &args) {
            return;
        }
        let up_level = self.frame().singleton && lexical_self;
        // Upstream v0.4.8 `singleton_class_include_recorded?` (#2998): a
        // constant-argument `include` (spelled `include` only — a `send`
        // spelling stays a taint) or `prepend` — `prepend` records through
        // `send(:prepend, P)` too — puts the module on the receiver's
        // singleton.
        let recordable = match self.mixin_view(call) {
            Some(("prepend", view_args, _)) => Some(view_args),
            Some(("include", view_args, false)) if name == "include" => Some(view_args),
            _ => None,
        };
        if !up_level {
            if let Some(view_args) = recordable {
                let consts: Vec<Option<String>> =
                    view_args.iter().map(strict_constant_path_string).collect();
                if !consts.is_empty() && consts.iter().all(Option::is_some) {
                    self.out
                        .singleton_mixins
                        .push((owner, consts.into_iter().flatten().collect()));
                    return;
                }
            }
        }
        self.out.singleton_mixins.push((owner, vec![WILDCARD.to_string()]));
    }

    /// Upstream v0.4.6 (#2094, `record_named_arguments_opaque`): the literal
    /// names a non-neutral receiverless (or `self.`) class-body call passes.
    /// Dynamic surface calls are included: a superset that only declines more.
    fn record_macro_names(&mut self, call: &ruby_prism::CallNode<'_>) {
        if call.receiver().is_some_and(|r| r.as_self_node().is_none()) {
            return;
        }
        let name = constant_string(call.name().as_slice());
        if NAME_NEUTRAL_MACROS.contains(&name.as_str()) {
            return;
        }
        let Some(owner) = self.owner() else { return };
        let Some(arguments) = call.arguments() else { return };
        for a in arguments.arguments().iter() {
            let lit = if let Some(d) = a.as_def_node() {
                Some(constant_string(d.name().as_slice()))
            } else {
                super::definitions::literal_symbol_or_string_name(&a)
            };
            if let Some(lit) = lit {
                for o in &owner {
                    let pair = (o.clone(), lit.clone());
                    if !self.out.macro_names.contains(&pair) {
                        self.out.macro_names.push(pair);
                    }
                }
            }
        }
    }

    fn block_mixes_in(&self, n: &PrismNode<'_>) -> bool {
        struct Finder<'a, 'pr> {
            census: &'a Census<'pr>,
            found: bool,
        }
        impl<'pr> Visit<'pr> for Finder<'_, 'pr> {
            fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
                if self.census.mixin_view(node).is_some() {
                    self.found = true;
                    return;
                }
                ruby_prism::visit_call_node(self, node);
            }
        }
        let mut f = Finder { census: self, found: false };
        f.visit(n);
        f.found
    }
}

impl<'pr> Visit<'pr> for Census<'pr> {
    fn visit_program_node(&mut self, node: &ruby_prism::ProgramNode<'pr>) {
        for s in node.statements().body().iter() {
            self.direct.insert(span_key(&s));
        }
        self.main_self = collect_main_self_calls(&node.statements().as_node());
        ruby_prism::visit_program_node(self, node);
    }

    fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
        if let Some(sup) = node.superclass() {
            self.visit(&sup);
        }
        let name = constant_path_string(&node.constant_path());
        self.declaration(&node.as_node(), name, node.body());
    }

    fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
        let name = constant_path_string(&node.constant_path());
        self.declaration(&node.as_node(), name, node.body());
    }

    fn visit_singleton_class_node(&mut self, node: &ruby_prism::SingletonClassNode<'pr>) {
        let expr = node.expression();
        self.visit(&expr);
        let is_self = expr.as_self_node().is_some();
        // Upstream v0.4.8 (#3010): `class << Const` is followed as the
        // constant's own singleton; `class << obj` names something this
        // walk cannot resolve to a class.
        let owner = if is_self {
            self.owner()
        } else {
            strict_constant_path_string(&expr).map(|raw| self.candidates(&raw))
        };
        // `class << self` inside `X.instance_eval { … }` opens `X`'s singleton.
        let eval_owner = if is_self { self.self_owner() } else { None };
        // `MixinAccumulator#class_body` — registered only where the
        // reference's walks register one: declaration bodies (in
        // [`declaration`]) and a FIRST-LEVEL `class << self`'s — `opens_self =
        // self && !in_singleton`. `class << Const`
        // (`walk_extends_constant_singleton`) and a nested `class << self`
        // inside a singleton body register nothing: a computed `send` in
        // either stays a taint.
        let registrable = is_self && !self.frame().singleton;
        if registrable {
            if let Some(b) = node.body() {
                self.class_bodies.push(node_copy(&b));
            }
        }
        self.frames.push(Frame { owner, singleton: true, uncertain: false, eval_owner });
        if let Some(b) = node.body() {
            self.visit(&b);
        }
        self.frames.pop();
    }

    fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
        let name = constant_string(node.name().as_slice());
        let recv_self = node.receiver().is_some_and(|r| r.as_self_node().is_some());
        let singleton_def = recv_self || (node.receiver().is_none() && self.frame().singleton);
        // `hook_def_params` — the parameters of ANY `def` named in `HOOK_DEFS`
        // (an instance `def included(base)` binds `base` the same as
        // `def self.included(base)`; the extends walk needs it for the
        // `included` / `inherited` hooks `extend` runs), at any `def` depth.
        let hook = HOOK_DEFS.contains(&name.as_str());
        if self.in_def == 0 {
            if let Some(owner) = self.owner() {
                for o in &owner {
                    if !self.out.def_owners.contains(o) {
                        self.out.def_owners.push(o.clone());
                    }
                }
                if singleton_def {
                    if ALL_HOOKS.contains(&name.as_str()) {
                        self.out.dirty_owners.extend(owner.iter().cloned());
                    }
                    // `extend_reshapes_instance?` — `extended` / `extend_object`
                    // on the extended module's singleton is a hook `extend`
                    // runs.
                    if ["extended", "extend_object"].contains(&name.as_str()) {
                        self.out.extend_hook_owners.extend(owner.iter().cloned());
                    }
                } else if node.receiver().is_none() {
                    let key = span_key(&node.as_node());
                    let recorded = self.direct.contains(&key) || self.recorded_arg_defs.contains(&key);
                    for o in &owner {
                        if !recorded {
                            self.out.possible_methods.push((o.clone(), name.clone()));
                        }
                        if self.frame().uncertain {
                            self.out.contested_visibilities.push((o.clone(), name.clone()));
                        }
                    }
                    if name == "method_missing" {
                        self.out.dirty_owners.extend(owner.iter().cloned());
                    }
                    // `INSTANCE_EXTEND_HOOKS` — the instance-side hooks an
                    // `extend` runs on the extended module.
                    if ["included", "append_features", "prepended", "prepend_features", "inherited"]
                        .contains(&name.as_str())
                    {
                        self.out.extend_hook_owners.extend(owner.iter().cloned());
                    }
                }
                // A constant receiver's `def X.hook` files under X's
                // candidates, as `discovered_singleton_def_nodes[X]` does.
                if let Some(r) = node.receiver() {
                    if r.as_self_node().is_none() {
                        if let Some(raw) = strict_constant_path_string(&r) {
                            if ["extended", "extend_object"].contains(&name.as_str()) {
                                let owners = self.candidates(&raw);
                                self.out.extend_hook_owners.extend(owners);
                            }
                        }
                    }
                }
            }
        }
        let saved_hooks = self.hook_params.len();
        if hook {
            if let Some(params) = node.parameters() {
                for p in params.requireds().iter().chain(params.optionals().iter()) {
                    if let Some(rp) = p.as_required_parameter_node() {
                        self.hook_params.push(constant_string(rp.name().as_slice()));
                    } else if let Some(op) = p.as_optional_parameter_node() {
                        self.hook_params.push(constant_string(op.name().as_slice()));
                    }
                }
            }
        }
        self.in_def += 1;
        ruby_prism::visit_def_node(self, node);
        self.in_def -= 1;
        self.hook_params.truncate(saved_hooks);
    }

    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        let n = node.as_node();
        let name = constant_string(node.name().as_slice());
        let args: Vec<PrismNode<'pr>> =
            node.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        let recv = node.receiver();
        let direct = self.is_direct(&n);
        let mut direct_modifier = false;
        // Reference v0.4.4 `visibility_call_parts` (#1633): receiverless, on
        // `self`, or `send(:private, …)`; a block is ignored.
        let parts = if self.in_def == 0 {
            super::definitions::visibility_call_parts(node)
        } else {
            None
        };
        if let Some((vname, vargs)) = &parts {
            let modifier = MODIFIERS.contains(&vname.as_str()) && !self.frame().singleton;
            let splat = vargs.iter().any(|a| a.as_splat_node().is_some());
            if vargs.is_empty() {
                // A bare toggle: only a direct one settles the running default.
                if let Some(f) = self.frames.last_mut() {
                    f.uncertain = !direct;
                }
            } else if splat {
                // `private(*names)` may be a bare toggle (`private(*[])`), so
                // the default after it is uncertain, and it may name any slot
                // recorded so far (`contest_class`): the wildcard slot.
                if let Some(f) = self.frames.last_mut() {
                    f.uncertain = true;
                }
                if modifier {
                    if let Some(owner) = self.owner() {
                        for o in &owner {
                            self.out.contested_visibilities.push((o.clone(), WILDCARD.to_string()));
                        }
                    }
                }
            }
            if modifier && !vargs.is_empty() {
                direct_modifier = direct;
                if let Some(owner) = self.owner() {
                    let mut contested: Vec<String> = Vec::new();
                    for a in vargs {
                        if let Some(def) = a.as_def_node() {
                            // `private def self.x` reaches an instance `x`, if one
                            // exists, in a way the walk cannot read.
                            if def.receiver().is_some() {
                                contested.push(constant_string(def.name().as_slice()));
                            }
                        } else if !direct {
                            if let Some(arr) = a.as_array_node() {
                                contested.extend(arr.elements().iter().filter_map(|e| {
                                    super::definitions::literal_symbol_or_string_name(&e)
                                }));
                            } else if let Some(lit) =
                                super::definitions::literal_symbol_or_string_name(a)
                            {
                                contested.push(lit);
                            }
                        }
                    }
                    for o in &owner {
                        for c in &contested {
                            self.out.contested_visibilities.push((o.clone(), c.clone()));
                        }
                    }
                }
            }
        } else if recv.is_none() && self.in_def == 0 && !direct && !self.frame().singleton {
            // An `attr_*` member written anywhere but a direct statement (or the
            // argument of a direct visibility call) holds a slot the port's
            // table does not record.
            let key = span_key(&n);
            if !self.recorded_arg_defs.contains(&key) {
                let members = super::definitions::attr_macro_members(node);
                if let Some(owner) = self.owner() {
                    for o in &owner {
                        for m in &members {
                            self.out.contested_visibilities.push((o.clone(), m.clone()));
                        }
                    }
                }
            }
        }
        self.record_mixin(&n, node);
        self.record_singleton_class_rewrite(&n, node);
        // `record_method_envelope_opaque` files the name on both sides even
        // inside `class << self`, so the macro table is side-agnostic here.
        if self.in_def == 0 {
            self.record_macro_names(node);
        }

        // An eval block on an object the walk cannot name: in a hook, or
        // holding a mixin call, it reshapes the enclosing class.
        let block = node.block().and_then(|b| b.as_block_node());
        let eval = EVAL_CALLS.contains(&name.as_str());
        if eval {
            if let (Some(r), Some(b)) = (&recv, &block) {
                if r.as_self_node().is_none()
                    && strict_constant_path_string(r).is_none()
                    && (self.hook_receiver(r) || self.block_mixes_in(&b.as_node()))
                {
                    if let Some(owner) = self.owner() {
                        // `taint_opaque_eval` (#2991): the include side's SET
                        // is unknown only where the block may mix into the
                        // class itself — a hook's `base`, or a receiverless /
                        // `self.` include|prepend in the block of a receiver
                        // other than a `singleton_class` — the extend side
                        // keeps the wildcard.
                        let hook = self.hook_receiver(r);
                        let sc_eval = r
                            .as_call_node()
                            .is_some_and(|c| constant_string(c.name().as_slice()) == "singleton_class");
                        if hook || (!sc_eval && self.block_mixes_into_self(&b.as_node())) {
                            self.unnamed(owner.clone(), MixinSide::Include);
                        } else {
                            self.wildcard(owner.clone(), MixinSide::Include);
                        }
                        self.wildcard(owner, MixinSide::Extend);
                    }
                }
            }
        }

        if let Some(r) = &recv {
            self.visit(r);
        }
        if let Some(arguments) = node.arguments() {
            if direct_modifier {
                // The harvest records a direct visibility call's `def` and
                // `attr_*` arguments (`send(:private, …)`'s too).
                for a in &args {
                    if a.as_def_node().is_some() || a.as_call_node().is_some() {
                        self.recorded_arg_defs.insert(span_key(a));
                    }
                }
            }
            self.visit(&arguments.as_node());
        }
        let Some(blk) = node.block() else { return };
        let Some(b) = blk.as_block_node() else {
            self.visit(&blk);
            return;
        };
        // Which owner the block's `self` is.
        let recv_const = recv.as_ref().and_then(strict_constant_path_string);
        let meta_new = matches!(
            (recv_const.as_deref(), name.as_str()),
            (Some("Class" | "::Class" | "Module" | "::Module" | "Struct" | "::Struct"), "new")
                | (Some("Data" | "::Data"), "define")
        );
        let frame = if meta_new {
            Some(Frame { owner: None, singleton: false, uncertain: false, eval_owner: None })
        } else if name == "define_method" {
            // Upstream `rebound_block_self` (`OPAQUE_SELF_BLOCK_CALLS`): a
            // `define_method` block is a METHOD body — its `self` is the
            // future instance, unnameable on ANY receiver — so an
            // `include` written in it records under nothing (no edge, no
            // taint), exactly as `self_owner()` answering `None` drops it.
            // The block's other arguments (a `->` lambda) keep the
            // enclosing `self`, as the reference's walk keeps them.
            Some(Frame { owner: None, singleton: false, uncertain: false, eval_owner: None })
        } else if eval {
            let instance_side = name == "instance_eval" || name == "instance_exec";
            let owner = match (&recv, &recv_const) {
                (None, _) => self.owner(),
                (Some(r), _) if r.as_self_node().is_some() => self.owner(),
                (Some(_), Some(raw)) => Some(self.candidates(raw)),
                _ => None,
            };
            // A bare / `self` receiver keeps the enclosing self, an
            // `instance_eval` frame's included.
            let eval_owner = match (&recv, &recv_const) {
                (None, _) => self.self_owner(),
                (Some(r), _) if r.as_self_node().is_some() => self.self_owner(),
                _ => owner.clone(),
            };
            let owner = if instance_side { None } else { owner };
            Some(Frame { owner, singleton: false, uncertain: false, eval_owner })
        } else {
            None
        };
        let pushed = frame.is_some();
        if let Some(f) = frame {
            self.frames.push(f);
        }
        ruby_prism::visit_block_node(self, &b);
        if pushed {
            self.frames.pop();
        }
    }
}

/// Collect the [`AncestryFacts`] of one file.
pub(crate) fn collect_ancestry_facts(root: &PrismNode<'_>) -> AncestryFacts {
    let mut c = Census {
        out: AncestryFacts::default(),
        prefix: Vec::new(),
        frames: vec![Frame { owner: None, singleton: false, uncertain: false, eval_owner: None }],
        direct: HashSet::new(),
        recorded_arg_defs: HashSet::new(),
        in_def: 0,
        hook_params: Vec::new(),
        direct_includes: std::collections::HashMap::new(),
        main_self: HashSet::new(),
        class_bodies: Vec::new(),
        class_body_calls: std::collections::HashMap::new(),
        class_bodies_read: 0,
    };
    c.visit(root);
    c.out.singleton_locals = super::singleton_locals::collect(root);
    c.out
}

#[cfg(test)]
mod tests {
    use super::required_vendored_features as scan;

    /// The reference's `ANY_FEATURE` + `commented?`, probed row by row
    /// against `RequiredFeatures.features_of` (v0.4.6), reduced to first
    /// segments.
    #[test]
    fn required_segment_scan_matches_the_reference_any_feature() {
        use super::required_feature_segments as seg;
        let rows: &[(&[u8], &[&str])] = &[
            (b"require \"digest\"", &["digest"]),
            (b"require \"digest/sha2\"", &["digest"]),
            (b"\xc3\xa9require \"digest\"", &["digest"]),
            (b"require \"digest/\"", &[]),
            (b"x.require \"digest\"", &[]),
            (b"require \"set\"; require \"matrix\"", &["set", "matrix"]),
            (b"  # require 'digest'", &[]),
            (b"Kernel.require('digest')", &["digest"]),
            (b"require_relative 'digest'", &[]),
            (b"require 'digest\"", &[]),
        ];
        for (src, want) in rows {
            let want: Vec<String> = want.iter().map(|s| s.to_string()).collect();
            assert_eq!(seg(src), want, "{}", String::from_utf8_lossy(src));
        }
    }

    /// The reference's `RequiredFeatures::PATTERN` + `commented?`, row by row.
    #[test]
    fn required_feature_scan_matches_the_reference_pattern() {
        let yes: &[&[u8]] = &[
            b"require \"prime\"\n",
            b"require 'prime'",
            b"require('prime')",
            b"require ( \"prime\" )",
            b"Kernel.require 'prime'",
            b"p 1 # require \"prime\"\n",
            b"x = <<~R\n  require 'prime'\nR\n",
            b"  x; require \"prime\"",
        ];
        for src in yes {
            assert_eq!(scan(src), vec!["prime".to_string()], "{}", String::from_utf8_lossy(src));
        }
        let no: &[&[u8]] = &[
            b"# require \"prime\"\n",
            b"   # require 'prime'",
            b"require_relative 'prime'",
            b"obj.require 'prime'",
            b"$require 'prime'",
            b"require \"primes\"",
            b"require 'prime\"",
            b"send(:require, 'prime')",
            b"require(\n'prime')",
            b"prime = 1",
        ];
        for src in no {
            assert!(scan(src).is_empty(), "{}", String::from_utf8_lossy(src));
        }
    }

    fn program_stmt<'a>(result: &'a ruby_prism::ParseResult<'a>) -> ruby_prism::Node<'a> {
        result
            .node()
            .as_program_node()
            .expect("program")
            .statements()
            .body()
            .iter()
            .next()
            .expect("one statement")
    }

    /// Callers besides `main_self_visit` pass Call/Def/Parameters; emptying
    /// those dropped mixin-taint writes (PR #669 review).
    #[test]
    fn compact_children_of_a_call_with_block_includes_the_block() {
        let result = crate::parse(b"[1].each { m = :include }\n");
        let stmt = program_stmt(&result);
        assert!(stmt.as_call_node().is_some());
        let kids = super::compact_children(&stmt);
        assert!(
            kids.iter().any(|k| k.as_block_node().is_some()),
            "CallNode block must be a compact child (got {})",
            kids.len()
        );
    }

    #[test]
    fn compact_children_of_def_self_is_nonempty() {
        let result = crate::parse(b"def self.go\n  m = :extend\nend\n");
        let stmt = program_stmt(&result);
        assert!(stmt.as_def_node().is_some());
        let kids = super::compact_children(&stmt);
        assert!(
            !kids.is_empty(),
            "DefNode compact children must include the body"
        );
    }

    #[test]
    fn compact_children_of_if_still_reaches_the_then_call() {
        let result = crate::parse(b"foo if x\n");
        let stmt = program_stmt(&result);
        assert!(stmt.as_if_node().is_some());
        let kids = super::compact_children(&stmt);
        assert!(
            kids.iter().any(|k| k.as_call_node().is_some() || k.as_statements_node().is_some()),
            "IfNode compact children must include the then-arm (got {})",
            kids.len()
        );
    }
}
