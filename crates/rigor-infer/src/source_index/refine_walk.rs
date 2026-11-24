//! The port of the reference's `RefineSelf` context walk
//! (`inference/refine_self.rb`) fused with the refine census' ownership
//! rules (`scope_indexer.rb` — `record_literal_target_refine` /
//! `record_deferred_refine` / `account_other_refine` /
//! `record_refine_block`, rigor-rs#446). For every `refine(T) { … }` call a
//! file holds, the walk answers:
//!
//! * which module it refines FOR — a module body (`refine` is a module's own
//!   method there), a `def self.x` or `def` inside that module's
//!   `class << self` (its own methods), or a keeps-self block under either,
//!   yields the module's name; a `def M.x` or `define_singleton_method`
//!   block yields the same module but with an unreadable target; an
//!   instance `def`, a `define_method` block, a `def <other>.x`, a lambda,
//!   a non-keeps-self block, an eval or `Module.new` body, or any of those
//!   inside a class yields the wildcard `*` the census records against every
//!   `using`; a class body, `class << x` or `Class.new`/`Struct.new`/
//!   `Data.define` body records nothing (a `refine` where `self` is a class
//!   is the class's own method, #1689); and a top-level `refine` records
//!   rows under `""`, which no `using` ever activates;
//! * what its BLOCK puts in effect (`record_refine_block`): the charged
//!   owner's names — `Some(module)` — over the block's span, or the
//!   wildcard `*` — `None` — for a context the walk cannot name (the
//!   wildcard name is opaque: the block declines every declined rule
//!   inside it). A `refine` nested under another `refine`'s block marks the
//!   outer body incomplete and records a wildcard-owned activation of its
//!   own — the census' `:nested`, never a table row.
//!
//! Every place the port cannot tell one context from the next records the
//! wildcard row — the census' own direction, and `check`'s safe side
//! (declining more, never less).

use rigor_parse::ast::{LoweredAst, Node, NodeId, Span};

use super::Refinement;

/// The refinement rows and activation regions one file contributes.
pub struct RefineScan {
    /// `module name -> its refinement rows` for the modules this file
    /// declares (own-method and targets-wildcard rows alike).
    pub(crate) rows: Vec<(String, Refinement)>,
    /// A `refine` sat in a context the walk cannot name — the census'
    /// `{*:{*:[*]}}` row, which makes every in-effect module opaque: every
    /// `using`/`refined` region in the project then declines every
    /// refinement-declined rule.
    pub(crate) global_wildcard: bool,
    /// `(block span, charged owner)` for every `refine … {}` that records a
    /// refine-block activation — `None` when the owner is the wildcard
    /// (the `*` name is opaque: everything inside the block declines).
    pub block_regions: Vec<(Span, Option<String>)>,
}

/// The walk's `self` context — a subset of `RefineSelf`'s context record.
#[derive(Clone)]
enum Ctx {
    /// File top level (`MAIN`): a `refine` here records under `""`.
    Top,
    /// A module's own context — its body, `def self.x`, a `def` inside its
    /// `class << self`, or a keeps-self block under any of those: real
    /// `T -> names` rows for `name`.
    Named(String),
    /// A context charged to `name` whose target the census cannot read:
    /// `def M.x` or a `define_singleton_method` block under it — the
    /// `{*:{*:[name]}}` row.
    WildNamed(String),
    /// A `class << self` body of `name` — `self` is the class object: a
    /// `refine` here is a DSL (records nothing), but its `def`s are the
    /// module's own methods.
    Singleton(String),
    /// The wildcard context: `{*:{*:[*]}}`.
    Wild,
    /// A class's own context — `class`, `Class.new`, `Struct.new`,
    /// `Data.define` bodies: a `refine` is a DSL, nothing (its `self` is a
    /// class, and `Class` undefines `refine`, so the call is the class's
    /// own method, #1689) — and so is every method inside it, whose `self`
    /// is a class or one of its instances, never a module the census could
    /// charge.
    Classy,
    /// A class context whose INSTANCE side is the wildcard — a
    /// `class << x` body or a `class X < Module` body: a `refine` directly
    /// here is still the DSL (nothing), but the methods inside run on an
    /// object the walk cannot name, so a `refine` in them charges the
    /// global wildcard.
    ClassyWild,
}

/// `RefineSelf::CORE_ITERATORS` — the core iteration methods whose block
/// keeps the caller's `self` when the receiver is a literal this file
/// holds (`refine_self.rb`). The reference also keeps `self` for a
/// literal-BOUND named receiver; the port cannot read that binding, so a
/// named receiver declines to the wildcard context — the safe direction.
const KEEPS_SELF_METHODS: &[&str] = &[
    "each",
    "each_with_index",
    "each_with_object",
    "each_pair",
    "each_key",
    "each_value",
    "map",
    "flat_map",
    "times",
    "select",
    "reject",
];

/// The calls whose block the walk cannot keep `self` through at all —
/// `SELF_REBINDING_EVAL_CALLS` + `INSTANCE_EVAL_CALLS`.
const EVAL_BLOCK_CALLS: &[&str] = &[
    "class_eval",
    "module_eval",
    "class_exec",
    "module_exec",
    "instance_eval",
    "instance_exec",
];

/// Scan `ast` for every `refine` call's census row and block region.
pub fn scan_refinements(ast: &LoweredAst) -> RefineScan {
    let mut scan = RefineScan {
        rows: Vec::new(),
        global_wildcard: false,
        block_regions: Vec::new(),
    };
    let mut prefix: Vec<String> = Vec::new();
    walk(ast, ast.root(), &Ctx::Top, &mut prefix, &mut scan);
    scan
}

fn walk(
    ast: &LoweredAst,
    id: NodeId,
    ctx: &Ctx,
    prefix: &mut Vec<String>,
    scan: &mut RefineScan,
) {
    match ast.get(id) {
        Node::Program { body, .. } | Node::Statements { body, .. } => {
            for &child in body {
                walk(ast, child, ctx, prefix, scan);
            }
        }
        Node::ModuleDef { name, body, .. } => {
            if name.is_empty() {
                // An un-nameable module: the census charges it by position
                // only where a `Module.new` write names it — nowhere else —
                // so a `refine` inside declines to the wildcard.
                for &child in body {
                    walk(ast, child, &Ctx::Wild, prefix, scan);
                }
                return;
            }
            let qualified = qualify_ctx(prefix, name);
            prefix.push(name.clone());
            for &child in body {
                walk(ast, child, &Ctx::Named(qualified.clone()), prefix, scan);
            }
            prefix.pop();
        }
        Node::ClassDef {
            name,
            body,
            superclass_path,
            ..
        } => {
            let pushed = !name.is_empty();
            if pushed {
                prefix.push(name.clone());
            }
            // `class_body(module_subclass)`: a plain class's instance side
            // is {CLASS} — a `refine` in its methods is the class's own DSL
            // and records nothing; a `Module` subclass's instances are
            // modules the walk cannot name, so its methods charge the
            // global wildcard (only the bare/`::`-rooted `Module`
            // superclass counts — `M::Module` does not).
            let class_ctx =
                if superclass_path.as_deref() == Some("Module") {
                    Ctx::ClassyWild
                } else {
                    Ctx::Classy
                };
            for &child in body {
                walk(ast, child, &class_ctx, prefix, scan);
            }
            if pushed {
                prefix.pop();
            }
        }
        Node::Definition {
            is_singleton_class: true,
            singleton_operand,
            body,
            ..
        } => {
            // `class << self` keeps the module's name for the `def`s it
            // holds (the reference's `runs_on` is the parent's singleton
            // side: the module under a module body, {CLASS} under a class
            // body, {MAIN} at the top level, the wildcard under a wildcard
            // context). A `class << x` body runs on whatever `x` is — the
            // walk cannot name it, so its methods charge the global
            // wildcard, while a `refine` directly here is still the class's
            // own DSL.
            let self_op = matches!(
                singleton_operand.map(|op| ast.get(op)),
                Some(Node::SelfExpr { .. })
            );
            let inner = match ctx {
                Ctx::Named(owner) if self_op => Ctx::Singleton(owner.clone()),
                Ctx::Classy | Ctx::ClassyWild | Ctx::Singleton(_) if self_op => Ctx::Classy,
                Ctx::Top if self_op => Ctx::Top,
                Ctx::Wild | Ctx::WildNamed(_) if self_op => ctx.clone(),
                _ => Ctx::ClassyWild,
            };
            for &child in body {
                walk(ast, child, &inner, prefix, scan);
            }
        }
        Node::Definition {
            name: Some(_), body, ..
        } => {
            // An instance `def`: its context's `here` is the parent's
            // INSTANCE side — the module's own method only inside its
            // `class << self` (the module), a class or one of its
            // instances inside a class body ({CLASS} — the refine there is
            // the class's own DSL and records nothing, #1689), and
            // whatever object extends the owner anywhere else (the
            // wildcard).
            let inner = match ctx {
                Ctx::Singleton(owner) => Ctx::Named(owner.clone()),
                Ctx::Classy => Ctx::Classy,
                Ctx::ClassyWild => Ctx::Wild,
                _ => Ctx::Wild,
            };
            for &child in body {
                walk(ast, child, &inner, prefix, scan);
            }
        }
        Node::Definition {
            singleton_name: Some(_),
            body,
            ..
        } => {
            // `def self.x`: the module's own method only inside the
            // module's own context — `lexical_self_receiver?` asks
            // `here.is_a?(String)`, which the singleton-class context
            // (`CLASS`) and every non-module context fail. A class body's
            // singleton side is {CLASS}, the top level's {MAIN}: a refine
            // in such a method is the DSL / records nothing a `using`
            // reaches; only the wildcard contexts keep charging.
            let inner = match ctx {
                Ctx::Named(owner) => Ctx::Named(owner.clone()),
                Ctx::Singleton(_) | Ctx::Classy | Ctx::ClassyWild => Ctx::Classy,
                Ctx::Top => Ctx::Top,
                _ => Ctx::Wild,
            };
            for &child in body {
                walk(ast, child, &inner, prefix, scan);
            }
        }
        Node::Definition {
            receiver_def_name: Some(_),
            def_receiver_path,
            body,
            ..
        } => {
            // `def <Const>.x`: a `def M.x` inside `module M` (matched on
            // the last segment, as `lexical_self_receiver?` does) is
            // charged to the module but not the module's own — the
            // targets-wildcard row. Any other receiver, and every
            // non-module context, is the global wildcard.
            let inner = match (ctx, def_receiver_path) {
                (Ctx::Named(owner), Some(path))
                    if !path.contains("::")
                        && owner.rsplit("::").next() == Some(path.as_str()) =>
                {
                    Ctx::WildNamed(owner.clone())
                }
                _ => Ctx::Wild,
            };
            for &child in body {
                walk(ast, child, &inner, prefix, scan);
            }
        }
        Node::Lambda { body, .. } => {
            for &child in body {
                walk(ast, child, &Ctx::Wild, prefix, scan);
            }
        }
        Node::Call {
            receiver,
            method,
            args,
            block_span,
            block_body,
            ..
        } => {
            let self_call = matches!(
                receiver.map(|r| ast.get(r)),
                None | Some(Node::SelfExpr { .. })
            );

            // `refine(T) { … }` — the census row plus the block's
            // activation region. Any `refine`-named call with a block on a
            // self receiver counts (`refine_call?`); a non-constant or
            // missing target leaves the class-wildcard.
            if method == "refine" && self_call && block_span.is_some() {
                let span = block_span.unwrap();
                match ctx {
                    Ctx::Named(owner) => {
                        // `refine_target` reads exactly one argument — any
                        // other arity, or a non-constant, is the
                        // class-wildcard row.
                        let target = if args.len() == 1 {
                            args.first().and_then(|a| match ast.get(*a) {
                                Node::ConstantRead { name, .. } => {
                                    Some(name.trim_start_matches(':').to_string())
                                }
                                _ => None,
                            })
                        } else {
                            None
                        };
                        let (names, names_wildcard) =
                            refine_body_names(ast, block_body, scan);
                        scan.rows.push((
                            owner.clone(),
                            Refinement {
                                target,
                                names,
                                names_wildcard,
                            },
                        ));
                        scan.block_regions.push((span, Some(owner.clone())));
                    }
                    Ctx::WildNamed(owner) => {
                        scan.rows.push((
                            owner.clone(),
                            Refinement {
                                target: None,
                                names: Vec::new(),
                                names_wildcard: true,
                            },
                        ));
                        scan.block_regions.push((span, Some(owner.clone())));
                        // The block's own subtree only yields nested
                        // `refine` regions — its statements record no row.
                        collect_nested_refine_regions(ast, block_body, scan);
                    }
                    Ctx::Wild | Ctx::Top => {
                        // `Ctx::Wild` records the `{*:{*:[*]}}` row; `Top`
                        // records under `""`, which no `using` activates.
                        // Both put a wildcard-named — opaque — activation
                        // in effect over the block.
                        if matches!(ctx, Ctx::Wild) {
                            scan.global_wildcard = true;
                        }
                        scan.block_regions.push((span, None));
                        collect_nested_refine_regions(ast, block_body, scan);
                    }
                    Ctx::Singleton(_) | Ctx::Classy | Ctx::ClassyWild => {
                        // The class's own `refine` DSL: nothing records,
                        // nothing activates. A `refine` nested in the
                        // block still gets its own wildcard region.
                        collect_nested_refine_regions(ast, block_body, scan);
                    }
                }
                // The block's contents are never walked as the parent's
                // context — inside it `self` is the refinement itself.
            } else {
                // Receiver and argument expressions are evaluated in the
                // current context; a block runs under its own context.
                if let Some(recv) = receiver {
                    walk(ast, *recv, ctx, prefix, scan);
                }
                for &arg in args {
                    walk(ast, arg, ctx, prefix, scan);
                }
                if block_span.is_some() {
                    let block_ctx = block_context(ast, *receiver, method, ctx);
                    for &child in block_body {
                        walk(ast, child, &block_ctx, prefix, scan);
                    }
                }
            }
        }
        // Every other node keeps the current context and is descended by
        // its lowered children — `if`/`case`/`begin`/`loop`/`rescue` arms,
        // writes, interpolations, ranges and so on.
        other => {
            let mut kids = Vec::new();
            child_ids(other, &mut kids);
            for child in kids {
                walk(ast, child, ctx, prefix, scan);
            }
        }
    }
}

/// The context a call's block body runs in — `RefineSelf.block`'s
/// `keeps_self?` / `method_block?` / `meta_new_body` / `eval_body`
/// branches, with the port's unreadable cases declining to [`Ctx::Wild`].
fn block_context(
    ast: &LoweredAst,
    receiver: Option<NodeId>,
    method: &str,
    ctx: &Ctx,
) -> Ctx {
    match method {
        "define_method" => {
            return match ctx {
                // `class << self`'s `define_method` charges the module
                // (`runs_on` is its instance side) but the target is
                // unreadable.
                Ctx::Singleton(owner) => Ctx::WildNamed(owner.clone()),
                // A class body's instance side is {CLASS}: the block is a
                // method of the class, and a refine in it records nothing.
                Ctx::Classy => Ctx::Classy,
                // `class << x` / a `Module` subclass: the instance side is
                // the wildcard, the method runs unnameable.
                _ => Ctx::Wild,
            };
        }
        "define_singleton_method" => {
            return match ctx {
                Ctx::Named(owner) => Ctx::WildNamed(owner.clone()),
                // The singleton side of every class context is {CLASS}: the
                // block is a class method, and a refine in it records
                // nothing.
                Ctx::Singleton(_) | Ctx::Classy | Ctx::ClassyWild => Ctx::Classy,
                _ => Ctx::Wild,
            };
        }
        _ => {}
    }
    if EVAL_BLOCK_CALLS.contains(&method) {
        // `M.class_eval { refine … }` charges `M` on the reference where
        // the receiver names a constant; the port reads the wildcard —
        // declining more, never less.
        return Ctx::Wild;
    }
    if let Some(recv) = receiver {
        if let Node::ConstantRead { name, .. } = ast.get(recv) {
            let last = name.rsplit("::").next().unwrap_or(name.as_str());
            match (last, method) {
                ("Module", "new") => return Ctx::Wild,
                ("Class", "new") | ("Struct", "new") | ("Data", "define") | ("Data", "new") => {
                    return Ctx::Classy;
                }
                _ => {}
            }
        }
    }
    // A core iterator's block on a literal receiver keeps `self`; every
    // other block — and every block at the top level — is the wildcard
    // context.
    match ctx {
        Ctx::Top => Ctx::Wild,
        _ if KEEPS_SELF_METHODS.contains(&method)
            && receiver
                .map(|r| literal_node(ast.get(r)))
                .unwrap_or(false) =>
        {
            ctx.clone()
        }
        _ => Ctx::Wild,
    }
}

/// `RefineSelf::LITERALS` — the literal receivers a keeps-self block may
/// iterate.
fn literal_node(n: &Node) -> bool {
    matches!(
        n,
        Node::ArrayLit { .. }
            | Node::HashLit { .. }
            | Node::Range { .. }
            | Node::IntegerLit { .. }
            | Node::FloatLit { .. }
            | Node::StringLit { .. }
            | Node::InterpolatedString { .. }
            | Node::SymbolLit { .. }
    )
}

/// `RefineCensus.read_body` + `classify_call?` — the method names a
/// `refine` block defines: `def`s of every form (the census pushes
/// `node.name` for any `DefNode`), `alias`/`alias_method` new names,
/// `attr_*`, `define_method`/`define_singleton_method` literal names, and
/// `undef`/`remove_method`/`undef_method` literal names. Any other
/// self-call the census does not classify — or a nested `refine` — marks
/// the body incomplete, the reference's names-wildcard. Calls inside
/// `def`s, `define_method` blocks and `class_eval` bodies do not classify
/// (but their nested `refine`s still record regions).
fn refine_body_names(
    ast: &LoweredAst,
    body: &[NodeId],
    scan: &mut RefineScan,
) -> (Vec<String>, bool) {
    let mut names: Vec<String> = Vec::new();
    let mut incomplete = false;
    let mut stack: Vec<NodeId> = body.to_vec();
    while let Some(id) = stack.pop() {
        match ast.get(id) {
            Node::Definition {
                name,
                singleton_name,
                receiver_def_name,
                is_singleton_class: false,
                ..
            } => {
                // `DefNode.name` — the method name for `def x`,
                // `def self.x` and `def M.x` alike.
                let def_name = name
                    .clone()
                    .or_else(|| singleton_name.clone())
                    .or_else(|| receiver_def_name.clone());
                if let Some(n) = def_name {
                    names.push(n);
                }
                // The def's BODY does not classify names — but a `refine`
                // inside it records a region and marks this body
                // incomplete.
                if collect_nested_refine_regions(ast, &[id], scan) {
                    incomplete = true;
                }
            }
            Node::Alias { new_name, .. } => {
                if let Some(name) = literal_name(ast.get(*new_name)) {
                    names.push(name);
                }
            }
            Node::Call {
                receiver,
                method,
                args,
                block_span,
                block_body,
                ..
            } => {
                let self_call = matches!(
                    receiver.map(|r| ast.get(r)),
                    None | Some(Node::SelfExpr { .. })
                );
                if self_call {
                    match method.as_str() {
                        "refine" => {
                            // `:nested` — marks the outer body incomplete;
                            // the block is its own wildcard-owned region.
                            incomplete = true;
                            if let Some(span) = block_span {
                                scan.block_regions.push((*span, None));
                            }
                            collect_nested_refine_regions(ast, block_body, scan);
                            continue;
                        }
                        "alias_method" => {
                            if let Some(name) =
                                args.first().and_then(|a| literal_name(ast.get(*a)))
                            {
                                names.push(name);
                            }
                        }
                        "attr" | "attr_reader" => {
                            for arg in args {
                                if let Some(name) = literal_name(ast.get(*arg)) {
                                    names.push(name.clone());
                                    if method == "attr" {
                                        names.push(format!("{name}="));
                                    }
                                }
                            }
                        }
                        "attr_writer" => {
                            for arg in args {
                                if let Some(name) = literal_name(ast.get(*arg)) {
                                    names.push(format!("{name}="));
                                }
                            }
                        }
                        "attr_accessor" => {
                            for arg in args {
                                if let Some(name) = literal_name(ast.get(*arg)) {
                                    names.push(name.clone());
                                    names.push(format!("{name}="));
                                }
                            }
                        }
                        "define_method" | "define_singleton_method" => {
                            if let Some(name) =
                                args.first().and_then(|a| literal_name(ast.get(*a)))
                            {
                                names.push(name);
                            }
                        }
                        "undef" | "undef_method" | "remove_method" => {
                            for arg in args {
                                if let Some(name) = literal_name(ast.get(*arg)) {
                                    names.push(name);
                                }
                            }
                        }
                        "private" | "public" | "protected" | "module_function"
                        | "private_class_method" | "public_class_method" | "target"
                        | "using" | "namespace" => {}
                        "main" | "refinement" => {
                            incomplete = true;
                        }
                        _ => {
                            incomplete = true;
                        }
                    }
                }
                // Block bodies of `define_method` / eval calls do not
                // classify; every other block's calls do.
                if let Some(recv) = receiver {
                    stack.push(*recv);
                }
                stack.extend_from_slice(args);
                match method.as_str() {
                    "define_method" | "define_singleton_method" | "class_eval"
                    | "module_eval" | "class_exec" | "module_exec" => {
                        if collect_nested_refine_regions(ast, block_body, scan) {
                            incomplete = true;
                        }
                    }
                    _ => {
                        stack.extend_from_slice(block_body);
                    }
                }
            }
            other => {
                let mut kids = Vec::new();
                child_ids(other, &mut kids);
                stack.extend_from_slice(&kids);
            }
        }
    }
    (names, incomplete)
}

/// The literal name a `SymbolLit`/`StringLit` arg spells, like the
/// reference's `literal_method_name`.
fn literal_name(n: &Node) -> Option<String> {
    match n {
        Node::SymbolLit { value, .. } | Node::StringLit { value, .. } => {
            Some(value.clone())
        }
        _ => None,
    }
}

/// Walk `body` for `refine`-shaped calls anywhere under it — each one's
/// block is a wildcard-owned region (`:nested` — no row of its own).
/// Returns true when at least one was found.
fn collect_nested_refine_regions(
    ast: &LoweredAst,
    body: &[NodeId],
    scan: &mut RefineScan,
) -> bool {
    let mut found = false;
    let mut stack: Vec<NodeId> = body.to_vec();
    while let Some(id) = stack.pop() {
        if let Node::Call {
            method,
            receiver,
            block_span,
            ..
        } = ast.get(id)
        {
            let self_call = matches!(
                receiver.map(|r| ast.get(r)),
                None | Some(Node::SelfExpr { .. })
            );
            if method == "refine" && self_call && block_span.is_some() {
                found = true;
                scan.block_regions.push((block_span.unwrap(), None));
            }
        }
        let mut kids = Vec::new();
        child_ids(ast.get(id), &mut kids);
        stack.extend_from_slice(&kids);
    }
    found
}

fn qualify_ctx(prefix: &[String], name: &str) -> String {
    let mut parts: Vec<String> = prefix.to_vec();
    parts.extend(
        name.split("::")
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    );
    parts.join("::")
}

/// Every lowered child of `n` — mirrored from
/// `flow_writes::node_child_ids`. A variant missed here under-collects
/// `refine` sites: the wildcard row then never records and the port FIRES
/// where the reference declines — so this list is deliberately exhaustive
/// over [`Node`]'s `NodeId` fields.
fn child_ids(n: &Node, out: &mut Vec<NodeId>) {
    match n {
        Node::Program { body, .. }
        | Node::Statements { body, .. }
        | Node::Definition { body, .. }
        | Node::ClassDef { body, .. }
        | Node::ModuleDef { body, .. }
        | Node::Lambda { body, .. } => out.extend_from_slice(body),
        Node::LocalVariableWrite { value, .. }
        | Node::LocalVariableOpWrite { value, .. }
        | Node::VariableWrite { value, .. }
        | Node::InstanceVariableWrite { value, .. }
        | Node::ConstantWrite { value, .. } => out.push(*value),
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
        Node::AttrWrite {
            receiver, value, ..
        } => {
            out.extend(receiver.iter().copied());
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
            out.extend(receiver.iter().copied());
            out.extend_from_slice(args);
            out.extend_from_slice(block_body);
        }
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
            out.extend(predicate.iter().copied());
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
            out.extend(predicate.iter().copied());
            out.extend_from_slice(body);
        }
        // `body` is the flat superset: the protected statements
        // (`main_body`), every rescue clause's exceptions and statements,
        // the `else` and the `ensure` all ride it. Descending the views as
        // well visited each section twice — ×2 per nesting level
        // (rigor-rs#963).
        Node::BeginRescue { body, .. } => out.extend_from_slice(body),
        Node::Logical { left, right, .. } => {
            out.push(*left);
            out.push(*right);
        }
        Node::ArrayLit { elements, .. } | Node::HashLit { elements, .. } => {
            out.extend_from_slice(elements);
        }
        Node::Range { left, right, .. } => {
            out.extend(left.iter().copied());
            out.extend(right.iter().copied());
        }
        Node::Return { values, .. } => out.extend_from_slice(values),
        Node::Alias {
            new_name, old_name, ..
        } => {
            out.push(*new_name);
            out.push(*old_name);
        }
        _ => {}
    }
}
