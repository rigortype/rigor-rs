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

/// See the module docs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AncestryFacts {
    pub unpositioned: Vec<UnpositionedMixin>,
    pub possible_methods: Vec<(String, String)>,
    pub contested_visibilities: Vec<(String, String)>,
    pub dirty_owners: Vec<String>,
    pub direct_mixins: Vec<(String, bool, Vec<String>)>,
    /// Owners with any `def` in their body (instance or singleton, recorded or
    /// not) — the reference's `known_user_class?` reads a `def` of either
    /// kind as making a name a project class.
    pub def_owners: Vec<String>,
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
    /// Upstream v0.4.5 (#2086 / issue #1722) — every RECEIVER-form instance
    /// mixin `X.include(M)` / `X.prepend(M)` / `X.send(:include, M)`, wherever
    /// it is written: `(owner candidates, from, names)`, with `from` the call's
    /// lexical nesting plus one placeholder segment (the shape
    /// `resolve_override_ancestor` reads) and a non-constant argument as
    /// [`WILDCARD`]. The reference records such an edge (unpositioned) under
    /// the class the receiver names; the port keeps its wildcard taint for the
    /// order readers and lists the names here for the set-shaped
    /// `call.undefined-method` decline only.
    pub receiver_mixins: Vec<(Vec<String>, String, Vec<String>)>,
    /// Upstream v0.4.5 (#2086 / issue #1722) — the reference's
    /// `LexicalMethodSites` singleton-local table for this file.
    pub singleton_locals: super::singleton_locals::SingletonLocals,
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
            // `(?:(?<![\w.$@])|(?<=Kernel\.))`
            let ok_prefix = at == 0 || {
                let b = source[at - 1];
                !(b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80 || b == b'.' || b == b'$' || b == b'@')
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

const MODIFIERS: [&str; 3] = ["private", "protected", "public"];
const MIXIN_KINDS: [&str; 3] = ["include", "prepend", "extend"];
const SEND_NAMES: [&str; 3] = ["send", "public_send", "__send__"];
const EVAL_CALLS: [&str; 6] =
    ["class_eval", "module_eval", "class_exec", "module_exec", "instance_eval", "instance_exec"];
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

/// One owner frame: the candidate names (`None` = an owner the walk cannot
/// name, which records nothing), whether `self` is the singleton, and the
/// running visibility default's uncertainty.
struct Frame {
    owner: Option<Vec<String>>,
    singleton: bool,
    uncertain: bool,
}

struct Census {
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
}

impl Census {
    fn frame(&self) -> &Frame {
        self.frames.last().expect("root frame")
    }

    fn owner(&self) -> Option<Vec<String>> {
        self.frame().owner.clone()
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

    fn declaration(&mut self, n: &PrismNode<'_>, name: String, body: Option<PrismNode<'_>>) {
        let direct = self.is_direct(n);
        if name.is_empty() {
            // An unnameable declaration owns nothing the port can hold.
            self.frames.push(Frame { owner: None, singleton: false, uncertain: false });
            if let Some(b) = &body {
                self.visit(b);
            }
            self.frames.pop();
            return;
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
        self.frames.push(Frame { owner: Some(vec![qualified]), singleton: false, uncertain: false });
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
    fn mixin_view<'pr>(
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

    fn record_mixin(&mut self, n: &PrismNode<'_>, call: &ruby_prism::CallNode<'_>) {
        let Some((kind, args, via_send)) = self.mixin_view(call) else {
            return;
        };
        let names: Vec<Option<String>> =
            args.iter().map(strict_constant_path_string).collect();
        let complete = !names.is_empty() && names.iter().all(Option::is_some);
        let names: Vec<String> = names.into_iter().flatten().collect();
        let recv = call.receiver();
        if recv.is_none() && !via_send && kind == "include" && self.frames.len() == 1 && self.is_direct(n)
        {
            if complete {
                self.out.toplevel_includes.extend(names.iter().cloned());
            } else if !args.is_empty() {
                self.out.toplevel_includes.push(WILDCARD.to_string());
            }
            return;
        }
        let self_recv = recv.as_ref().is_some_and(|r| r.as_self_node().is_some());
        if recv.is_none() || self_recv {
            let Some(owner) = self.owner() else { return };
            let singleton = self.frame().singleton;
            let side = match (singleton, kind) {
                (true, "extend") => return,
                (true, _) | (false, "extend") => MixinSide::Extend,
                _ => MixinSide::Include,
            };
            if recv.is_none() && !via_send && complete {
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
                    }
                    return;
                }
                self.unpositioned(owner, side, names);
            } else {
                self.wildcard(owner, side);
            }
            return;
        }
        let recv = recv.expect("receiver");
        if let Some(sc) = recv.as_call_node() {
            let sc_name = constant_string(sc.name().as_slice());
            let plain = sc.receiver().is_none_or(|r| r.as_self_node().is_some());
            if sc_name == "singleton_class" && sc.arguments().is_none() && plain {
                if let Some(owner) = self.owner() {
                    self.wildcard(owner, MixinSide::Extend);
                }
                return;
            }
        }
        if let Some(raw) = strict_constant_path_string(&recv) {
            let side = if kind == "extend" { MixinSide::Extend } else { MixinSide::Include };
            let owners = self.candidates(&raw);
            if kind != "extend" && !args.is_empty() {
                let mut listed = names.clone();
                if !complete {
                    listed.push(WILDCARD.to_string());
                }
                let mut from = self.prefix.clone();
                from.push("_".to_string());
                self.out.receiver_mixins.push((owners.clone(), from.join("::"), listed));
            }
            self.wildcard(owners, side);
            return;
        }
        if self.hook_receiver(&recv) {
            if let Some(owner) = self.owner() {
                self.wildcard(owner.clone(), MixinSide::Include);
                self.wildcard(owner, MixinSide::Extend);
            }
        }
    }

    fn block_mixes_in(&self, n: &PrismNode<'_>) -> bool {
        struct Finder<'a> {
            census: &'a Census,
            found: bool,
        }
        impl<'pr> Visit<'pr> for Finder<'_> {
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

impl<'pr> Visit<'pr> for Census {
    fn visit_program_node(&mut self, node: &ruby_prism::ProgramNode<'pr>) {
        for s in node.statements().body().iter() {
            self.direct.insert(span_key(&s));
        }
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
        let owner = if expr.as_self_node().is_some() { self.owner() } else { None };
        self.frames.push(Frame { owner, singleton: true, uncertain: false });
        if let Some(b) = node.body() {
            self.visit(&b);
        }
        self.frames.pop();
    }

    fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
        let name = constant_string(node.name().as_slice());
        let recv_self = node.receiver().is_some_and(|r| r.as_self_node().is_some());
        let singleton_def = recv_self || (node.receiver().is_none() && self.frame().singleton);
        let mut hook = false;
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
                    hook = HOOK_DEFS.contains(&name.as_str());
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
                        self.wildcard(owner.clone(), MixinSide::Include);
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
            Some(Frame { owner: None, singleton: false, uncertain: false })
        } else if eval {
            let instance_side = name == "instance_eval" || name == "instance_exec";
            let owner = match (&recv, &recv_const) {
                (None, _) => self.owner(),
                (Some(r), _) if r.as_self_node().is_some() => self.owner(),
                (Some(_), Some(raw)) => Some(self.candidates(raw)),
                _ => None,
            };
            let owner = if instance_side { None } else { owner };
            Some(Frame { owner, singleton: false, uncertain: false })
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
        frames: vec![Frame { owner: None, singleton: false, uncertain: false }],
        direct: HashSet::new(),
        recorded_arg_defs: HashSet::new(),
        in_def: 0,
        hook_params: Vec::new(),
        direct_includes: std::collections::HashMap::new(),
    };
    c.visit(root);
    c.out.singleton_locals = super::singleton_locals::collect(root);
    c.out
}

#[cfg(test)]
mod tests {
    use super::required_vendored_features as scan;

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
}
