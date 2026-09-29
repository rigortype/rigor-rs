//! Passes 1c/1d: the def-attribution walk (issue #141), which files every `def`-family name under
//! the owner that binds it, its orphan post-pass, and the file's declared-constant census the walk
//! resolves eval receivers against.

use std::collections::{HashMap, HashSet};

use rigor_parse::{LoweredAst, Node, NodeId, Span};

// ==========================================================================
// Pass 1c/1d — def-attribution walk (upstream `fb781023`, rigortype/rigor#1135,
// rigor-rs issue #141).
//
// A `def` is filed under the class that OWNS it, which is not always the
// lexical enclosure: inside a `Recv.class_eval { … }` block `Module.nesting`
// stays lexical but `self` (and therefore `def`) belongs to the receiver. The
// reference threads two prefixes through `walk_methods_and_def_nodes` —
// `qualified_prefix` (lexical, for declarations) and `def_owner_prefix` (the
// rebound self, for `def`-family leaves) — plus the `in_singleton_class`,
// `singleton_cref` and `defs_singleton` flags. [`walk_defs`] is the port of
// that walk, limited to the two tables the port carries: `toplevel_defs`
// (the reference's `discovered_def_nodes["<toplevel>"]`, what
// `call.unresolved-toplevel` resolves a bare call against) and
// `discovered_methods` (the kind-less `record_def_method` existence table).
// ==========================================================================

/// The six `*_eval` / `*_exec` spellings whose block rebinding the walk
/// honours (`RECEIVER_EVAL_CALLS` upstream). The rules layer's
/// `receiver_eval_block_spans` carve-out recognises the same set for the
/// unrelated purpose of suppressing calls INSIDE the block; this set decides
/// who owns a `def` written inside one.
const RECEIVER_EVAL_METHODS: &[&str] = &[
    "class_eval",
    "module_eval",
    "class_exec",
    "module_exec",
    "instance_eval",
    "instance_exec",
];

/// `instance_eval` / `instance_exec` (`INSTANCE_EVAL_CALLS`): their block's
/// `def`s bind on the receiver's SINGLETON — `X.instance_eval { def m }` is
/// `X.m`, not `X#m` — which matters only for the `Object` toplevel collapse
/// (an `Object` singleton method is not bare-callable; probes `oi_eval` /
/// `oi_exec` fire on the oracle).
const INSTANCE_EVAL_METHODS: &[&str] = &["instance_eval", "instance_exec"];

/// The class-creating calls a constant write can name — the reference's
/// `meta_new_constant_rvalue?` (`Class.new` / `Module.new` / `Struct.new` /
/// `Data.define`). `K = Class.new { def m }` files `m` under `K`; the same
/// call anywhere else is anonymous (`ANONYMOUS_META_OWNER`).
const META_NEW_SELECTORS: &[(&str, &str)] = &[
    ("Class", "new"),
    ("Module", "new"),
    ("Struct", "new"),
    ("Data", "define"),
];

/// Marker def-owner for a `Class.new`/`Module.new`/`Struct.new`/`Data.define`
/// block no constant write names — the reference's `AnonymousMetaClass`
/// synthetic name. It is a single segment that matches
/// `Type::AnonymousClassName`, which is the `record_anonymous_body_def_as_toplevel`
/// (upstream #319) condition: a `def` inside an anonymous factory block ALSO
/// stays in `<toplevel>`, so `Module.new { def m }; m` is silent on the
/// oracle. The marker keeps that case out of `discovered_methods` while
/// routing the def to `toplevel_defs`.
const ANONYMOUS_META_OWNER: &str = "<anonymous-meta>";

/// Which side of the owner a position binds `def`s on —
/// `eval_body_def_context`'s `defs_singleton`/`unnameable` answer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DefsSide {
    /// Ordinary instance-side binding (`class_eval`/`module_eval`, class
    /// bodies). `Object` ownership collapses to `<toplevel>` here.
    Instance,
    /// Singleton-side binding (`instance_eval`/`instance_exec`, `class <<`
    /// bodies, `def self.x`/`def Owner.x`). Never reaches `<toplevel>`.
    Singleton,
    /// No nameable owner at all (`instance_eval` under an already-singleton
    /// self — `class << S; instance_eval { def m }`). Files nowhere.
    Unnameable,
}

/// The def-attribution walk's per-position context — the reference's
/// `qualified_prefix` + `def_owner_prefix` + flags, carried as one struct so
/// a single signature stays readable.
///
/// `lexical` is the declaration prefix (`Module.nesting`-style, advanced only
/// by `class`/`module` headers). `owner` is the `def`-owner override an eval
/// block, `class <<` body or `K = Class.new` write installs: `None` means
/// "own whatever `lexical` says", `Some([])` means "explicitly nowhere" (an
/// eval on an un-nameable receiver — its `def`s file under NO owner, not
/// `<toplevel>`). The distinction is the whole issue: before this walk every
/// def outside a class/module span landed on `<toplevel>` unconditionally.
pub(crate) struct DefCx {
    lexical: Vec<String>,
    owner: Option<Vec<String>>,
    /// `in_singleton_class` — inside a `class <<` body or a singleton-side
    /// eval context (the CALL side; a `def` here is a singleton def).
    in_singleton: bool,
    /// `singleton_cref` — the cref itself is an unnameable singleton class
    /// (`class << …`), so a BARE `class D`/`K =` inside names nothing.
    singleton_cref: bool,
    /// `defs_singleton`/`unnameable` — which side `def` leaves bind on.
    defs_side: DefsSide,
}

impl DefCx {
    /// The enclosing `self`'s prefix — the owner override when one is
    /// installed (an eval block's rebound self, a `class <<` prefix), else the
    /// lexical prefix (`eval_receiver_prefix`'s `self_prefix` argument).
    fn self_prefix(&self) -> Vec<String> {
        match &self.owner {
            Some(o) => o.clone(),
            None => self.lexical.clone(),
        }
    }
}

/// The root context — toplevel: lexical `[]`, no owner override, instance side.
pub(crate) fn def_root_cx() -> DefCx {
    DefCx {
        lexical: Vec::new(),
        owner: None,
        in_singleton: false,
        singleton_cref: false,
        defs_side: DefsSide::Instance,
    }
}

/// The per-file def-attribution accumulators [`walk_defs`] (and the
/// orphan-position post-pass [`file_orphan_defs`]) fill — the port of the
/// reference's `methods` / `def_nodes` split (`build_methods_and_def_nodes`
/// feeding `finalize_def_index`):
///
/// * `toplevel` — `def_nodes["<toplevel>"]`: real toplevel `def`s and
///   anonymous-factory defs. Merged CROSS-FILE (`top_level_def_for` is
///   project-wide: a `def helper` in `a.rb` resolves `helper` in `b.rb`).
/// * `macro_methods` — the instance-kind names CALLS introduce
///   (`define_method`, `attr_*`, `mattr_*`/`cattr_*`/`class_attribute`,
///   `alias_method`), keyed by qualified owner. These are what survives the
///   merge's `subtract_def_methods` into the cross-file `methods` table: an
///   accessor declared in `a.rb` suppresses `obj.a` in `b.rb`, while a plain
///   `def` is deliberately per-file (a cross-file `def` on a class IS the
///   ADR-17 monkey-patch case `undefined-method` surfaces).
/// * `def_names` — the instance-kind names that carry a `def` node (plain
///   instance defs, non-self `def Foo.x` receiver forms, `alias`-of-def
///   names) — the name sets `subtract_def_methods` removes from the
///   cross-file table.
/// * `file_methods` — this file's OWN instance-kind existence overlay (defs
///   AND macros): `seed_discovered_methods` deep-merges the file's raw table
///   over the def-stripped seed, so a name this file defines suppresses a
///   diagnostic in THIS file even when a def elsewhere kept it out of the
///   cross-file table.
/// * `file_toplevel` — the `Object`-owner slice of `file_methods`: the
///   per-file half of the `Object` → bare-callable collapse
///   (`source_declared_method?` reads `Object` through the same two-layer
///   table). A bare `m` in the defining file resolves
///   `Object.class_eval { def m }`; the same call in a sibling file
///   surfaces (probed: the reference fires; the pre-fix project-wide
///   collapse stayed silent).
/// * `pending_aliases` — `(owner, new, old)` `alias`/`alias_method` pairs,
///   resolved AFTER the whole walk: `apply_alias_def_nodes` consults the
///   file's COMPLETE def-node table, so an alias may precede its target
///   `def`.
#[derive(Default)]
pub(crate) struct DefTables {
    pub(crate) toplevel: HashSet<String>,
    pub(crate) macro_methods: HashMap<String, HashSet<String>>,
    pub(crate) def_names: HashMap<String, HashSet<String>>,
    pub(crate) file_methods: HashMap<String, HashSet<String>>,
    pub(crate) file_toplevel: HashSet<String>,
    pub(crate) pending_aliases: Vec<(String, String, String)>,
}

/// The body `DefCx` a `class`/`module` header opens — the reference's
/// `decl_body_context`:
///
/// * A `self::`-anchored header rides the REBOUND self
///   (`self_anchored_decl_prefix`): `self_base` is the singleton marker's
///   `[]` under `class <<`, else the def-owner override (eval/meta-new
///   block), else nil — so `Object.class_eval { class self::String }` names
///   `Object::String`, `class <<` makes it unnameable, and a bare `class
///   self::X` inside `module M` stays lexical (`M::X`). An EMPTY `self_decl`
///   marks the body ownerless.
/// * Otherwise `Source::ConstantPath.declaration_prefix` — a `::`-rooted
///   header RESETS the lexical prefix to the header's own name rather than
///   appending (`class ::Object` inside `module M` opens `Object`, not
///   `M::Object`).
/// * `unnameable_decl?` — an empty `self_decl` is unnameable; a non-empty one
///   re-anchors even under `class <<`; otherwise a bare/`self::` header under
///   an unnameable cref opens `#<singleton>::Name` while a constant path
///   (rooted or explicit non-self base) still re-anchors lexically
///   (`decl_nameable_under_cref?`).
fn decl_body_cx(cx: &DefCx, name: &str, rooted: bool, self_anchored: bool) -> DefCx {
    let self_decl = if self_anchored {
        let self_base: Option<Vec<String>> = if cx.in_singleton {
            Some(Vec::new())
        } else {
            cx.owner.clone()
        };
        self_base.map(|base| {
            if base.is_empty() {
                // `return [] if self_base.empty?` — a `self::` header under a
                // self nothing names (anonymous factory, `class <<`) is
                // unnameable, NOT `<base>::Name`.
                Vec::new()
            } else {
                // `self_anchored_decl_prefix` — `self_base + tail` where
                // `self_anchored_tail` returns one ELEMENT PER SEGMENT
                // (`self::A::B` ⇒ `["A", "B"]`), unlike `declaration_prefix`'s
                // single-element push below. The intermediate `base::A` rung
                // this creates is the reference's own shape.
                let mut p = base;
                p.extend(name.split("::").map(str::to_string));
                p
            }
        })
    } else {
        None
    };
    let child_prefix: Vec<String> = match &self_decl {
        Some(p) => p.clone(),
        None => {
            // `Source::ConstantPath.declaration_prefix` — the rendered name is
            // pushed as ONE element (`rooted ? [name] : outer + [name]`), so
            // `lexical_nesting_for_prefix` yields a rung only at declaration
            // boundaries: `class ::M::N` inside `module Outer` nests as
            // `M::N` alone (NEVER a bare `M` rung — #141 round 3), and
            // `class A::B` inside `module M` nests `M::A::B`, `M` — never a
            // `M::A` partial-segment rung.
            if rooted {
                vec![name.to_string()]
            } else {
                let mut lexical = cx.lexical.clone();
                lexical.push(name.to_string());
                lexical
            }
        }
    };
    let decl_nameable = rooted || (name.contains("::") && !self_anchored);
    let child_cref = match &self_decl {
        Some(d) => d.is_empty(),
        None => cx.singleton_cref && !decl_nameable,
    };
    DefCx {
        lexical: if child_cref { Vec::new() } else { child_prefix },
        owner: None,
        in_singleton: false,
        singleton_cref: child_cref,
        defs_side: DefsSide::Instance,
    }
}

/// `walk_methods_and_def_nodes` / `walk_eval_methods_and_defs`: file every
/// `def`-family name under the owner that binds it, threading lexical vs
/// rebound-self prefixes the way the reference does. `declared` is the file's
/// own qualified constant-name census (`eval_file_declared_names`) — a
/// constant a receiver names resolves through lexical rungs only when the
/// FILE declares it. `visited` collects every `Definition` this
/// edge-following walk reaches, so [`file_orphan_defs`] can file the
/// arena-resident defs it cannot.
pub(crate) fn walk_defs(
    ast: &LoweredAst,
    declared: &HashSet<String>,
    tables: &mut DefTables,
    visited: &mut HashSet<NodeId>,
    node: NodeId,
    cx: &DefCx,
) {
    match ast.get(node) {
        Node::ClassDef {
            name,
            rooted,
            self_anchored,
            body,
            ..
        }
        | Node::ModuleDef {
            name,
            rooted,
            self_anchored,
            body,
            ..
        } => {
            if name.is_empty() {
                // An un-renderable header keeps the enclosing context (the
                // reference falls through to the generic child walk).
                for &child in body {
                    walk_defs(ast, declared, tables, visited, child, cx);
                }
                return;
            }
            let inner = decl_body_cx(cx, name, *rooted, *self_anchored);
            for &child in body {
                walk_defs(ast, declared, tables, visited, child, &inner);
            }
        }
        Node::Definition {
            is_singleton_class: true,
            singleton_operand,
            body,
            ..
        } => {
            visited.insert(node);
            // `class << <expr>` — the SingletonClassNode arm: the operand is
            // walked for its own contents, then the body walks under the
            // operand's resolved singleton prefix (`singleton_body_prefix` /
            // `singleton_class_prefix`).
            if let Some(op) = singleton_operand {
                walk_defs(ast, declared, tables, visited, *op, cx);
            }
            let prefix = singleton_operand_prefix(ast, *singleton_operand, cx, declared);
            let inner = DefCx {
                lexical: cx.lexical.clone(),
                owner: Some(prefix),
                in_singleton: true,
                singleton_cref: true,
                // Defs in a `class <<` body bind on the singleton
                // (`singleton_def = in_singleton_class || …`) — which keeps
                // them OUT of the `Object` toplevel collapse (`class << self`
                // inside `Object.class_eval` leaves a bare call firing,
                // oracle `oss`/`ois`).
                defs_side: DefsSide::Singleton,
            };
            for &child in body {
                walk_defs(ast, declared, tables, visited, child, &inner);
            }
        }
        Node::Definition {
            name,
            receiver_def_name,
            singleton_name,
            def_receiver_path,
            ..
        } => {
            visited.insert(node);
            // A `def` leaf records under its effective owner and does NOT
            // descend: the reference's DefNode arm returns without walking the
            // body, so a `def` nested in a method body files nowhere.
            file_def(
                cx,
                name.as_deref(),
                receiver_def_name.as_deref(),
                singleton_name.as_deref(),
                def_receiver_path.as_deref(),
                tables,
            );
        }
        Node::Call {
            receiver,
            method,
            args,
            block_body,
            block_span,
            ..
        } => {
            // A literal block only: `X.class_eval(&blk)` passes a
            // `BlockArgumentNode` the reference does NOT treat as an eval body
            // (`receiver_eval_call?` requires a `BlockNode`). `block_span`
            // distinguishes the two — it is `None` for `&expr`.
            if block_span.is_some() && RECEIVER_EVAL_METHODS.contains(&method.as_str()) {
                // `walk_eval_methods_and_defs`: the receiver and arguments
                // evaluate in the ENCLOSING context; only the block body
                // rebinds.
                if let Some(r) = receiver {
                    walk_defs(ast, declared, tables, visited, *r, cx);
                }
                for &arg in args {
                    walk_defs(ast, declared, tables, visited, arg, cx);
                }
                let self_prefix = cx.self_prefix();
                let unnameable = cx.in_singleton
                    || cx.owner.as_ref().is_some_and(|o| o.is_empty())
                    || (cx.singleton_cref && self_prefix.is_empty());
                let eval_prefix = eval_receiver_prefix(
                    ast,
                    *receiver,
                    &self_prefix,
                    &cx.lexical,
                    unnameable,
                    declared,
                )
                .unwrap_or_default();
                let named = receiver.is_some_and(|r| {
                    !matches!(ast.get(r), Node::SelfExpr { .. })
                });
                let is_instance_eval = INSTANCE_EVAL_METHODS.contains(&method.as_str());
                // `eval_body_def_context`: the block's `in_singleton_class`
                // (call side) collapses to `in_singleton && !named` on this
                // subset; `defs_singleton` is `:unnameable` for an unnamed
                // `instance_eval` under an already-singleton self.
                let in_singleton_child = cx.in_singleton && !named;
                let defs_side = if is_instance_eval && cx.in_singleton && !named {
                    DefsSide::Unnameable
                } else if is_instance_eval || in_singleton_child {
                    DefsSide::Singleton
                } else {
                    DefsSide::Instance
                };
                let inner = DefCx {
                    lexical: cx.lexical.clone(),
                    owner: Some(eval_prefix),
                    in_singleton: in_singleton_child,
                    singleton_cref: cx.singleton_cref,
                    defs_side,
                };
                for &child in block_body {
                    walk_defs(ast, declared, tables, visited, child, &inner);
                }
                return;
            }
            // The anonymous factory arm (`walk_anonymous_meta_block`): a
            // `Class.new`/`Module.new`/`Struct.new`/`Data.define` literal block
            // NOT under a recognised constant write. Its defs belong to the
            // anonymous class — keyed by the synthetic name — and stay in
            // `<toplevel>` via `record_anonymous_body_def_as_toplevel`.
            if block_span.is_some() && is_meta_new_call(ast, *receiver, method) {
                if let Some(r) = receiver {
                    walk_defs(ast, declared, tables, visited, *r, cx);
                }
                for &arg in args {
                    walk_defs(ast, declared, tables, visited, arg, cx);
                }
                let inner = DefCx {
                    lexical: cx.lexical.clone(),
                    owner: Some(vec![ANONYMOUS_META_OWNER.to_string()]),
                    in_singleton: false,
                    singleton_cref: cx.singleton_cref,
                    defs_side: DefsSide::Instance,
                };
                for &child in block_body {
                    walk_defs(ast, declared, tables, visited, child, &inner);
                }
                return;
            }
            // `record_call_node_methods`: method-introducing macros file their
            // generated names under the owner — `X.class_eval { attr_reader :a }`
            // registers `X#a` (and `Object.class_eval { attr_reader :a }` keeps
            // `a` bare-callable), so a later `a` call is not unresolved.
            file_call_methods(ast, cx, method, *receiver, args, tables);
            if let Some(r) = receiver {
                walk_defs(ast, declared, tables, visited, *r, cx);
            }
            for &arg in args {
                walk_defs(ast, declared, tables, visited, arg, cx);
            }
            for &child in block_body {
                walk_defs(ast, declared, tables, visited, child, cx);
            }
        }
        Node::ConstantWrite { name, value, .. } => {
            // `K = Class.new { def m }` (`meta_new_block_split`): the factory
            // call's receiver and arguments keep the enclosing context; the
            // block's defs belong to the class the WRITE names — `K`, not the
            // enclosing owner — while `Module.nesting` stays lexical.
            if let Some(call_id) = meta_new_rvalue(ast, name, *value) {
                let Node::Call {
                    receiver,
                    args,
                    block_body,
                    ..
                } = ast.get(call_id)
                else {
                    unreachable!("meta_new_rvalue only returns calls");
                };
                if let Some(r) = receiver {
                    walk_defs(ast, declared, tables, visited, *r, cx);
                }
                for &arg in args {
                    walk_defs(ast, declared, tables, visited, arg, cx);
                }
                // A bare `K =` under an unnameable cref names nothing; its
                // block's defs file nowhere (`meta_ownerless`).
                let body_owner = if cx.singleton_cref {
                    Vec::new()
                } else {
                    qualify_vec(&cx.lexical, name)
                };
                let inner = DefCx {
                    lexical: cx.lexical.clone(),
                    owner: Some(body_owner),
                    in_singleton: false,
                    singleton_cref: cx.singleton_cref,
                    defs_side: DefsSide::Instance,
                };
                for &child in block_body {
                    walk_defs(ast, declared, tables, visited, child, &inner);
                }
            } else {
                walk_defs(ast, declared, tables, visited, *value, cx);
            }
        }
        Node::Alias {
            new_name, old_name, ..
        } => {
            // `record_alias_method` — the `alias` keyword registers the NEW
            // name under the effective owner exactly like `alias_method :n, :o`
            // does; `apply_alias_def_nodes` adoption rides `pending_aliases`
            // the same way (an `alias` of a `def` is per-file, an `alias` of a
            // core method stays in the cross-file seed). The reference returns
            // WITHOUT descending into the operands — a `def` buried in an
            // interpolated name files nowhere here.
            file_alias(cx, ast, *new_name, *old_name, tables);
        }
        node => {
            let mut children = Vec::new();
            def_walk_children(node, &mut children);
            for child in children {
                walk_defs(ast, declared, tables, visited, child, cx);
            }
        }
    }
}

/// File one `def`'s names under the effective owner — the port's combined
/// `record_def_method` / `record_def_node`. A `def` records under the def-owner
/// override when one exists, else the lexical prefix; an EMPTY effective owner
/// is `<toplevel>` for the instance names, and an explicit-empty override or
/// `:unnameable` side is NOWHERE (never `<toplevel>`).
fn file_def(
    cx: &DefCx,
    name: Option<&str>,
    receiver_def_name: Option<&str>,
    singleton_name: Option<&str>,
    def_receiver_path: Option<&str>,
    tables: &mut DefTables,
) {
    let _ = singleton_name; // always `:singleton` kind — see the filing below.
    if cx.defs_side == DefsSide::Unnameable {
        return;
    }
    let owner: &[String] = match &cx.owner {
        Some(o) if o.is_empty() => return,
        Some(o) => o,
        None => &cx.lexical,
    };
    if cx.singleton_cref && owner.is_empty() {
        return;
    }

    // `<toplevel>`: a bare `def m`, and — matching `record_def_node` filing
    // `def Foo.bar` under the toplevel key whenever its receiver does not
    // name `self` or the enclosing class — a receiver-bearing `def`.
    // `def self.x` is excluded (the reference's `def_singleton?` skip).
    // `def_nodes["<toplevel>"]` merges PROJECT-WIDE: a `def helper` in one
    // file resolves `helper` in a sibling (`top_level_def_for`).
    if owner.is_empty() {
        for nm in [name, receiver_def_name].into_iter().flatten() {
            tables.toplevel.insert(nm.to_string());
        }
        return;
    }

    // The anonymous-factory owner: its instance defs join `<toplevel>` too
    // (`record_anonymous_body_def_as_toplevel` — a `Module.new { def m }`
    // method still resolves a bare call, upstream #319).
    if owner.len() == 1 && owner[0] == ANONYMOUS_META_OWNER {
        for nm in [name, receiver_def_name].into_iter().flatten() {
            tables.toplevel.insert(nm.to_string());
        }
        return;
    }

    // `def_singleton?`'s last clause — `def_receiver_targets_lexical_self?`:
    // a receiver-bearing def whose rendered constant path equals the def-owner
    // prefix's tail is a SINGLETON def (`Object.class_eval { def Object.x }`
    // binds `Object.x`, never `Object#x`), so it is kept out of the instance
    // table entirely.
    let targets_self = def_receiver_path.is_some_and(|p| {
        let segs: Vec<&str> = p.split("::").collect();
        owner.len() >= segs.len() && owner[owner.len() - segs.len()..] == segs[..]
    });
    let key = owner.join("::");
    // `record_def_method`'s kind stamp: `:instance` for a receiver-less def on
    // the instance side and for a receiver-bearing def that does not name the
    // enclosing class; `:singleton` for `def self.x`, for anything under a
    // singleton-side body (`class <<`, `instance_eval`), and for the
    // self-targeting receiver form. The port's existence table answers
    // `:instance` queries only (the singleton branch of `check_call` never
    // consults it), so singleton-kind names file NOWHERE — the Blocking-3
    // fix: `class String; def self.m` / `Float.instance_eval { def m }` must
    // not suppress `"x".m` / `1.5.m` (reference + master both fire).
    if cx.defs_side != DefsSide::Instance {
        return;
    }
    let mut file_instance = |nm: &str| {
        // `def_names` — the name has a project `def` node, so it is per-file
        // only at the merge (`subtract_def_methods` drops it from the
        // cross-file `methods` seed; `file_methods` keeps it for THIS file).
        // `file_toplevel` — `Object`'s instance surface is the toplevel
        // surface (`source_declared_method?` consults `Object` for
        // implicit-self calls), but the same per-file rule holds: the
        // `Object` slice of `file_methods` is what a bare call in THIS file
        // resolves (oracle `oi_eval` / `oi_exec` / `osm2` / `oss` /
        // `helper_orecv` pin the singleton-side exclusions;
        // `helper_kern`/`helper_bo`/`dk_m`/`db_m` pin the missing
        // Kernel/BasicObject collapse).
        tables.def_names.entry(key.clone()).or_default().insert(nm.to_string());
        tables.file_methods.entry(key.clone()).or_default().insert(nm.to_string());
        if key == "Object" {
            tables.file_toplevel.insert(nm.to_string());
        }
    };
    if let Some(nm) = name {
        file_instance(nm);
    }
    if !targets_self {
        if let Some(nm) = receiver_def_name {
            file_instance(nm);
        }
    }
}

/// File one `alias new old` — the `AliasMethodNode` arm of
/// `record_alias_or_undef`. The NEW name joins the instance-kind existence
/// table under the effective owner whenever `new_name` is a literal symbol,
/// with the same owner/side gates as [`file_def`] (`unnameable`, explicit-
/// empty override, `qualified_prefix.empty?`, singleton-side), and the same
/// `pending_aliases` queueing as `alias_method`: an `old` that names a `def`
/// hands `new` its def node, which `subtract_def_methods` then strips from
/// the cross-file seed — a def-backed alias is per-file like the def it wraps,
/// while `alias zz upcase` (a core-method target) stays cross-file.
fn file_alias(
    cx: &DefCx,
    ast: &LoweredAst,
    new_name: NodeId,
    old_name: NodeId,
    tables: &mut DefTables,
) {
    if cx.defs_side == DefsSide::Unnameable {
        return;
    }
    let owner: &[String] = match &cx.owner {
        Some(o) if o.is_empty() => return,
        Some(o) => o,
        None => &cx.lexical,
    };
    if cx.singleton_cref && owner.is_empty() {
        return;
    }
    // `return if qualified_prefix.empty?` — a toplevel `alias` records nothing
    // in the reference's class-keyed table either.
    if owner.is_empty() {
        return;
    }
    // `in_singleton_class || defs_singleton` ⇒ `:singleton` kind — the port's
    // instance-side table files it nowhere, exactly like [`file_def`].
    if cx.defs_side != DefsSide::Instance {
        return;
    }
    // `record_alias_method` registers the new name only when it is a literal
    // `Prism::SymbolNode` — `literal_method_name` reads `SymbolLit`/`StringLit`.
    let Some(new) = literal_method_name(ast, new_name) else {
        return;
    };
    let key = owner.join("::");
    tables.macro_methods.entry(key.clone()).or_default().insert(new.clone());
    tables.file_methods.entry(key.clone()).or_default().insert(new.clone());
    if key == "Object" {
        tables.file_toplevel.insert(new.clone());
    }
    if let Some(old) = literal_method_name(ast, old_name) {
        tables.pending_aliases.push((key, new, old));
    }
}

/// The module-level accessor macros (ActiveSupport's `Module` extensions) —
/// `MODULE_ATTR_MACROS`, `[reader, writer, predicate]` per name.
fn module_attr_shape(method: &str) -> Option<(bool, bool, bool)> {
    match method {
        "mattr_reader" | "cattr_reader" => Some((true, false, false)),
        "mattr_writer" | "cattr_writer" => Some((false, true, false)),
        "mattr_accessor" | "cattr_accessor" => Some((true, true, false)),
        "class_attribute" => Some((true, true, true)),
        _ => None,
    }
}

/// A `SymbolLit`/`StringLit` argument's method name —
/// `literal_method_name`.
fn literal_method_name(ast: &LoweredAst, id: NodeId) -> Option<String> {
    match ast.get(id) {
        Node::SymbolLit { value, .. } | Node::StringLit { value, .. } => Some(value.clone()),
        _ => None,
    }
}

/// The `record_call_node_methods` half of the def walk's CallNode arm: the
/// names `define_method` / `attr_*` / `mattr_*` / `cattr_*` / `class_attribute`
/// / `alias_method` introduce, filed under the current owner. Every recorder
/// declines an empty owner prefix, so the toplevel set is untouched by a bare
/// `attr_reader` at file scope (exactly as the reference's
/// `return if qualified_prefix.empty?`). The `Object` collapse uses the
/// `in_singleton_class` kind the reference assigns these calls — instance
/// unless the position is already singleton-side (an `instance_eval` block
/// stays instance-side for its calls: only `def`/`alias` bind on the
/// singleton there).
fn file_call_methods(
    ast: &LoweredAst,
    cx: &DefCx,
    method: &str,
    receiver: Option<NodeId>,
    args: &[NodeId],
    tables: &mut DefTables,
) {
    let owner: &[String] = match &cx.owner {
        Some(o) if o.is_empty() => return,
        Some(o) => o,
        None => &cx.lexical,
    };
    if owner.is_empty() || (owner.len() == 1 && owner[0] == ANONYMOUS_META_OWNER) {
        return;
    }
    let key = owner.join("::");
    // `kind = in_singleton_class ? :singleton : :instance` — the module-attr
    // macros are the exception, recording BOTH kinds so they file under
    // either side (`record_module_attr_methods`). Instance-kind names join
    // BOTH the file's own overlay and the cross-file macro table — unlike a
    // `def`, an accessor survives `subtract_def_methods` unless a project
    // `def` shares its name — and under an `Object` owner they stay
    // bare-callable through `file_toplevel` (the per-file half; the merge
    // adds the cross-file half).
    let singleton_side = cx.in_singleton;
    let mut file = |name: String, both_kinds: bool| {
        if singleton_side && !both_kinds {
            return;
        }
        tables.macro_methods.entry(key.clone()).or_default().insert(name.clone());
        tables.file_methods.entry(key.clone()).or_default().insert(name.clone());
        if key == "Object" {
            tables.file_toplevel.insert(name);
        }
    };

    if method == "define_method" {
        // `record_define_method` — no receiver check upstream: the call is the
        // macro wherever it appears under a named owner.
        if let Some(name) = args.first().and_then(|a| literal_method_name(ast, *a)) {
            file(name, false);
        }
        return;
    }
    if receiver.is_some() {
        return; // `attr_*` / module-attr / `alias_method` are implicit-self macros.
    }
    if matches!(method, "attr_reader" | "attr_writer" | "attr_accessor") {
        let reader = method != "attr_writer";
        let writer = method != "attr_reader";
        for &arg in args {
            let Some(base) = literal_method_name(ast, arg) else {
                continue;
            };
            if reader {
                file(base.clone(), false);
            }
            if writer {
                file(format!("{base}="), false);
            }
        }
        return;
    }
    if let Some((reader, writer, predicate)) = module_attr_shape(method) {
        for &arg in args {
            let Some(base) = literal_method_name(ast, arg) else {
                continue;
            };
            if reader {
                file(base.clone(), true);
            }
            if writer {
                file(format!("{base}="), true);
            }
            if predicate {
                file(format!("{base}?"), true);
            }
        }
        return;
    }
    // `record_alias_method_call` — `alias_method :new, :old` registers the NEW
    // name as a call-introduced method (the `alias` keyword is a different
    // node kind the lowered tree does not preserve as a call). When `old` is
    // a `def`, `apply_alias_def_nodes` gives the new name `old`'s def node —
    // which `subtract_def_methods` strips cross-file — so the pair is queued
    // for resolution against the COMPLETE per-file def table.
    if method == "alias_method" {
        let mut names = args.iter().filter_map(|a| literal_method_name(ast, *a));
        if let (Some(new), Some(old)) = (names.next(), names.next()) {
            if !singleton_side {
                tables.macro_methods.entry(key.clone()).or_default().insert(new.clone());
                tables.file_methods.entry(key.clone()).or_default().insert(new.clone());
                if key == "Object" {
                    tables.file_toplevel.insert(new.clone());
                }
                tables.pending_aliases.push((key.clone(), new, old));
            }
        }
    }
}

/// The `class << <expr>` body's def-owner prefix — `singleton_body_prefix` /
/// `singleton_class_prefix`. `class << self` names the enclosing self's
/// prefix; `class << Const` resolves like an eval receiver; `self::X` keeps
/// the enclosing-self + tail resolution (declining under an unnameable self);
/// anything else (`class << obj`, `class << expr()`) names nothing.
fn singleton_operand_prefix(
    ast: &LoweredAst,
    operand: Option<NodeId>,
    cx: &DefCx,
    declared: &HashSet<String>,
) -> Vec<String> {
    let Some(op) = operand else {
        return Vec::new();
    };
    match ast.get(op) {
        Node::SelfExpr { .. } => cx.self_prefix(),
        Node::ConstantRead {
            name,
            self_anchored: true,
            ..
        } => {
            let self_prefix = cx.self_prefix();
            if cx.singleton_cref && self_prefix.is_empty() {
                Vec::new()
            } else {
                let mut prefix = self_prefix;
                prefix.extend(name.split("::").map(str::to_string));
                collapse_object_owner(prefix)
            }
        }
        Node::ConstantRead {
            name,
            dynamic_base: false,
            rooted,
            ..
        } => eval_const_prefix(name, *rooted, &cx.lexical, declared),
        _ => Vec::new(),
    }
}

/// `eval_receiver_prefix`: the owner a `*eval`/`*exec` block rebinds `self`
/// to. `None` when the receiver names nothing the file can see (a local, a
/// call result, a dynamic constant) — the block's defs then file NOWHERE, not
/// toplevel.
fn eval_receiver_prefix(
    ast: &LoweredAst,
    receiver: Option<NodeId>,
    self_prefix: &[String],
    lexical: &[String],
    unnameable: bool,
    declared: &HashSet<String>,
) -> Option<Vec<String>> {
    // `return self_prefix if receiver.nil? || receiver.is_a?(SelfNode)` — a
    // BARE `class_eval` keeps the enclosing self exactly like `self.class_eval`
    // (`Object.class_eval { class_eval { def m } }` still lands on `Object`,
    // while the same bare eval at toplevel names nothing and files nowhere).
    let Some(receiver) = receiver else {
        return Some(self_prefix.to_vec());
    };
    match ast.get(receiver) {
        Node::SelfExpr { .. } => Some(self_prefix.to_vec()),
        Node::ConstantRead {
            name,
            self_anchored: true,
            ..
        } => {
            // `self::Foo` names the enclosing self's path; when that self is
            // unnameable the read raises at runtime, so the receiver declines.
            if unnameable {
                None
            } else {
                let mut prefix = self_prefix.to_vec();
                prefix.extend(name.split("::").map(str::to_string));
                Some(collapse_object_owner(prefix))
            }
        }
        Node::ConstantRead {
            name,
            dynamic_base: false,
            rooted,
            ..
        } => Some(eval_const_prefix(name, *rooted, lexical, declared)),
        _ => None,
    }
}

/// The constant-receiver tail of `eval_receiver_prefix` —
/// `eval_constant_receiver_prefix`. `rendered` is already the lenient name.
/// `rooted` is `Source::ConstantPath.rooted?`: a `::`-rooted spelling names
/// the TOP LEVEL — `::B` inside `class A::B` is still `B` — so the root check
/// precedes BOTH the self-reopen shortcut and the lexical walk (the reference
/// checks `rooted?(path_node)` first; [`Node::ConstantRead`] carries the flag
/// the `::` spelling lowered with). A receiver spelling the lexically
/// enclosing class keeps the enclosing prefix; otherwise the first
/// `<nesting>::<first segment>` the file's own declared constants answer,
/// innermost first, wins — falling back to the name as written (never a
/// lexical guess).
fn eval_const_prefix(
    rendered: &str,
    rooted: bool,
    lexical: &[String],
    declared: &HashSet<String>,
) -> Vec<String> {
    let segments: Vec<&str> = rendered.split("::").collect();
    if rooted || lexical.is_empty() {
        return segments.iter().map(|s| (*s).to_string()).collect();
    }
    if lexical.last().map(String::as_str) == Some(rendered) {
        return lexical.to_vec();
    }
    // `lexical_nesting_for_prefix`: innermost rung first — for ["M", "A::B"]
    // the rungs are "M::A::B" then "M".
    for n in (1..=lexical.len()).rev() {
        let entry = lexical[..n].join("::");
        let candidate = format!("{entry}::{}", segments[0]);
        if declared.contains(&candidate) {
            let mut prefix: Vec<String> = candidate.split("::").map(str::to_string).collect();
            prefix.extend(segments[1..].iter().map(|s| (*s).to_string()));
            return prefix;
        }
    }
    segments.iter().map(|s| (*s).to_string()).collect()
}

/// `collapse_object_owner` — a `self::`-anchored receiver under `Object`
/// drops the `Object` segment (`Object`'s constants ARE the toplevel
/// constants).
fn collapse_object_owner(prefix: Vec<String>) -> Vec<String> {
    if prefix.first().map(String::as_str) == Some("Object") {
        prefix[1..].to_vec()
    } else {
        prefix
    }
}

/// `meta_new_call?`: `Class.new`/`Module.new`/`Struct.new`/`Data.define` on
/// the bare constant receiver — a dynamic (`expr::Class`) or `self::`-rooted
/// base does not name the factory, matching the reference's constant-receiver
/// requirement.
fn is_meta_new_call(ast: &LoweredAst, receiver: Option<NodeId>, method: &str) -> bool {
    let Some(recv) = receiver else {
        return false;
    };
    let Node::ConstantRead {
        name,
        dynamic_base: false,
        self_anchored: false,
        ..
    } = ast.get(recv)
    else {
        return false;
    };
    META_NEW_SELECTORS
        .iter()
        .any(|&(k, m)| k == name.as_str() && m == method)
}

/// `meta_new_rvalue`: the write-target recognition for `K = Class.new do …
/// end`. The reference accepts a direct factory call, a `K = K || Factory`
/// fallback (`Left or SameConst` — Prism `||`), and a repeated receiverful
/// `.freeze` tail (`K = Class.new { }.freeze.freeze`). Returns the factory
/// CALL's id so the arm can split receiver/args (enclosing context) from the
/// block (the written owner).
fn meta_new_rvalue(ast: &LoweredAst, write_name: &str, mut value: NodeId) -> Option<NodeId> {
    // `K = K || <factory>` — an `||` whose left operand reads the written
    // constant.
    if let Node::Logical {
        left,
        right,
        is_and: false,
        ..
    } = ast.get(value)
    {
        if let Node::ConstantRead {
            name,
            dynamic_base: false,
            self_anchored: false,
            ..
        } = ast.get(*left)
        {
            if name == write_name {
                value = *right;
            }
        }
    }
    // A `.freeze` tail, repeated — only a receiverful, argument-less,
    // block-less call unwraps.
    loop {
        match ast.get(value) {
            Node::Call {
                receiver: Some(recv),
                method,
                args,
                block_body,
                ..
            } if method == "freeze" && args.is_empty() && block_body.is_empty() => {
                value = *recv;
            }
            _ => break,
        }
    }
    let Node::Call {
        receiver,
        method,
        block_span,
        ..
    } = ast.get(value)
    else {
        return None;
    };
    if block_span.is_some() && is_meta_new_call(ast, *receiver, method) {
        Some(value)
    } else {
        None
    }
}

/// `qualify` for an already-segmented write target: `K =` inside `Outer`
/// names `Outer::K`.
fn qualify_vec(lexical: &[String], name: &str) -> Vec<String> {
    let mut out = lexical.to_vec();
    out.extend(name.split("::").map(str::to_string));
    out
}

/// A span-indexed context transformer for the orphan post-pass: each variant
/// is the arena-side residue of one [`walk_defs`] arm, recovered by span
/// containment instead of child edges.
enum DefFrame {
    /// `class`/`module` header — `decl_body_cx`: pushes the path onto
    /// `lexical`, RESETS it when `::`-rooted, or rides the rebound self when
    /// `self::`-anchored.
    Cref {
        name: String,
        rooted: bool,
        self_anchored: bool,
    },
    /// `class << <operand>` — singleton context for the body; a `def` inside
    /// the OPERAND's span keeps the enclosing context instead.
    SingletonBody {
        operand: Option<NodeId>,
        operand_span: Option<Span>,
    },
    /// A `*eval`/`*exec` literal-block body — rebinds `self` to the receiver.
    EvalBody {
        receiver: Option<NodeId>,
        method: String,
        block: Span,
    },
    /// An anonymous `Class.new`/`Module.new`/`Struct.new`/`Data.define`
    /// literal-block body — defs stay toplevel (`ANONYMOUS_META_OWNER`).
    MetaBlock { call: NodeId, block: Span },
    /// `K = Class.new { … }` — the block's defs belong to `K`, not the
    /// anonymous owner; shadows the [`DefFrame::MetaBlock`] for the same call.
    MetaWrite { name: String, block: Span, call: NodeId },
    /// Any `def` subtree — the reference's DefNode arm never descends, so a
    /// def ANYWHERE inside another def's span files nowhere.
    DefBody,
}

/// Blocking-1 post-pass — file every `Definition` node lowered into the arena
/// but never reached by [`walk_defs`]'s edge following. The reference's
/// `compact_child_nodes` traversal reaches EVERY Prism child; the lowered
/// tree's recoverable-child list omits positions the lowering keeps outside
/// its child slots — range endpoints, `def` receiver expressions, parameter
/// defaults, dynamic constant-path parents — so a `def` in one of those
/// positions silently vanished from every def table (issue #141 blocker 1).
/// The context that `walk_defs` would have built is recovered SPAN-WISE: the
/// context-shaping ancestors of a `def` are exactly the nodes whose spans
/// strictly contain it, applied outermost-first.
pub(crate) fn file_orphan_defs(
    ast: &LoweredAst,
    declared: &HashSet<String>,
    visited: &HashSet<NodeId>,
    tables: &mut DefTables,
) {
    // Frame collection — one arena pass. `(outer span for containment and
    // outermost-first ordering, transform)`.
    let mut frames: Vec<(Span, DefFrame)> = Vec::new();
    let mut orphans: Vec<NodeId> = Vec::new();
    for (id, node) in ast.iter() {
        match node {
            Node::ClassDef {
                name,
                rooted,
                self_anchored,
                ..
            }
            | Node::ModuleDef {
                name,
                rooted,
                self_anchored,
                ..
            } if !name.is_empty() =>
            {
                frames.push((
                    node.span(),
                    DefFrame::Cref {
                        name: name.clone(),
                        rooted: *rooted,
                        self_anchored: *self_anchored,
                    },
                ));
            }
            Node::Definition {
                is_singleton_class: true,
                singleton_operand,
                ..
            } => {
                let operand_span = singleton_operand.map(|op| ast.get(op).span());
                frames.push((
                    node.span(),
                    DefFrame::SingletonBody {
                        operand: *singleton_operand,
                        operand_span,
                    },
                ));
            }
            Node::Definition { .. } => {
                frames.push((node.span(), DefFrame::DefBody));
                if !visited.contains(&id) {
                    orphans.push(id);
                }
            }
            Node::Call {
                receiver,
                method,
                block_span: Some(block),
                ..
            } => {
                if RECEIVER_EVAL_METHODS.contains(&method.as_str()) {
                    frames.push((
                        node.span(),
                        DefFrame::EvalBody {
                            receiver: *receiver,
                            method: method.clone(),
                            block: *block,
                        },
                    ));
                } else if is_meta_new_call(ast, *receiver, method) {
                    frames.push((
                        node.span(),
                        DefFrame::MetaBlock {
                            call: id,
                            block: *block,
                        },
                    ));
                }
            }
            Node::ConstantWrite { name, value, .. } => {
                if let Some(call_id) = meta_new_rvalue(ast, name, *value) {
                    if let Node::Call {
                        block_span: Some(block),
                        ..
                    } = ast.get(call_id)
                    {
                        frames.push((
                            node.span(),
                            DefFrame::MetaWrite {
                                name: name.clone(),
                                block: *block,
                                call: call_id,
                            },
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    if orphans.is_empty() {
        return;
    }
    let contains = |outer: Span, inner: Span| outer.0 <= inner.0 && inner.1 <= outer.1;
    for id in orphans {
        let Node::Definition {
            name,
            receiver_def_name,
            singleton_name,
            def_receiver_path,
            ..
        } = ast.get(id)
        else {
            unreachable!("orphans only collects Definition nodes");
        };
        let dspan = ast.get(id).span();
        let mut enclosing: Vec<&(Span, DefFrame)> = frames
            .iter()
            .filter(|(outer, _)| *outer != dspan && contains(*outer, dspan))
            .collect();
        // Outermost-first: earlier start wins; equal starts take the LARGER
        // end (a strictly containing span always starts no later and ends no
        // sooner than its interior).
        enclosing.sort_by_key(|(outer, _)| (outer.0, std::cmp::Reverse(outer.1)));
        // A def anywhere inside another def's subtree files nowhere — the
        // reference's DefNode arm returns without walking children.
        if enclosing
            .iter()
            .any(|(_, k)| matches!(k, DefFrame::DefBody))
        {
            continue;
        }
        let mut cx = def_root_cx();
        // Constant writes whose meta-call frame is shadowed — applying the
        // `K =` owner instead of the anonymous factory's.
        let mut meta_writes: HashSet<NodeId> = HashSet::new();
        for (_, kind) in enclosing {
            match kind {
                DefFrame::DefBody => unreachable!("def-in-def filtered above"),
                DefFrame::Cref {
                    name,
                    rooted,
                    self_anchored,
                } => {
                    cx = decl_body_cx(&cx, name, *rooted, *self_anchored);
                }
                DefFrame::SingletonBody {
                    operand,
                    operand_span,
                } => {
                    if operand_span.is_some_and(|s| contains(s, dspan)) {
                        continue; // a def in the operand keeps the enclosing cx.
                    }
                    let prefix = singleton_operand_prefix(ast, *operand, &cx, declared);
                    cx = DefCx {
                        lexical: cx.lexical.clone(),
                        owner: Some(prefix),
                        in_singleton: true,
                        singleton_cref: true,
                        defs_side: DefsSide::Singleton,
                    };
                }
                DefFrame::EvalBody {
                    receiver,
                    method,
                    block,
                } => {
                    if !contains(*block, dspan) {
                        continue; // receiver/args keep the enclosing cx.
                    }
                    let self_prefix = cx.self_prefix();
                    let unnameable = cx.in_singleton
                        || cx.owner.as_ref().is_some_and(|o| o.is_empty())
                        || (cx.singleton_cref && self_prefix.is_empty());
                    let eval_prefix = eval_receiver_prefix(
                        ast,
                        *receiver,
                        &self_prefix,
                        &cx.lexical,
                        unnameable,
                        declared,
                    )
                    .unwrap_or_default();
                    let named = receiver
                        .is_some_and(|r| !matches!(ast.get(r), Node::SelfExpr { .. }));
                    let is_instance_eval = INSTANCE_EVAL_METHODS.contains(&method.as_str());
                    let in_singleton_child = cx.in_singleton && !named;
                    let defs_side = if is_instance_eval && cx.in_singleton && !named {
                        DefsSide::Unnameable
                    } else if is_instance_eval || in_singleton_child {
                        DefsSide::Singleton
                    } else {
                        DefsSide::Instance
                    };
                    cx = DefCx {
                        lexical: cx.lexical.clone(),
                        owner: Some(eval_prefix),
                        in_singleton: in_singleton_child,
                        singleton_cref: cx.singleton_cref,
                        defs_side,
                    };
                }
                DefFrame::MetaWrite { name, block, call } => {
                    if !contains(*block, dspan) {
                        continue;
                    }
                    let body_owner = if cx.singleton_cref {
                        Vec::new()
                    } else {
                        qualify_vec(&cx.lexical, name)
                    };
                    meta_writes.insert(*call);
                    cx = DefCx {
                        lexical: cx.lexical.clone(),
                        owner: Some(body_owner),
                        in_singleton: false,
                        singleton_cref: cx.singleton_cref,
                        defs_side: DefsSide::Instance,
                    };
                }
                DefFrame::MetaBlock { call, block } => {
                    if meta_writes.contains(call) || !contains(*block, dspan) {
                        continue;
                    }
                    cx = DefCx {
                        lexical: cx.lexical.clone(),
                        owner: Some(vec![ANONYMOUS_META_OWNER.to_string()]),
                        in_singleton: false,
                        singleton_cref: cx.singleton_cref,
                        defs_side: DefsSide::Instance,
                    };
                }
            }
        }
        file_def(
            &cx,
            name.as_deref(),
            receiver_def_name.as_deref(),
            singleton_name.as_deref(),
            def_receiver_path.as_deref(),
            tables,
        );
    }
}

/// Every child id the generic walk descends — the lowered tree's recoverable
/// children for the variants that do not get a dedicated arm. Mirrors the
/// reference's `node.compact_child_nodes.each` fallback: anything it does not
/// explicitly context-shift keeps the enclosing context.
fn def_walk_children(node: &Node, out: &mut Vec<NodeId>) {
    match node {
        Node::Program { body, .. } | Node::Statements { body, .. } => {
            out.extend_from_slice(body);
        }
        Node::LocalVariableWrite { value, .. }
        | Node::LocalVariableOpWrite { value, .. }
        | Node::VariableWrite { value, .. }
        | Node::InstanceVariableWrite { value, .. } => out.push(*value),
        Node::MultiWrite {
            value, target_exprs, ..
        } => {
            out.push(*value);
            out.extend_from_slice(target_exprs);
        }
        Node::IndexWrite {
            receiver,
            indices,
            value,
            ..
        } => {
            out.extend(receiver.iter().copied());
            out.extend_from_slice(indices);
            out.push(*value);
        }
        Node::InterpolatedString { parts, .. } | Node::InterpolatedSymbol { parts, .. } => {
            out.extend_from_slice(parts);
        }
        Node::Call {
            receiver,
            args,
            block_body,
            ..
        } => {
            if let Some(r) = receiver {
                out.push(*r);
            }
            out.extend_from_slice(args);
            out.extend_from_slice(block_body);
        }
        Node::Definition { body, .. }
        | Node::ClassDef { body, .. }
        | Node::ModuleDef { body, .. } => out.extend_from_slice(body),
        Node::If {
            predicate,
            then_body,
            else_body,
            ..
        } => {
            out.push(*predicate);
            out.extend_from_slice(then_body);
            out.extend_from_slice(else_body);
        }
        Node::Case {
            predicate,
            branches,
            else_body,
            ..
        } => {
            if let Some(p) = predicate {
                out.push(*p);
            }
            out.extend_from_slice(branches);
            out.extend_from_slice(else_body);
        }
        Node::When {
            conditions, body, ..
        } => {
            out.extend_from_slice(conditions);
            out.extend_from_slice(body);
        }
        Node::Loop {
            predicate, body, ..
        } => {
            if let Some(p) = predicate {
                out.push(*p);
            }
            out.extend_from_slice(body);
        }
        Node::BeginRescue {
            body,
            ensure_body,
            clauses,
            ..
        } => {
            out.extend_from_slice(body);
            out.extend_from_slice(ensure_body);
            for c in clauses {
                out.extend_from_slice(&c.exceptions);
                out.extend_from_slice(&c.body);
            }
        }
        Node::Lambda { body, .. } | Node::Return { values: body, .. } => {
            out.extend_from_slice(body);
        }
        Node::Logical { left, right, .. } => {
            out.push(*left);
            out.push(*right);
        }
        Node::ArrayLit { elements, .. } | Node::HashLit { elements, .. } => {
            out.extend_from_slice(elements);
        }
        _ => {}
    }
}

/// The file's own qualified constant-name census —
/// `eval_file_declared_names` (`collect_declared_constant_names`): every
/// `class`/`module` header and `CONST =` write's qualified name, keyed so
/// `eval_const_prefix` can tell `<nesting>::X` resolvable rungs from
/// as-written guesses. `def` bodies are skipped (a constant declaration
/// inside one is a SyntaxError); bare declarations under an unnameable cref
/// (`class <<`, anonymous factory blocks) name nothing, while explicit-base
/// paths still re-anchor lexically.
pub(crate) fn collect_declared_names(ast: &LoweredAst) -> HashSet<String> {
    let mut out = HashSet::new();
    collect_declared_names_at(ast, ast.root(), &[], false, &mut out);
    out
}

/// `add_declared_name` — every enclosing prefix joins the census: a file
/// declaring `S::A::B` necessarily has `S` and `S::A` to declare it under, so
/// an `A::B` eval receiver inside `class S` resolves `S::A::B`.
fn add_declared(out: &mut HashSet<String>, prefix: &[String]) {
    for i in 1..=prefix.len() {
        out.insert(prefix[..i].join("::"));
    }
}

fn collect_declared_names_at(
    ast: &LoweredAst,
    node: NodeId,
    prefix: &[String],
    unnameable_cref: bool,
    out: &mut HashSet<String>,
) {
    match ast.get(node) {
        Node::ClassDef {
            name,
            rooted,
            self_anchored,
            body,
            ..
        }
        | Node::ModuleDef {
            name,
            rooted,
            self_anchored,
            body,
            ..
        } => {
            if name.is_empty() {
                for &child in body {
                    collect_declared_names_at(ast, child, prefix, unnameable_cref, out);
                }
                return;
            }
            // `declared_constant_path_prefix`: a `::`-rooted header re-anchors
            // at the top level (always named, even below an unnameable cref);
            // a bare or `self::` header under an unnameable cref names NOTHING
            // (nil — the body keeps the enclosing prefix); every other path
            // qualifies lexically.
            let declines =
                !*rooted && unnameable_cref && (*self_anchored || !name.contains("::"));
            let child_prefix: Vec<String> = if *rooted {
                name.split("::").map(str::to_string).collect()
            } else if declines {
                prefix.to_vec()
            } else {
                qualify_vec(prefix, name)
            };
            if !declines {
                add_declared(out, &child_prefix);
            }
            for &child in body {
                collect_declared_names_at(ast, child, &child_prefix, unnameable_cref, out);
            }
        }
        Node::Definition {
            is_singleton_class: true,
            singleton_operand,
            body,
            ..
        } => {
            // `class <<` — the operand is inspected; the body's cref is the
            // unnameable singleton.
            if let Some(op) = singleton_operand {
                collect_declared_names_at(ast, *op, prefix, unnameable_cref, out);
            }
            for &child in body {
                collect_declared_names_at(ast, child, prefix, true, out);
            }
        }
        Node::Definition { .. } => {} // a `def` body cannot declare constants.
        Node::ConstantWrite { name, value, .. } => {
            if !unnameable_cref && !name.is_empty() {
                add_declared(out, &qualify_vec(prefix, name));
            }
            if let Some(call_id) = meta_new_rvalue(ast, name, *value) {
                // `collect_rvalue_declared_names`: the factory's non-block
                // children keep the enclosing context; the block's cref is the
                // written class.
                if let Node::Call {
                    receiver,
                    args,
                    block_body,
                    ..
                } = ast.get(call_id)
                {
                    if let Some(r) = receiver {
                        collect_declared_names_at(ast, *r, prefix, unnameable_cref, out);
                    }
                    for &arg in args {
                        collect_declared_names_at(ast, arg, prefix, unnameable_cref, out);
                    }
                    let block_prefix = if unnameable_cref {
                        prefix.to_vec()
                    } else {
                        qualify_vec(prefix, name)
                    };
                    for &child in block_body {
                        collect_declared_names_at(ast, child, &block_prefix, unnameable_cref, out);
                    }
                }
            } else {
                collect_declared_names_at(ast, *value, prefix, unnameable_cref, out);
            }
        }
        Node::Call {
            receiver,
            method,
            args,
            block_body,
            block_span,
            ..
        } if block_span.is_some() && is_meta_new_call(ast, *receiver, method) => {
            // `collect_declared_anonymous_factory?`: an anonymous factory's
            // non-block children keep context; the block's cref is the
            // unnameable anonymous class.
            if let Some(r) = receiver {
                collect_declared_names_at(ast, *r, prefix, unnameable_cref, out);
            }
            for &arg in args {
                collect_declared_names_at(ast, arg, prefix, unnameable_cref, out);
            }
            for &child in block_body {
                collect_declared_names_at(ast, child, prefix, true, out);
            }
        }
        node => {
            let mut children = Vec::new();
            def_walk_children(node, &mut children);
            for child in children {
                collect_declared_names_at(ast, child, prefix, unnameable_cref, out);
            }
        }
    }
}
