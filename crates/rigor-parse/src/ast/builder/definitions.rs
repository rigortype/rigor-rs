//! `Builder::lower_definitions` holds the definition arms of
//! `Builder::lower_node`: `def`, `class`, `module` and `class << self`.

use crate::ruby_prism::Node as PrismNode;

use super::{
    all_param_names, body_has_explicit_return, constant_node_name, constant_path_string,
    constant_string, direct_method_names, discover_visibilities_and_includes, param_shape_of,
    plain_positional_params, rooted_constant_path, self_anchored_constant_path, span_of, Builder,
    Node, NodeId, ParamShape, UNMATCHED,
};

impl<'src> Builder<'src> {
    /// [`Self::lower_node`]'s definition arms: `def`, `class`, `module` and
    /// `class << self`.
    // Not a perf hint: keeps this frame out of `lower_node`'s recursion.
    // Inlined, the release nesting limit on a 2 MiB thread roughly halves (#413).
    #[inline(never)]
    pub(super) fn lower_definitions(&mut self, node: &PrismNode<'_>) -> NodeId {
        if let Some(def) = node.as_def_node() {
            // Lower the method body so its calls reach the walk. Parameters are
            // intentionally NOT bound to any type: an unknown local read is
            // already `Dynamic[top]` (silent), the zero-FP-safe choice — binding
            // a param to a guessed type could mint a false `undefined-method`.
            let body = self.lower_optional_body(def.body().as_ref());
            // Retain the method name (None for a receiver-bearing `def self.x` /
            // `def obj.x` — a singleton method, never an instance method, so it
            // must not be harvested as a tier-4b instance-method body) and whether
            // any explicit `return` appears in the body (the tier-4b decline gate).
            let name = def
                .receiver()
                .is_none()
                .then(|| constant_string(def.name().as_slice()));
            let has_explicit_return = def
                .body()
                .as_ref()
                .map(body_has_explicit_return)
                .unwrap_or(false);
            // The plain-positional param names (for tier-4b call-site binding),
            // or `None` to decline when the signature has anything that breaks
            // positional index<->arg alignment (splat/post/kwargs/block/optional).
            let params = plain_positional_params(def.parameters().as_ref());
            // The full RBS-relevant param structure (for sig-gen's initialize stub).
            let param_shape = param_shape_of(def.parameters().as_ref());
            // The method-NAME token span (for the override-visibility rule's
            // diagnostic anchor); `None` for a receiver-bearing singleton def
            // (kept parallel to `name`, which is also `None` there).
            let name_span = def
                .receiver()
                .is_none()
                .then(|| span_of(&def.name_loc()));
            // A receiver-bearing def (`def recv.x`) evaluates `recv` in the
            // ENCLOSING scope. Lower it so its reads are visible — otherwise a
            // `def local.m` looks like `local` is assigned-but-never-read
            // (flow.dead-assignment FP, real-corpus audit: textbringer). The node
            // lives in the arena; the span-scan analyses find it (orphan-proof).
            if let Some(recv) = def.receiver() {
                let _ = self.lower_node(&recv);
            }
            // C2: lower each parameter DEFAULT-VALUE expression as an arena node
            // so the call rules reach it (the reference checks positional and
            // keyword defaults, incl. nested calls within — `def f(t =
            // Time.current)`, `def g(a: [1,2].frob)`). Orphaned like the
            // receiver above: it lives in the arena so the span-scan / `ast.iter`
            // call walk finds it, but is not a body statement (so a default's
            // write is not mis-attributed to the method body). Params themselves
            // stay unbound (Dynamic ⇒ silent), so only a literal/constant
            // receiver in a default is ever witnessed — the FP-safe subset.
            if let Some(params) = def.parameters() {
                for opt in params.optionals().iter() {
                    if let Some(o) = opt.as_optional_parameter_node() {
                        let _ = self.lower_node(&o.value());
                    }
                }
                for kw in params.keywords().iter() {
                    if let Some(o) = kw.as_optional_keyword_parameter_node() {
                        let _ = self.lower_node(&o.value());
                    }
                }
            }
            // A `def self.x` (SELF receiver) captures its method name here so
            // `sig-gen` can collect the singleton; `name` stays `None` so the
            // instance-method harvest still skips it. A non-self receiver
            // (`def obj.x`) is left `None`.
            let singleton_name = def
                .receiver()
                .filter(|r| r.as_self_node().is_some())
                .map(|_| constant_string(def.name().as_slice()));
            // The mirror for a NON-self receiver (`def IO.console_size`). Its
            // rendered constant path rides along so the def-attribution walk
            // can apply `def_receiver_targets_lexical_self?` (`def Object.x`
            // inside `Object.class_eval` is a singleton def, not `Object#x`).
            let def_receiver_path = def
                .receiver()
                .filter(|r| r.as_self_node().is_none())
                .map(|r| constant_path_string(&r))
                .filter(|p| !p.is_empty());
            let receiver_def_name = def
                .receiver()
                .filter(|r| r.as_self_node().is_none())
                .map(|_| constant_string(def.name().as_slice()));
            return self.push(Node::Definition {
                name,
                is_singleton_class: false,
                singleton_name,
                receiver_def_name,
                def_receiver_path,
                param_span: def.parameters().as_ref().map(|p| span_of(&p.location())),
                has_explicit_return,
                params,
                param_shape,
                param_names: all_param_names(def.parameters().as_ref()),
                name_span,
                singleton_operand: None,
                body,
                span: span_of(&def.location()),
            });
        }

        if let Some(class) = node.as_class_node() {
            // The constant-path name (`Point`, `Foo::Bar`). The superclass name,
            // if a `< Bar` clause is written (a bare const or a const path; its
            // last component is what the source-chain walk keys on). The instance
            // methods are the `def` names defined directly in the body — read
            // from Prism BEFORE lowering, since lowering erases a def's name.
            let name = constant_path_string(&class.constant_path());
            let superclass = class
                .superclass()
                .and_then(|s| constant_node_name(&s));
            // The FULL written superclass path (for the override-visibility walk).
            let superclass_path = class.superclass().map(|s| constant_path_string(&s)).filter(|s| !s.is_empty());
            let methods = class
                .body()
                .as_ref()
                .map(direct_method_names)
                .unwrap_or_default();
            let body = self.lower_optional_body(class.body().as_ref());
            // Harvest per-method bodies for tier-4b RETURN inference. The DIRECT
            // children of the lowered class body (lower_optional_body flattens the
            // Statements wrapper) are exactly the body's top-level statements, so
            // a direct, named `Definition` among them is a direct instance method
            // — the same inclusion rule as `direct_method_names` (`def self.x`
            // lowers to a name-less Definition and is skipped; a def nested in a
            // conditional/inner class is not a direct child and is skipped).
            let method_bodies = self.harvest_method_bodies(&body);
            // ADR-35 slice 1: the source-discovered instance-method visibility
            // table + include/prepend names, read from the Prism body BEFORE
            // lowering (lowering erases the modifier-call/`def`-name structure
            // the discovery needs). Mirrors `scope_indexer.rb` exactly.
            let (method_visibilities, includes) = class
                .body()
                .as_ref()
                .map(discover_visibilities_and_includes)
                .unwrap_or_default();
            return self.push(Node::ClassDef {
                name,
                rooted: rooted_constant_path(&class.constant_path()),
                self_anchored: self_anchored_constant_path(&class.constant_path()),
                superclass,
                superclass_path,
                methods,
                method_bodies,
                method_visibilities,
                includes,
                body,
                span: span_of(&class.location()),
            });
        }

        if let Some(module) = node.as_module_node() {
            let name = constant_path_string(&module.constant_path());
            let methods = module
                .body()
                .as_ref()
                .map(direct_method_names)
                .unwrap_or_default();
            let body = self.lower_optional_body(module.body().as_ref());
            let method_bodies = self.harvest_method_bodies(&body);
            let (method_visibilities, includes) = module
                .body()
                .as_ref()
                .map(discover_visibilities_and_includes)
                .unwrap_or_default();
            return self.push(Node::ModuleDef {
                name,
                rooted: rooted_constant_path(&module.constant_path()),
                self_anchored: self_anchored_constant_path(&module.constant_path()),
                methods,
                method_bodies,
                method_visibilities,
                includes,
                body,
                span: span_of(&module.location()),
            });
        }

        if let Some(sclass) = node.as_singleton_class_node() {
            let operand = self.lower_node(&sclass.expression());
            let body = self.lower_optional_body(sclass.body().as_ref());
            return self.push(Node::Definition {
                name: None, // `class << self` has no single method name.
                is_singleton_class: true, // a CLASS scope, not a method def.
                singleton_name: None, // the BODY's inner defs are the singletons.
                receiver_def_name: None,
                def_receiver_path: None,
                param_span: None,
                has_explicit_return: false,
                params: None,    // no single method ⇒ no param binding.
                param_shape: ParamShape::default(),
                param_names: Vec::new(),
                name_span: None, // no single name ⇒ no name span.
                singleton_operand: Some(operand),
                body,
                span: span_of(&sclass.location()),
            });
        }

        UNMATCHED
    }
}
