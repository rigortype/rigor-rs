//! Call dispatch: typing a block-less method call ([`Typer::type_call`]) —
//! `X.new` instances, literal folding, and the RBS return lookup (singleton
//! and object-constant receivers included), with its untyped-argument and
//! bare-nominal-join declines and in-source parameter bounds.

use rigor_index::{OverloadSignature, RetainedParamType};
use rigor_parse::{LoweredAst, Node, NodeId};
use rigor_types::{Interner, RefinementId, Scalar, Type, TypeId};

use crate::{folding, ParamBoundReturn, TypeEnv, Typer};

impl<'i> Typer<'i> {
    /// Type a method call with a receiver, running the conservative head of the
    /// dispatch cascade (ADR-0023):
    ///
    /// 1. **Constant folding** (ADR-0008 Rust core): if the receiver types to a
    ///    value-pinned `Constant(scalar)` and [`folding::fold`] yields a result,
    ///    return that pinned `Constant`.
    /// 2. **RBS-ish return resolution**: else resolve the receiver's class via
    ///    the index and look up [`rigor_index::method_return`]; intern the
    ///    result as a `Nominal { class }` so the *next* call in a chain can be
    ///    typed (and a typo on it flagged).
    /// 3. **Fallback**: otherwise `Dynamic[top]` — silence over a guess.
    ///
    // TODO(spec): tier-2 shape dispatch, tier-4 in-source bodies, argument
    // contracts, the Ruby sidecar for non-Rust-foldable calls (ADR-0008/0023).
    // too_many_arguments: the call context (receiver, args and their
    // plain-positional flag, env, interner) is one logical bundle.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn type_call(
        &self,
        ast: &LoweredAst,
        receiver: NodeId,
        method: &str,
        args: &[NodeId],
        args_plain_positional: bool,
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> TypeId {
        let ty = self.type_call_declines(
            ast,
            receiver,
            method,
            args,
            args_plain_positional,
            env,
            interner,
            true,
        );
        self.decline_fold_gap(ast, receiver, env, ty, interner)
    }

    /// sig-gen's def-local typing (rigor-rs#546): a call on a def-local
    /// constant-carrier (`a = 1.5; a.to_s`, `s = "ab"; s.strip`, `t = [1, 2];
    /// t.join`) that the port does not fold types to the method's RBS nominal
    /// (`String`, `Integer`, `Array[..]`), where the reference folds it to a
    /// constant (`"1.5"`, `"ab"`, `"12"`). The fold tables differ, so the
    /// answer is not the reference's — decline (`untyped`; the method is
    /// skipped). A folded result (a `Constant`) is kept. A literal receiver
    /// (`1.5.to_s`) is the pre-existing fold gap master shares and is left alone.
    fn decline_fold_gap(
        &self,
        ast: &LoweredAst,
        receiver: NodeId,
        env: &TypeEnv,
        ty: TypeId,
        interner: &mut Interner,
    ) -> TypeId {
        if self.def_scope.get().is_none()
            || !match ast.get(receiver) {
                Node::LocalVariableRead { .. } => true,
                Node::VariableRead { name, .. } => crate::def_local_marks::is_ivar(name),
                _ => false,
            }
            || !self.is_scalar_nominal_result(interner, ty)
        {
            return ty;
        }
        let recv = self.type_of(ast, receiver, env, interner);
        if Self::is_constant_carrier(interner, recv) {
            interner.untyped()
        } else {
            ty
        }
    }

    fn is_constant_carrier(interner: &Interner, ty: TypeId) -> bool {
        match interner.get(ty) {
            Type::Constant(_) | Type::Tuple(_) | Type::HashShape(_) | Type::IntegerRange { .. } => true,
            Type::Union(ms) => ms.iter().all(|&m| Self::is_constant_carrier(interner, m)),
            _ => false,
        }
    }

    fn is_scalar_nominal_result(&self, interner: &Interner, ty: TypeId) -> bool {
        match interner.get(ty) {
            Type::Nominal { class, .. } => matches!(
                self.index.class_name_for_id(*class),
                Some(
                    "String" | "Integer" | "Float" | "Symbol" | "Rational" | "Complex" | "Array" | "Hash"
                        | "TrueClass" | "FalseClass"
                )
            ),
            Type::Union(ms) => ms.iter().all(|&m| {
                matches!(interner.get(m), Type::Constant(Scalar::Bool(_)))
                    || self.is_scalar_nominal_result(interner, m)
            }),
            _ => false,
        }
    }

    /// `type_call` for the internal `recv[k]` / `recv.attr` current-read a
    /// compound write performs (rigor-rs#178). A user-facing `x[9]`
    /// declines the nilable lookup, but declining here would erase the
    /// stored-slot record to `Dynamic` (`s = "abc"; s[0] ||= nil; x =
    /// s[0]; x.upcase` lost its `possible-nil` row); the read keeps the
    /// flat nominal so the record's join stands as on master.
    pub(crate) fn type_call_for_write_value(
        &self,
        ast: &LoweredAst,
        receiver: NodeId,
        method: &str,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> TypeId {
        // The synthesized `recv[k]`/`recv.attr` read carries only the index /
        // plain exprs — plain positional by construction.
        self.type_call_declines(ast, receiver, method, args, true, env, interner, false)
    }

    #[allow(clippy::too_many_arguments)]
    fn type_call_declines(
        &self,
        ast: &LoweredAst,
        receiver: NodeId,
        method: &str,
        args: &[NodeId],
        args_plain_positional: bool,
        env: &TypeEnv,
        interner: &mut Interner,
        user_facing: bool,
    ) -> TypeId {
        // Tier 4 (in-source / RBS `.new`): `X.new` where `X` is a constant
        // naming a class known to the RBS index OR the SourceIndex types to a
        // Nominal INSTANCE of `X`, so a chained `X.new.method` can be checked.
        // We resolve the receiver constant's NAME directly (the bare constant
        // read itself stays Dynamic — we never type a class object). A core
        // (RBS) class wins its core ClassId; a source-only class gets a
        // source-range ClassId from the SourceIndex.
        if method == "new" {
            if let Some(ty) = self.type_dot_new(ast, receiver, args, env, interner) {
                return ty;
            }
            // Not a typeable `.new` (metaclass constructor / unknown constant /
            // a reference constant-constructor lift) ⇒ fall through to the
            // folding / RBS-return cascade below.
        }

        // Tier 4c (ADR-0038): interprocedural literal-tail fold on a `Const.method`
        // SINGLETON call. When `receiver` is a project class/module constant whose
        // OWN singleton `method` provably returns one scalar literal (and is not
        // overridable), the call types that pinned `Constant` — feeding
        // `flow.always-truthy-condition` (`Gitlab::Database.read_only? -> false`).
        // A dedicated, minimal-blast-radius tier: it consults the definers index
        // directly and does NOT type the bare constant as `Singleton`, so no other
        // rule's view of a project constant changes. Any miss falls through
        // (Dynamic, silent) — a project constant still types Dynamic as before.
        if let Node::ConstantRead { name, dynamic_base: false, .. } = ast.get(receiver) {
            if !name.is_empty() {
                // rigor-rs#450: a project-sig declaration answers ahead of
                // the body fold, resolved lexically — `T.one` inside `module
                // A` reads `A::T`'s signature, not a toplevel `T`'s fold.
                if let Some(ty) =
                    self.project_sig_singleton_return(ast, receiver, name, method, interner)
                {
                    return ty;
                }
                if let Some(scalar) = self.source.const_singleton_literal(name, method) {
                    return interner.intern(Type::Constant(scalar));
                }
            }
        }

        // C3a Part A: `self.class.name` / `self.class.to_s` inside a lexical
        // class/module returns the class name as a `String` (the reference
        // unwraps the `Module#name : String?` optional to `String` for
        // witnessing). This lights the `self.class.name.demodulize` /
        // `.underscore` idiom.
        //
        // We match the SPECIFIC `(self.class).name` shape and type ONLY the tail,
        // WITHOUT ever typing `self.class` itself to a witnessable `Singleton`.
        // Typing `self.class` to a project `Singleton` would route
        // `self.class.<class_method>` (calling one of the class's OWN class
        // methods — a ubiquitous idiom) through the class-method witnessing path,
        // which sees only the core RBS surface and cannot verify a project-defined
        // class method ⇒ a flood of false positives (`valid_provider?`,
        // `with_redis`, …). The reference resolves those against the project class
        // and stays silent, so `self.class` itself must remain untyped (Dynamic)
        // here — only the always-String `name`/`to_s` tail is resolved. Toplevel
        // (`enclosing_prefix` empty) declines → silent, matching the reference.
        if (method == "name" || method == "to_s") && args.is_empty() {
            if let Node::Call { receiver: Some(inner), method: inner_m, args: inner_args, .. } =
                ast.get(receiver)
            {
                if inner_m == "class"
                    && inner_args.is_empty()
                    && matches!(ast.get(*inner), Node::SelfExpr { .. })
                {
                    if let Node::SelfExpr { span } = ast.get(*inner) {
                        if !self.enclosing_prefix(*span).is_empty() {
                            return self.nominal_or_untyped("String", interner);
                        }
                    }
                }
            }
        }

        let recv_ty = self.type_of(ast, receiver, env, interner);

        // Upstream v0.4.3 (#1836, rigor #1752): a class-object call whose
        // name a root refinement in scope defines types `Dynamic` — so a
        // call chained after `Integer.name` stays silent (rigor-rs#467). The
        // rules driver's `refinement_reaches` decline withholds the
        // diagnostic on the refined call itself but left the `String` typing
        // for `.zz_chain` to witness on.
        if let Type::Singleton(class) = interner.get(recv_ty) {
            let recv_name = self
                .index
                .class_name_for_id(*class)
                .or_else(|| self.source.class_name_for_id(*class));
            if let Some(recv_name) = recv_name {
                let site = ast.get(receiver).span();
                if self.refined_class_object_method(ast, site, recv_name, method) {
                    return interner.untyped();
                }
            }
        }

        if self.literal_concat_declines(ast, receiver, recv_ty, method, args, env, interner) {
            return interner.untyped();
        }

        // Issue #352 — port of the reference's `try_non_nil_receiver_retry`
        // (expression_typer.rb, upstream #519): a `T | nil` union receiver
        // whose dispatch exhausts every tier retries the whole cascade on the
        // non-nil fragment — `s[0]` post-mutation types `String | nil`, so
        // `s[0].strip` still answers `String` and a chained `.frobnicate`
        // witnesses on it. The flat env has no memberwise union dispatch, so
        // this retry IS the union's only dispatch here — which is exactly why
        // the reference's `nil_class_defines?` guard must ride along: when
        // NilClass defines `method` (`to_s`, `inspect`, `nil?`, `itself`,
        // `==`, …) the nil arm is a live answerer (`x.to_s` joins a carrier
        // `.frobnicate` must not witness on), not the veto that triggers the
        // retry. Diagnostics read the receiver's own type and are untouched —
        // this substitution only feeds the call's RESULT type.
        let recv_ty = match interner.get(recv_ty) {
            Type::Union(_) => match self.non_nil_fragment(recv_ty, interner) {
                Some(non_nil) if !self.index.class_has_method("NilClass", method) => non_nil,
                _ => recv_ty,
            },
            _ => recv_ty,
        };

        // Indexed stored-slot narrowing (rigor-rs#325): a `h[k]` read with a
        // stable `(local, literal key)` address answers the record an
        // `operand`-flagged `h[k] ||= v` left (`eval_index_or_write` →
        // `Scope#with_indexed_narrowing`), ahead of the `[]` dispatch — the
        // reference's `indexed_narrowing_for` "sits ahead of
        // MethodDispatcher.dispatch so the standard `Hash#[]` answer does
        // not override the narrowing" (expression_typer.rb:1394).
        if method == "[]" && args.len() == 1 {
            if let Node::LocalVariableRead { name, .. } = ast.get(receiver) {
                if let Some(key) = crate::stable_index_key(ast.get(args[0])) {
                    if let Some(&recorded) =
                        env.get(crate::indexed_narrowing_key(name, &key).as_str())
                    {
                        return recorded;
                    }
                }
            }

            // rigor-rs#770 — the `recv[k]` read on a verbatim-carrier UNION.
            // The reference's `dispatch_union` erases each union member to
            // `Hash[K,V]` / `Array[E]` (`hash_shape_type_args` /
            // `tuple_type_args`) and unions the `[]` returns, so `{a:1} | [1]`
            // `h[0]` answers the element envelope `1`, and `{a:1} | {b:2}`
            // `h[0]` answers `1 | 2` — never a memberwise slot lookup (the
            // single-member path owns those) and never `Dynamic`. This is
            // also the `current` half `slot_stored_type` reads for the
            // `h[k] ||= v` record, which `verbatim_slot?` already admits for
            // every-member-verbatim unions. A union holding any
            // non-verbatim member declines whole — as `dispatch_union`
            // declines when any member does — falling through to the same
            // tiers that decline today.
            if let Some(ty) = self.union_index_read(recv_ty, interner) {
                return ty;
            }
        }

        // C3a Part B: `Module#name` / `Class#name` / `#to_s` on a CLASS OBJECT
        // (`Singleton` receiver) returns the class name as a `String`. This is a
        // real (core-RBS) `Singleton` — from the `ConstantRead` arm's zero-FP gate
        // (`Time.name`, `Foo.name` where `Foo` is a known top-level class) — so it
        // is NOT the project-class hazard Part A avoids: a core `Singleton` already
        // witnesses class-method typos against a KNOWN surface. `name`/`to_s` are
        // always valid on a class object and always yield `String`, so this is
        // zero-FP; the returned `String` is NON-nilable, so the possible-nil
        // channel (which resolves the receiver via `class_name_of`, `None` for a
        // `Singleton`) never mints a nilable fact from it.
        if (method == "name" || method == "to_s")
            && matches!(interner.get(recv_ty), Type::Singleton(_))
        {
            return self.nominal_or_untyped("String", interner);
        }

        // Kernel intrinsic explicit-receiver spelling: `Kernel.p(x)` /
        // `Kernel.format(...)` / `Kernel.String(x)` etc. `module_function` exposes
        // each Kernel intrinsic as a public singleton on the Kernel module object,
        // so the explicit `Kernel.` receiver dispatches to the SAME fold as the
        // implicit-self spelling (reference `kernel_owned_call?` +
        // `kernel_module_receiver?`, upstream c9d2e473 — pinned after the rigor-rs
        // port's harness found `Kernel.p` declining while `Kernel.format` folded).
        // Gated on the receiver TYPE resolving to `Singleton[Kernel]` (not the node
        // spelling), so a namespaced user `Kernel` constant — which types Dynamic,
        // never `Singleton[Kernel]` — cannot slip through. The shared fold carries
        // the same user-redefinition / splat decline guards; a non-fold Kernel
        // method (`Kernel.puts`) returns `None` and falls through unchanged.
        let kernel_module_receiver = matches!(
            interner.get(recv_ty),
            Type::Singleton(class) if self.source.class_name_for_id(*class) == Some("Kernel")
        );
        if kernel_module_receiver {
            if let Some(ty) =
                self.type_implicit_self_call(ast, method, args, None, env, interner)
            {
                return ty;
            }
        }

        // Singleton-method RBS return typing (M2-GO slice 4): a CLASS-method
        // call on a core `Singleton` receiver types its RBS return when that
        // return is unanimous across every overload (`Date.today -> Date`,
        // `Time.at -> Time`), so a chained AS-method typo witnesses
        // (`Date.today.end_of_month` — probed: the reference fires, rigor-rs
        // was silent). Divergent-overload returns (`Regexp.last_match`:
        // `MatchData?` vs `String?`) are `None` by the index's
        // all-overloads-agree collapse — decline, fall through (the receiver
        // stays `Singleton`, so class-method typo witnessing is unchanged).
        // `.new` never reaches here (intercepted by `type_dot_new` above).
        if let Type::Singleton(class) = interner.get(recv_ty) {
            let class = *class;
            if let Some(class_name) = self.source.class_name_for_id(class) {
                // Issue #168: a PROJECT-sig class object's `def self.m` return —
                // `Foo::Impl.make` behind `use Foo::*`. The member names a
                // project signature stores (`::`-anchored / `use`-mapped)
                // resolve only through the qualified path, so this rides
                // `receiver_singleton_*` — which falls back to the identical
                // short-key answer for bundled names — gated the same way the
                // instance arm is: project-sig provenance requires the
                // reference's `build_singleton` to succeed
                // (`project_sig_chain_ok`); a SYNTHESIZED stub receiver is
                // `Dynamic[top]` upstream, never a mintable carrier.
                if self.index.is_synthesized_stub(class_name) {
                    return interner.untyped();
                }
                if self.index.is_qualified_project_sig_class(class_name)
                    && self.index.project_sig_chain_ok(class_name)
                {
                    if let Some(ty) =
                        self.project_sig_singleton_answer(class_name, method, interner)
                    {
                        return ty;
                    }
                }
                // A TUPLE return (`Process.wait2 : [Integer, Process::Status]`)
                // types to a `Type::Tuple` of its element classes — the shape the
                // flat `singleton_method_return` slot collapses to `None`. Same
                // all-overloads-agree discipline (the index declines a divergent
                // set), so this only ever REPLACES a `Dynamic[top]` result.
                if let Some(shapes) = self.index.singleton_method_tuple_return(class_name, method) {
                    return self.intern_rbs_tuple(shapes, interner);
                }
                // Collection-shape stage 2a: `Dir.glob(…)` / `Dir[…]` declare a
                // BLOCK overload returning `nil`, which breaks the flat slot's
                // all-overloads-agree collapse even though THIS call site — the
                // block-free path (a block routes to `type_block_call`) —
                // unambiguously yields `Array[String]`. The block-free slot is
                // populated only for methods with BOTH overload kinds and only
                // when every block-free overload agrees on one concrete class,
                // so it can only ever REPLACE a `Dynamic[top]` result.
                if let Some(ret) = self
                    .index
                    .singleton_method_return(class_name, method)
                    .or_else(|| self.index.singleton_method_return_block_free(class_name, method))
                {
                    // Mint the return instance with the type_dot_new id
                    // resolution: a core (CORE_CLASSES) nominal id when
                    // available, else the source-registry id in the high range
                    // (`Time`/`Date` are not in the 9-class core id space; the
                    // rules recover their name via `class_name_for_id_of`).
                    if let Some(class_id) = self.index.class_id(ret) {
                        return interner.intern(Type::Nominal { class: class_id, args: vec![] });
                    }
                    if let Some(class_id) = self.source.class_id(ret) {
                        return interner.intern(Type::Nominal { class: class_id, args: vec![] });
                    }
                }
            }
        }

        // Collection-shape stage 2b: an RBS TOP-LEVEL **object constant**
        // receiver — `ENV`, declared `ENV: RBS::Unnamed::ENVClass` in core RBS.
        // The reference resolves the constant's declared type and types
        // `ENV.keys` as `Array[String]`; rigor-rs left `ENV` `Dynamic[top]`, so
        // the whole `(ENV.keys.select { … } - base).present?` chain went
        // unwitnessed.
        //
        // Only the CALL's RETURN is typed here — the constant itself is never
        // minted as a `Nominal`, so no new undefined-method witnessing surface
        // appears for `ENV.<anything>` itself (a strict under-emit vs the
        // reference). Gated on the SAME lexical shadow predicate the
        // `ConstantRead` arm's C1 gate uses: a project `ENV` constant/class, or
        // a C5-harvested literal constant of that name, declines.
        if let Node::ConstantRead { name, span, dynamic_base: false, .. } = ast.get(receiver) {
            if let Some(decl_class) = self.index.object_constant_class(name) {
                let prefix = self.enclosing_prefix(*span);
                if !self.source.constant_shadowed(name, prefix)
                    && !self.source.project_writes_constant(name)
                    && !self.source.literal_constant_visible_any_file(name, prefix)
                {
                    // NILABLE RETURNS DECLINE. `ENVClass#[]` is `(String) ->
                    // String?`; the reference carries the `String | nil` union
                    // and dispatch on it declines, so typing the chain as a bare
                    // `String` fired `ENV['X'].present?` where the oracle is
                    // silent (measured: 13 of the sweep's 15 FPs on the first
                    // cut of this arm). The flat `method_return` slot drops the
                    // nil bit, so this arm reads `method_return_nilable`
                    // instead. The block-free slot records only bare concrete
                    // returns, so it is non-nilable by construction.
                    if let Some(ret) = self
                        .index
                        .method_return_nilable(decl_class, method)
                        .and_then(|(c, nilable)| (!nilable).then_some(c))
                        .or_else(|| self.index.method_return_block_free(decl_class, method))
                    {
                        if let Some(class_id) =
                            self.index.class_id(ret).or_else(|| self.source.class_id(ret))
                        {
                            return interner
                                .intern(Type::Nominal { class: class_id, args: vec![] });
                        }
                    }
                }
            }
        }

        // Tier 1: constant folding on a value-pinned receiver. Fold only when
        // EVERY argument also types to a value-pinned `Constant` (ADR-0008
        // zero-FP: a non-pinned arg means we can't prove the result, so we
        // decline and widen to the nominal return / Dynamic below — never
        // guess). The nullary case (`args` empty) folds the no-arg core.
        if let Type::Constant(scalar) = interner.get(recv_ty).clone() {
            if let Some(arg_scalars) = self.pin_arg_scalars(ast, args, env, interner) {
                // ConstantFolding `try_fold_self_return`
                // (constant_folding.rb:318): a no-arg `freeze` / `itself` /
                // `dup` / `clone` on a value-pinned receiver returns THE PIN
                // — `"a".dup.zork` witnesses `for "a"` (rigor-rs#489).
                if arg_scalars.is_empty()
                    && matches!(method, "freeze" | "itself" | "dup" | "clone")
                {
                    return interner.intern(Type::Constant(scalar));
                }
                // Upstream v0.4.6 (#2125 / #2247): a String fold past the byte
                // limit declines whole; the reference's next tier answers a
                // `literal-string` carrier the port does not model, so the
                // call types untyped (silent) rather than as a giant constant
                // or a bare `String` whose message would drift.
                if folding::string_fold_oversized(&scalar, method, &arg_scalars) {
                    return interner.untyped();
                }
                // A String lookup can answer `nil`, so a stale value flips the
                // result's CLASS. The top-level flat env keeps a local's first
                // literal across `<<`, `+=`, branch and block writes, so a lookup
                // that reads a local declines to the RBS answer (#149 review:
                // `buf = ""; buf << "x"; buf[0].upcase` fired `for nil`).
                // `Float#<=>` joins them: its `nil` arm has the same problem.
                // rigor-rs#499: the risk is a STALE pin, not a local read —
                // `s = "abc"; s[9]` folds to `nil` on the oracle because its
                // env tracks the literal through the binding. A local whose
                // only mark is the seeding write still proves its pin
                // (`stale_local` counts a second rebind or any content
                // mutation); a composite expression reading any local keeps
                // the old any-read decline — the port cannot see which of
                // its names the value pinned on.
                let stale_risk = folding::is_nilable_fold(&scalar, method)
                    && std::iter::once(receiver)
                        .chain(args.iter().copied())
                        .any(|id| match ast.get(id) {
                            Node::LocalVariableRead { name, .. } => {
                                self.stale_local(ast, name)
                            }
                            _ => ast.reads_local_within(ast.get(id).span()),
                        });
                if !stale_risk {
                    match folding::fold_outcome(&scalar, method, &arg_scalars) {
                        folding::FoldOutcome::Value(folded) => {
                            if folding::oversized_string_result(&folded) {
                                return interner.untyped();
                            }
                            return interner.intern(Type::Constant(folded));
                        }
                        // Upstream #2115's `WidenedResult`: a fold past
                        // `INTEGER_FOLD_BIT_LIMIT` still knows its class —
                        // the reference answers `Nominal[Integer]` (or
                        // `Rational` for a negative exponent), where the
                        // port's plain decline landed on `Dynamic` and
                        // `x.zz` stayed silent (rigor-rs#694).
                        folding::FoldOutcome::Widened(class) => {
                            return self.nominal_or_untyped(class, interner);
                        }
                        folding::FoldOutcome::Declined => {}
                    }
                }
                // rigor-rs#499 — a STALE pin cannot fold (the env literal is
                // not the runtime value) but the oracle still answers the RBS
                // signature: `s << "x"; s[9]` types `String?`, `s.index` types
                // `Integer?`. A bare untyped loses the nil (`x = s[0]; x.upcase`
                // keeps its `possible-nil` row on the oracle), and a bare
                // nominal would invent the flat `String` the pin's decline was
                // introduced to erase — so the fallback is the SAME
                // `method_return_nilable` answer tier 3 gives a Nominal
                // receiver: `T | nil` for a nilable return, the flat nominal
                // otherwise, untyped only when the class does not define the
                // method at all.
                if stale_risk {
                    return match self
                        .index
                        .method_return_nilable(folding::scalar_class(&scalar), method)
                    {
                        Some((ret, nilable)) => {
                            let nominal = self.nominal_or_untyped(ret, interner);
                            if nilable {
                                rigor_types::Algebra::join(interner, nominal, interner.nil())
                            } else {
                                nominal
                            }
                        }
                        None => interner.untyped(),
                    };
                }
                // Issue #164: for these `Integer?` lookups an unfolded literal
                // call must not reach tier 3's bare `Integer` (see the predicate).
                // Issue #178 widened the set to the whole `is_str_lookup` family,
                // but only for the user-facing read: a compound write's internal
                // `current` read keeps the flat nominal (its slot record joins
                // `String | nil` / `Integer | nil` from it).
                if folding::declines_unfolded(&scalar, method)
                    && (user_facing || !folding::declines_str_index_family(&scalar, method))
                {
                    return interner.untyped();
                }
                // ADR-0008 sidecar fallback: the Rust core declined, but if this
                // is a `sidecar_foldable` pure call and a real-Ruby folder is
                // wired (full-fidelity mode), execute it there. A declined /
                // absent folder leaves the value widened (sound subset).
                if let Some(folder) = self.folder {
                    if folding::sidecar_foldable(folding::scalar_class(&scalar), method)
                        && !folding::sidecar_blows_up(method, &arg_scalars)
                    {
                        if let Some(folded) = folder.fold(&scalar, method, &arg_scalars) {
                            if folding::oversized_string_result(&folded) {
                                return interner.untyped();
                            }
                            return interner.intern(Type::Constant(folded));
                        }
                    }
                }
            } else if user_facing && folding::is_str_lookup(&scalar, method) {
                // Issue #178: the fold never runs when an argument does not
                // pin (a Range or Regexp literal, a local bound to one), but
                // a nilable String lookup can still answer `nil` — the
                // reference folds the real call and is silent on the miss,
                // where tier 3's bare `String` / `Integer` fires on the
                // chained call. Decline as `declines_unfolded` does for the
                // pinned-but-unfolded path.
                return interner.untyped();
            }
        }

        // Tier 2: value-pinned shape projection on a `Tuple` receiver (reference
        // ShapeDispatch). A no-arg accessor / constant-index read on a
        // value-pinned Tuple folds to the pinned element or arity — `[1, 2].first`
        // → `1`, `[1, 2].size` → `2`, `[1, 2][0]` → `1` — sharpening `type-of` /
        // `annotate` and chained witnessing (`[1, 2].first.frist` flags on `1`).
        // Only reached for BLOCK-FREE calls (the Call arm routes block calls to
        // `type_block_call`), so a block form never mis-folds here.
        if let Some(folded) = self.fold_tuple_projection(recv_ty, method, ast, args, env, interner) {
            return folded;
        }

        // Tier 2b: value-pinned shape projection on a `HashShape` receiver
        // (reference ShapeDispatch's HashShape catalogue). A static-key lookup /
        // slice / inversion folds to the precise member type — `{ a: 1 }[:a]` →
        // `1`, `{ a: 1 }.has_key?(:a)` → `true`. Declines (→ None) on any
        // uncertainty, so the RBS `Hash` dispatch below still answers (and a
        // typo'd method still witnesses via `class_name_of(HashShape) == Hash`).
        // Block-free only (block calls never reach `type_call`), so no over-fold.
        if let Some(folded) =
            self.fold_hash_shape_projection(recv_ty, method, ast, args, env, interner)
        {
            return folded;
        }

        // Issue #146: a call whose argument reaches more than one distinct
        // precise value (`v = 1; v = 2 if c`) is folded MEMBER-WISE on the
        // reference when the receiver is a value-pinned `Constant` —
        // `"abc"[v]` answers the `"b" | "c"` union and `1.fdiv(v)` the
        // `1.0 | 0.5` union, both carriers `call.undefined-method` never
        // witnesses on — never the flat `method_return` class tier 3 would
        // mint (the row's FP). Withhold the nominal only on that exact
        // shape: a single reaching literal still folds or pins (`"abc"[1]`
        // fires `for "b"`, and `v = 1` keeps its `String`), the untyped /
        // guarded declines keep their own gates, and non-Constant receivers
        // never entered the member fold to begin with.
        //
        // Issue #332: the multi-valuedness is the ARGUMENT's, not its type's.
        // The old conjunct (`arg_ty == untyped || Union`) only asked about the
        // carrier rigor-rs types the argument with, and missed the nominal
        // ones: `"abc"[1 + v]` types `Integer` while `v` is `1 | 2`, and
        // `"abc"[v..]` types `Nominal[Range]` — the reference still member-
        // folds `1 + v` to `2 | 3` (then `"abc"[2 | 3]` to `"c" | nil`) or
        // leaves `String | nil` for a range it cannot pin. `arg_reach` now
        // composes the operands of an arg expression, so `.multi` carries the
        // union through the nominal shell; nominal-typed single values keep
        // their own decline below (`rbs_dispatch_declines_on_untyped_arg`'s
        // `pins_one_constant`).
        if matches!(interner.get(recv_ty), Type::Constant(_)) {
            let multi_arg = args.iter().any(|&a| self.arg_reach(ast, a).multi);
            if multi_arg {
                return interner.untyped();
            }
        }

        // Tier 3 (-ish): resolve receiver class -> method return class.
        if let Some(class_name) = self.index.class_name_of(interner, recv_ty) {
            // Reference `MethodDispatcher::ShapeDispatch` (`shape_dispatch.rb`)
            // refinement projections: `Integer#to_s` / `#inspect` (and
            // `Float#to_s`) answer the `decimal-int-string` /
            // `non-empty-string` refinement carriers ahead of the flat RBS
            // `String` (rigor-rs#696), and those carriers keep their
            // refinement through the transformations ShapeDispatch knows.
            if let Some(projected) =
                self.refinement_projection(ast, recv_ty, method, args, env, interner)
            {
                return projected;
            }
            // The instance twin of the singleton tuple arm above
            // (`"a-b".partition("-") : [String, String, String]`): a tuple return
            // types per-position instead of collapsing to `Dynamic[top]`.
            if let Some(shapes) = self.index.method_tuple_return(class_name, method) {
                return self.intern_rbs_tuple(shapes, interner);
            }
            // Collection-shape stage 2c: the instance twin of the singleton
            // block-free arm above. `String#split: (…) -> Array[String] | (…)
            // { … } -> self` loses its return to the flat slot's
            // all-overloads-agree collapse; a BLOCK-FREE `x.split(':', 2)` (the
            // only kind that reaches `type_call`) is unambiguously an `Array`,
            // which is what the reference's `block_required: false` overload
            // selection resolves too.
            if let Some(ret_class) = self
                .index
                .method_return(class_name, method)
                .or_else(|| self.index.method_return_block_free(class_name, method))
            {
                // #521 in the GENERIC dispatch (issue #118). The flat return
                // slot answers a BARE `Nominal[C]`; the reference answers what
                // the surviving overloads actually join to. When those differ —
                // a nilable `C?`, or two candidates the erased head hid — a
                // REFERENCE-UNTYPED argument means the reference cannot fold the
                // call to a value either, so its carrier is the union and no
                // negative rule fires on it. Decline to `Dynamic[top]` there.
                let untyped_arg = self.rbs_dispatch_declines_on_untyped_arg(
                    class_name, method, ast, args, env, interner,
                );
                if untyped_arg {
                    return interner.untyped();
                }
                if let Some(class_id) = self.index.class_id(ret_class) {
                    let nominal = interner.intern(Type::Nominal {
                        class: class_id,
                        args: vec![],
                    });
                    // Issue #352 — the nil bit the flat slot erases. A `T?`
                    // RBS return on a Nominal receiver is `T | nil` in the
                    // reference (`String#[]` joins `String?` across all four
                    // overloads): a chained `.frobnicate` declines on the nil
                    // arm instead of witnessing on bare `String`, while the
                    // non-nil fragment keeps feeding `non_nil_fragment`'s
                    // retry above (`s[0].strip -> String`) and the
                    // `x = s[0]` local's possible-nil channel. A `Constant`
                    // receiver is folded/pinned upstream, so it keeps the
                    // bare nominal — `s = "abc"; s[0].frobnicate` still fires
                    // — and `class_name_of` only reaches here for carriers
                    // whose reads the reference also runs RBS dispatch on.
                    if matches!(interner.get(recv_ty), Type::Nominal { .. })
                        && matches!(
                            self.index.method_return_nilable(class_name, method),
                            Some((ret, true)) if ret == ret_class
                        )
                    {
                        return rigor_types::Algebra::join(interner, nominal, interner.nil());
                    }
                    return nominal;
                }
            }
            // rigor-rs#775 — when the flat `method_return` slot collapses on a
            // divergent overload set (`Integer#/` is `(Integer) -> Integer |
            // (Float) -> Float | …`, so the slot holds no answer), the
            // reference's `OverloadSelector` still answers the call from the
            // argument types — pass 0 (`ProvenOverload`) for a call whose
            // every argument is a plain value, else the first-overload
            // fallback. `x = 2 ** 20000 / 3` is the headline row: the `**`
            // widened the receiver to `Nominal[Integer]`, the fold had
            // nothing to pin, and the slot had nothing to give.
            // Only a plain-positional call reaches the selector: a splat, a
            // bare keyword-hash (`f.round(2, half: :even)` — rigor-rs#775
            // review) or `...` is counted in `args` but carries no plain
            // argument's class for its slot, and the reference's
            // `KeywordArguments` / `keywords_last` engagement is unmodeled —
            // the call declines rather than mis-select.
            if args_plain_positional {
                if let Some(overloads) = self.index.method_overloads(class_name, method) {
                    if let Some(ty) = self.proven_overload_return(
                        overloads, recv_ty, ast, args, env, interner,
                    ) {
                        return ty;
                    }
                }
            }
        }

        // Tier 4b (ADR-0023): in-source method RETURN inference. A SOURCE-class
        // receiver (a project `X.new` instance) whose called method has a
        // precomputed concrete CORE return interns that CORE nominal, so the
        // chained call witnesses against the real RBS (e.g. `user.full_name :
        // String`, then `.lenght` flags against String). The source receiver is
        // recovered via `class_name_for_id_of` (the core `class_name_of` above
        // returns `None` for a source-range id, so this never overlaps tier 3).
        // Any miss — no source receiver, no inferred return, or an unregistered
        // core name — falls through to Dynamic (silent; zero-FP).
        if let Some(src_name) = self.source.class_name_for_id_of(interner, recv_ty) {
            let src_name = src_name.to_string();
            // Issue #168: a SYNTHESIZED missing-type stub receiver is
            // `Dynamic[top]` in the reference (`try_synthesized_stub_type`) —
            // never a carrier a source method table may answer for either
            // (the stub exists exactly because nothing declared the name).
            if self.index.is_synthesized_stub(&src_name) {
                return interner.untyped();
            }
            // Issue #168 (tier-4b RBS half): a receiver typed by a PROJECT-sig
            // class gets its method returns from the loaded RBS — `x.make`
            // where `use Foo::*` mapped the receiver's signature. The member
            // names a project signature stores (`::`-anchored / `use`-mapped /
            // root-only) resolve only through the qualified path, which
            // `receiver_method_*` prefers for exactly these names; a bundled
            // qualified-only name (`Process::Status`, reached through a tuple
            // element mint) takes the same ADR-0042 Slice-5 routing it already
            // had via `method_return_nilable`. Two gates keep it
            // zero-false-positive:
            //   * `project_sig_chain_ok` — the reference's `build_instance`
            //     collapses the whole definition to `Dynamic[top]` when the
            //     chain is incomplete / a module sits where a superclass
            //     belongs / a module self-type never declared (project-sig
            //     names only; bundled names keep prefix semantics);
            //   * nilable — a `C?` return is `C | nil` upstream, a carrier no
            //     negative rule fires on (the `ENV['X']` arm's discipline).
            let qualified_receiver = self.index.is_qualified_project_sig_class(&src_name)
                || (self.index.knows_qualified_class(&src_name)
                    && !self.index.knows_class(&src_name));
            if qualified_receiver
                && (!self.index.is_qualified_project_sig_class(&src_name)
                    || self.index.project_sig_chain_ok(&src_name))
            {
                if let Some(shapes) =
                    self.index.receiver_method_tuple_return(&src_name, method)
                {
                    return self.intern_rbs_tuple(&shapes, interner);
                }
                if let Some((ret, nilable)) =
                    self.index.receiver_method_return(&src_name, method)
                {
                    if nilable {
                        return interner.untyped();
                    }
                    if let Some(class_id) = self
                        .index
                        .class_id(ret)
                        .or_else(|| self.source.class_id(ret))
                    {
                        return interner
                            .intern(Type::Nominal { class: class_id, args: vec![] });
                    }
                }
            }
            // Upstream v0.4.1 (#1594): a call reached through an ancestry the
            // file cannot order — a conditional / block / hook / receiver-form
            // mixin, a fork, an external ahead of the definer, a possible def
            // — types `Dynamic`, even for the class's own `def`.
            if !self.source.own_def_typing_settled(self.index, &src_name, method) {
                return interner.untyped();
            }
            // ADR-17: a `pre_eval:` patch of the name may answer ahead of the
            // class's own `def` (the reference's patched tier sits above the
            // discovered-method tier and answers `Dynamic`). Coarse by name:
            // declining only costs precision.
            if self.source.pre_eval_defines_name(method) {
                return interner.untyped();
            }
            if let Some(ret_core) = self.source.method_return(&src_name, method) {
                if let Some(class_id) = self.index.class_id(ret_core) {
                    return interner.intern(Type::Nominal { class: class_id, args: vec![] });
                }
            }
            // Tier 4b call-site PARAMETER BINDING (ADR-0023): a source method
            // whose return DEFERS to a positional argument. We bind the ARG's
            // type to the rooted param, then re-derive the core return — the
            // param-independent path above never fired for it (its tail is param-
            // rooted, hence Dynamic under the empty build-time env). The whole
            // safety argument is a STRICT under-approximation: we resolve only
            // when the bound arg AND every chain step land on a concrete CORE
            // class via the same `method_return` table tier 3 uses; any miss
            // (arg out of range, non-core arg, a chain step with no core return)
            // ⇒ Dynamic (silent). No AST/node-id is needed — the descriptor
            // carries the param index + the no-arg core chain, so this is fully
            // cross-file safe. No re-entry into `infer_method_returns` (the
            // chain walks the core return table only, never an in-source body),
            // so there is no recursion into the build pass.
            if let Some(pb) = self.source.param_bound_return(&src_name, method) {
                if let Some(core_class) =
                    self.resolve_param_bound(ast, pb, args, env, interner)
                {
                    if let Some(class_id) = self.index.class_id(&core_class) {
                        return interner.intern(Type::Nominal { class: class_id, args: vec![] });
                    }
                }
            }
        }

        // Tier 5: unknown -> Dynamic[top].
        interner.untyped()
    }

    /// Whether the tier-3 flat-slot answer for `class_name#method` must be
    /// WITHHELD at this call site — the generic-dispatch half of upstream #521
    /// (`3d5dddbb`, PR #537), issue #118.
    ///
    /// ## What the reference does, and where the flat slot diverges
    ///
    /// The reference selects the overloads that match the call's arity and block
    /// shape and joins their translated returns (`join_candidate_returns`): one
    /// candidate answers its own return, several with the SAME return answer it,
    /// several with distinct returns answer `Dynamic[union]` — a carrier no
    /// negative rule fires on. rigor-rs has no RBS type translator, so tier 3
    /// reads a per-method flat slot instead, which is right only when the
    /// reference's join is a single BARE nominal. Two measured ways it is not
    /// (both oracle-measured at pin `ffb456b0`):
    ///
    /// 1. **A nilable return.** `String#[]`'s four overloads all return
    ///    `String?`; the reference answers `String | nil` and stays silent,
    ///    while `method_return` drops the nil bit and hands tier 3 a bare
    ///    `String` that `call.undefined-method` witnesses on — issue #118's
    ///    `"abc"[u].frobnicate` (rows a15, and the `slice` / `byteslice` /
    ///    `index` / `rindex` / `getbyte` / `byteindex` / `assoc` / `rassoc` /
    ///    `Float#<=>` twins).
    /// 2. **Returns that agree only after ERASURE.** `method_signature` compares
    ///    the head class NAME, so `Array[[E, X]]` and `Array[Array[E | U]]`
    ///    "agree" on `Array`; the reference joins them to `Dynamic[union]` and
    ///    stays silent. This is #521's own `[true] * n` class of defect — here
    ///    `Array#product`, `Array#zip` and `String#scan`, whose candidate sets
    ///    this predicate reproduces exactly.
    ///
    /// ## The gate
    ///
    /// Withhold when some argument is one the reference cannot value-fold to
    /// a single reportable value ([`Typer::arg_reach`], the same analysis the
    /// Kernel folds use):
    ///
    /// - a REFERENCE-IMPRECISE argument — untyped, or since #1021 (`5496acd6`)
    ///   a union with an untyped member — skips the strict and alias passes,
    ///   so the gradual join keeps `Dynamic[union]` standing (`s = 1 if
    ///   s.nil?; "abc"[s]` and `"abc".index(s)` are reference-silent, rows
    ///   r01/r11);
    /// - on a NILABLE join, an argument that is not one pinned `Constant`
    ///   (`pins_one_constant`): a multi-valued local member-folds to a union
    ///   (#146), and a precise-but-never-`Constant` one — a nominal like
    ///   `i = rand.to_i` or `1 + v`, a range with a non-static endpoint
    ///   (`"abc"[v..]`), a guarded parameter (#121), an interpolated /
    ///   Tuple / Hash literal — leaves the `C?` overload's nil arm in place,
    ///   so the reference answers `C | nil` and is silent (rigor-rs#332:
    ///   the old gate asked `arg_ty == untyped`, which saw none of these).
    ///
    /// With a LITERAL argument the reference folds `"abc"[0]` to `"a"` and
    /// fires, and rigor-rs's bare `String` matches that row — so the foldable
    /// shapes keep their diagnostic (`"abc"[0]`, `"abc"[1..]`, `v = "x"` then
    /// `"abc"[v]`, `v = 0` — `0` is `Constant[0]` upstream despite its `rand`
    /// opacity — all measured).
    ///
    /// A disagreeing NON-nilable join keeps the flat slot for an unpinned
    /// precise argument on purpose: the argument's own nominal can narrow
    /// the overload set back to one agreeing return — `[1, 2].product(u)`
    /// with `u` guarded `Integer` fires on the reference — so only the
    /// imprecise arm declines there.
    ///
    /// The index test runs FIRST and the arena walk only if it says the answer
    /// is at risk: `arg_reach` is two arena scans per root visited (see its own
    /// COST note), and tier 3 is the hot dispatch path. An argument the
    /// reference would pin but this analysis cannot see as such — a chain over
    /// a pinned local (`v = s.length`), a `Constant`-valued range endpoint —
    /// declines into a coverage loss, never a false positive.
    fn rbs_dispatch_declines_on_untyped_arg(
        &self,
        class_name: &str,
        method: &str,
        ast: &LoweredAst,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> bool {
        if args.is_empty() || self.rbs_join_is_one_bare_nominal(class_name, method, args.len()) {
            return false;
        }
        let nilable = matches!(self.index.method_return_nilable(class_name, method), Some((_, true)));
        args.iter().any(|&a| {
            let arg_ty = self.type_of(ast, a, env, interner);
            // A port-pinned arg — `K = 5; "abc"[K]`, an inline literal — is
            // what the fold itself sees; leave it alone.
            if matches!(interner.get(arg_ty), Type::Constant(_)) {
                return false;
            }
            let reach = self.arg_reach(ast, a);
            // An imprecise argument (bare `Dynamic[top]` or a union carrying
            // it) skips the strict/alias passes on ANY non-bare join — the
            // gradual answer is the union and no negative rule fires on it.
            if reach.untyped {
                return true;
            }
            // A precise argument that is not one pinned `Constant` keeps the
            // nil arm of a `C?` overload — `C | nil` is silent — but can
            // still narrow a disagreeing NON-nilable join back to one
            // agreeing return (`[1, 2].product(u)` with `u` guarded
            // `Integer` fires on the reference), so only the nilable join
            // declines.
            nilable && !self.arg_pins_one_constant(ast, a)
        })
    }

    /// Whether `arg` resolves to exactly one `Type::Constant` on the
    /// reference ([`Reach::pins_one_constant`]) — looking through an
    /// `is_a?`/`kind_of?`/`instance_of?` guard the way issue #121's
    /// `arg_is_guarded_parameter` did: a class guard only NARROWS the value,
    /// so `u = 1; return unless u.is_a?(Integer)` still leaves `u` a pinned
    /// `Constant[1]` that `"abc"[u]` folds through (it fires `for "b"`).
    fn arg_pins_one_constant(&self, ast: &LoweredAst, arg: NodeId) -> bool {
        if self.arg_reach(ast, arg).pins_one_constant() {
            return true;
        }
        match ast.get(arg) {
            Node::LocalVariableRead { name, span, .. } => self
                .local_reach(ast, name, *span, &mut vec![format!("{name}@{}", span.0)], true)
                .0
                .pins_one_constant(),
            _ => false,
        }
    }

    /// rigor-rs#775 — the reference `OverloadSelector` pass 0
    /// (`proven_overload.rb`, `overload_selector.rb:find_proven_overload`)
    /// plus its `no_match_fallback`, consulted only where the flat
    /// `method_return` slot found nothing. With every argument a plain value
    /// (a `Constant`, or a `Nominal` with no type arguments —
    /// `ProvenOverload.applies?`), the FIRST overload in declared order that
    /// gradually matches answers the call, but only when it is PROVEN: every
    /// positional parameter it engages is a lone class naming its argument's
    /// own class (`names_arg_class?`), and every overload declared before it
    /// is proven out (`contested?` — each of its parameters disjoint from
    /// every argument's class under both class orderings). When no overload
    /// matches at all the reference fails soft onto the first overload that
    /// does not require a block; the same fallback applies here.
    ///
    /// The strict / alias / gradual passes and the receiver-affinity reorder
    /// the reference falls to when pass 0 declines are NOT ported: a gradual
    /// match that fails the proven tests, and every imprecise-argument call
    /// (whose `#521` union-of-matches this slice does not model), declines
    /// back to the cascade's `Dynamic[top]` — silence, never a guessed class.
    /// Only a BARE class-name `return_form` translates to a `Nominal` (the
    /// reference's translator answers unions, tuples, `self` and literal
    /// returns too); anything else declines.
    fn proven_overload_return(
        &self,
        overloads: &[OverloadSignature],
        recv_ty: TypeId,
        ast: &LoweredAst,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<TypeId> {
        // `applies?`: at least one argument and every argument a `Constant`
        // or a `Nominal` with no type arguments.
        if args.is_empty() {
            return None;
        }
        let mut arg_classes: Vec<String> = Vec::with_capacity(args.len());
        let mut all_constant_args = true;
        for &a in args {
            let arg_ty = self.type_of(ast, a, env, interner);
            let name = match interner.get(arg_ty) {
                Type::Constant(s) => folding::scalar_class(s).to_string(),
                Type::Nominal { class, args } if args.is_empty() => {
                    all_constant_args = false;
                    self.index
                        .class_name_for_id(*class)
                        .map(str::to_string)
                        .or_else(|| self.source.class_name_for_id(*class).map(str::to_string))?
                }
                _ => return None,
            };
            arg_classes.push(name);
        }
        // The receiver must be a carrier the reference's RbsDispatch actually
        // reaches. A `Constant` ran ConstantFolding first, so it is admitted
        // only when an argument the fold cannot pin proves the fold declined
        // upstream too: an all-`Constant` call may fold there to a literal
        // (`x = 1; x / 3.0` prints `0.333…`, not `Float`) or a widened
        // nominal this answer would drift — the port's fold is the narrower
        // one. A `BigInt` receiver is that same fold gap (rigor-rs#695),
        // gate or no gate.
        match interner.get(recv_ty) {
            Type::Nominal { .. } => {}
            Type::Constant(s) if !matches!(s, Scalar::BigInt(_)) && !all_constant_args => {}
            _ => return None,
        }
        let argc = arg_classes.len();
        let mut uncontested = true;
        // The `no_match_fallback` answers only where the reference reached the
        // end of EVERY selector pass. An arity-eligible overload that rejected
        // an argument is a safe fallback base only when its `no` came through
        // a lone `ClassInstance` param — a class disjointness the gradual pass
        // proves the same way. An `Alias` / `Interface` / `Variable` / `Other`
        // param rejects here but can still match in the reference's alias or
        // gradual pass, so the fallback would answer an overload its selector
        // never reached (`f.round("x")` — rigor-rs#775 review).
        let mut fallback_ok = true;
        for ov in overloads {
            // A block-free call site never engages a block-REQUIRED overload,
            // and a call carrying no keyword arguments (the caller gates on
            // `args_plain_positional`) never satisfies a required one —
            // `engages_block_shape?` and `KeywordArguments.accepted_by?`.
            let engages = !ov.block_required && !ov.has_required_keywords;
            // Rest and trailing positionals are presence-only in the retained
            // shapes. A trailing param is unreadable whenever the overload
            // could engage; a rest param is unreadable only when the call
            // actually spills into it (`argc > required + optional` — below
            // that it contributes no engaged slot, so the overload is still
            // decidable). An overload that could engage either is neither a
            // safe pick NOR a safe fallback: it might be the match.
            if engages
                && (ov.has_trailing_positionals
                    || (ov.has_rest_positionals
                        && argc > ov.required_positionals.len() + ov.optional_positionals.len()))
                && argc >= ov.required_positionals.len()
            {
                return None;
            }
            let arity_ok = argc >= ov.required_positionals.len()
                && argc <= ov.required_positionals.len() + ov.optional_positionals.len();
            if engages && arity_ok {
                let needed = argc - ov.required_positionals.len();
                let engaged: Vec<&RetainedParamType> = ov
                    .required_positionals
                    .iter()
                    .chain(ov.optional_positionals.iter().take(needed))
                    .collect();
                if engaged
                    .iter()
                    .zip(&arg_classes)
                    .all(|(p, a)| self.index.param_accepts_arg_class(p, a))
                {
                    // The FIRST gradual match — pass 0's pick, contingent on
                    // the proof: uncontested, and every engaged param a lone
                    // class naming its argument's own class.
                    let named = engaged
                        .iter()
                        .zip(&arg_classes)
                        .all(|(p, a)| Self::names_arg_class(p, a));
                    if !uncontested || !named {
                        return None;
                    }
                    return self.overload_bare_class_return(ov, interner);
                }
                if engaged
                    .iter()
                    .any(|p| !matches!(p, RetainedParamType::ClassInstance(..)))
                {
                    fallback_ok = false;
                }
            }
            if !self.overload_proven_out(ov, &arg_classes) {
                uncontested = false;
            }
        }
        if !fallback_ok {
            return None;
        }
        // `no_match_fallback`: nothing matched, so the first overload that
        // does not require a block answers (else `method_types.first`) —
        // read in RECEIVER-AFFINITY order, as the reference reads it: an
        // overlay that prepends a disjoint-sibling arm (`bigdecimal`'s
        // `Integer#/(BigDecimal) -> BigDecimal`) cannot win by position, so
        // `x / "a"` answers `Integer`, not `BigDecimal`.
        let ordered =
            self.receiver_affinity_order(overloads, self.index.class_name_of(interner, recv_ty)?);
        ordered
            .iter()
            .find(|ov| !ov.block_required)
            .or_else(|| ordered.first())
            .and_then(|ov| self.overload_bare_class_return(ov, interner))
    }

    /// The reference `ReceiverAffinity.reorder` (`receiver_affinity.rb`): a
    /// stable partition pulling forward every overload whose positional
    /// parameters all name the receiver's class or a proper RBS ancestor of
    /// it (`param_class_in_ancestry?` — a ClassInstance whose name equals the
    /// receiver's class, or sits above it in `param_arg_ordering`; Optional
    /// and Union walk their members, every other shape disqualifies). A
    /// nullary overload and one with unreadable trailing params carry no
    /// affinity; the rest param is unexamined, as upstream.
    fn receiver_affinity_order<'o>(
        &self,
        overloads: &'o [OverloadSignature],
        self_class: &str,
    ) -> Vec<&'o OverloadSignature> {
        let mut affinity: Vec<&OverloadSignature> = Vec::new();
        let mut other: Vec<&OverloadSignature> = Vec::new();
        for ov in overloads {
            if self.overload_has_receiver_affinity(ov, self_class) {
                affinity.push(ov);
            } else {
                other.push(ov);
            }
        }
        affinity.extend(other);
        affinity
    }

    fn overload_has_receiver_affinity(&self, ov: &OverloadSignature, self_class: &str) -> bool {
        // `required.empty? && optional.empty? && trailing.empty?` disqualifies
        // — a nullary (or rest-only) overload has no param classes to compare.
        // Trailing params are unreadable in the retained shape, so a `(*T, U)`
        // overload lands in the non-affinity bucket either way upstream could.
        if ov.has_trailing_positionals
            || (ov.required_positionals.is_empty() && ov.optional_positionals.is_empty())
        {
            return false;
        }
        ov.required_positionals
            .iter()
            .chain(&ov.optional_positionals)
            .all(|p| self.param_class_in_ancestry(p, self_class))
    }

    fn param_class_in_ancestry(&self, param: &RetainedParamType, self_class: &str) -> bool {
        match param {
            RetainedParamType::ClassInstance(name, _) => {
                let name = name.strip_prefix("::").unwrap_or(name);
                name == self_class
                    || matches!(
                        self.index.param_arg_ordering(self_class, name),
                        rigor_index::ClassOrdering::Subclass
                    )
            }
            RetainedParamType::Optional(inner) => {
                self.param_class_in_ancestry(inner, self_class)
            }
            RetainedParamType::Union(members) => members
                .iter()
                .all(|m| self.param_class_in_ancestry(m, self_class)),
            _ => false,
        }
    }

    /// `names_arg_class?` — the param is a lone class-instance spelling whose
    /// name IS the argument's class (`(Integer)` for an `Integer` arg). The
    /// retained name is already lexically resolved (`(Queue)` inside
    /// `class Thread` stored as `Thread::Queue`), as the reference's
    /// translated `param.class_name` is; a `::`-rooted spelling reads bare.
    fn names_arg_class(param: &RetainedParamType, arg_class: &str) -> bool {
        match param {
            RetainedParamType::ClassInstance(name, _) => {
                name.strip_prefix("::").unwrap_or(name) == arg_class
            }
            _ => false,
        }
    }

    /// `ProvenOverload.proven_out?` — an overload declared before the pick is
    /// proven out when EVERY positional parameter is a plain class disjoint
    /// from every argument's class under both orderings. Union and optional
    /// params recurse; a rest / trailing param is unreadable here and an
    /// alias / interface / variable / literal proves nothing — either way the
    /// overload stays a live contestant and the pick is contested.
    fn overload_proven_out(&self, ov: &OverloadSignature, arg_classes: &[String]) -> bool {
        if ov.has_rest_positionals || ov.has_trailing_positionals {
            return false;
        }
        ov.required_positionals
            .iter()
            .chain(&ov.optional_positionals)
            .all(|p| self.param_proven_disjoint(p, arg_classes))
    }

    fn param_proven_disjoint(&self, param: &RetainedParamType, arg_classes: &[String]) -> bool {
        match param {
            RetainedParamType::ClassInstance(name, _) => arg_classes
                .iter()
                .all(|a| self.index.classes_proven_disjoint(a, name)),
            RetainedParamType::Optional(inner) => {
                self.param_proven_disjoint(inner, arg_classes)
            }
            RetainedParamType::Union(members) => members
                .iter()
                .all(|m| self.param_proven_disjoint(m, arg_classes)),
            _ => false,
        }
    }

    /// The call's answer when the selected overload's `return_form` is a
    /// single class name (`Integer`, `A::B`): its `Nominal`. Every other
    /// written shape — a union, a tuple, `self`/`instance`, `bool`, a literal,
    /// a generic application — is more than this slice translates; `None`
    /// declines back to the cascade's `Dynamic[top]`.
    fn overload_bare_class_return(
        &self,
        ov: &OverloadSignature,
        interner: &mut Interner,
    ) -> Option<TypeId> {
        let return_form = ov.return_form.as_str();
        let name = return_form.strip_prefix("::").unwrap_or(return_form);
        // A method type parameter's name is a variable, not a class —
        // `Object#to_yaml`'s `[IO] (IO, …) -> IO` would otherwise resolve
        // `IO` to the class through `class_id` (rigor-rs#775 review).
        // `type_param_bounds` records only BOUNDED params, so an unbounded
        // `[T]` shows up as a `Variable` leaf in the param shapes instead;
        // check both.
        if ov.type_param_bounds.iter().any(|(n, _)| *n == name)
            || ov.required_positionals
                .iter()
                .chain(&ov.optional_positionals)
                .any(|p| Self::param_mentions_variable(p, name))
        {
            return None;
        }
        let bare = name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':');
        if !bare {
            return None;
        }
        let class_id = self
            .index
            .class_id(name)
            .or_else(|| self.source.class_id(name))?;
        Some(interner.intern(Type::Nominal {
            class: class_id,
            args: vec![],
        }))
    }

    /// Does `name` occur as a bare type `Variable` anywhere inside a retained
    /// param shape (a `[T]`/bounded `[I < _ToInt]` method parameter's own
    /// spellings — used to spot a `return_form` that is really a variable)?
    fn param_mentions_variable(p: &RetainedParamType, name: &str) -> bool {
        match p {
            RetainedParamType::Variable(v) => *v == name,
            RetainedParamType::ClassInstance(_, args)
            | RetainedParamType::Alias(_, args)
            | RetainedParamType::Interface(_, args)
            | RetainedParamType::Union(args)
            | RetainedParamType::Tuple(args) => {
                args.iter().any(|a| Self::param_mentions_variable(a, name))
            }
            RetainedParamType::Optional(inner) => Self::param_mentions_variable(inner, name),
            RetainedParamType::Other(_) => false,
        }
    }

    /// The index half of [`Typer::rbs_dispatch_declines_on_untyped_arg`]:
    /// whether the reference's join over the overloads that match `argc` and a
    /// BLOCK-FREE call site is a single bare nominal — the only shape tier 3's
    /// flat slot can spell faithfully.
    ///
    /// `true` (keep the flat answer) when the return is non-nilable AND every
    /// candidate declares the same verbatim return. `true` also when the method
    /// retains no overload shapes (nothing to contradict the slot) or when no
    /// candidate matches the arity — the reference then falls back to a SINGLE
    /// overload (`overloads.find { !requires_block } || overloads.first`) and
    /// pins it, exactly as the flat slot does.
    ///
    /// Candidate selection is deliberately PERMISSIVE where the retained shapes
    /// are coarse (a trailing positional does not raise the minimum arity, and
    /// no per-argument type filtering is applied): admitting an extra candidate
    /// can only turn agreement into disagreement, i.e. make the port answer
    /// LESS, which is FP-safe.
    fn rbs_join_is_one_bare_nominal(&self, class_name: &str, method: &str, argc: usize) -> bool {
        // (1) A nilable return — the reference's carrier is `C | nil`.
        if matches!(self.index.method_return_nilable(class_name, method), Some((_, true))) {
            return false;
        }
        // (2) The candidates the reference would join.
        let Some(overloads) = self.index.method_overloads(class_name, method) else {
            return true;
        };
        let mut agreed: Option<&str> = None;
        for ov in overloads {
            // A block-less call site never engages a block-REQUIRING overload,
            // and the reference skips an overload with a required keyword
            // (`rejects_keyword_required?`) because no keyword is passed here.
            if ov.block_required || ov.has_required_keywords {
                continue;
            }
            if argc < ov.required_positionals.len() {
                continue;
            }
            if !ov.has_rest_positionals
                && !ov.has_trailing_positionals
                && argc > ov.required_positionals.len() + ov.optional_positionals.len()
            {
                continue;
            }
            match agreed {
                None => agreed = Some(ov.return_form.as_str()),
                Some(prev) if prev == ov.return_form => {}
                Some(_) => return false,
            }
        }
        true
    }

    /// Resolve a tier-4b call-site PARAMETER-BINDING descriptor against the
    /// actual call arguments, returning the concrete CORE class NAME the method
    /// returns for THIS call, or `None` to decline (Dynamic, silent).
    ///
    /// 1. The arg at `pb.param_index` must exist (arg count > index) — fewer args
    ///    than required positional params ⇒ decline.
    /// 2. Type that arg under the CURRENT call-site `env` and resolve its CORE
    ///    class; a Dynamic / non-core / source-only arg ⇒ decline (we can only
    ///    witness against core/RBS classes, the existing witness gate).
    /// 3. Walk `pb.chain` through the SAME `method_return` table tier 3 uses: each
    ///    no-arg core method must yield a registered core return; any miss ⇒
    ///    decline. The chain is core-only and uses the already-built index — it
    ///    cannot re-enter the in-source return inference, so there is no recursion
    ///    into the build pass and no fixpoint in this slice.
    fn resolve_param_bound(
        &self,
        ast: &LoweredAst,
        pb: &ParamBoundReturn,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<String> {
        // Gate 1: the bound positional arg must be present.
        let &arg_id = args.get(pb.param_index)?;
        // Gate 2: type the arg under the call-site env; keep only a concrete CORE
        // class (a Dynamic / Constant-of-unknown / source-only carrier ⇒ None).
        let arg_ty = self.type_of(ast, arg_id, env, interner);
        let mut class_name = self.index.class_name_of(interner, arg_ty)?.to_string();
        if !self.index.knows_class(&class_name) {
            return None;
        }
        // Gate 3: walk the no-arg core chain. Each step must yield a registered
        // core return; otherwise decline.
        for step in &pb.chain {
            let ret = self.index.method_return(&class_name, step)?;
            if !self.index.knows_class(ret) {
                return None;
            }
            class_name = ret.to_string();
        }
        Some(class_name)
    }

    /// rigor-rs#450: the project-`sig/` return of a `Const.method` singleton
    /// call whose receiver constant names a class or module the PROJECT
    /// declares — the singleton twin of tier 4b's project-sig arm, in the
    /// same order (signature ahead of the body), as the reference's
    /// `RbsDispatch` tier sits above its body-inference tiers.
    ///
    /// A project-declared constant types `Dynamic` (the C1 shadow gate), so
    /// the `Singleton` arm below never sees it; this resolves the receiver
    /// NAME lexically instead (longest enclosing prefix first, as Ruby's
    /// lookup does) and types only the call's return, never the class
    /// object. A `::T` receiver resolves at the top level; `expr::T` and
    /// `self::T` decline (no lexical answer). Gated like the instance arm:
    /// the qualified name must be a project-sig class whose chain the
    /// reference can build (`project_sig_chain_ok`). The return comes from
    /// [`Self::project_sig_singleton_answer`] (a nilable return ⇒ `Dynamic`;
    /// a union is `None` in the flat slot ⇒ decline). A root refinement of
    /// the name in scope declines too (`refined_class_object_method`).
    fn project_sig_singleton_return(
        &self,
        ast: &LoweredAst,
        receiver: NodeId,
        name: &str,
        method: &str,
        interner: &mut Interner,
    ) -> Option<TypeId> {
        let Node::ConstantRead { dynamic_base, self_anchored, rooted, .. } = ast.get(receiver) else {
            return None;
        };
        if *dynamic_base || *self_anchored {
            return None;
        }
        let site = ast.get(receiver).span();
        let prefix = self.enclosing_prefix_in(ast, site);
        let raw = if *rooted { format!("::{name}") } else { name.to_string() };
        let qualified = self.source.resolve_declared_constant_at(&prefix, &raw)?;
        if !self.index.is_qualified_project_sig_class(&qualified)
            || !self.index.project_sig_chain_ok(&qualified)
            || self.index.is_synthesized_stub(&qualified)
        {
            return None;
        }
        if self.refined_class_object_method(ast, site, &qualified, method) {
            return None;
        }
        self.project_sig_singleton_answer(&qualified, method, interner)
    }

    /// The project-`sig/` answer for `qualified.method`, shared by the
    /// `Singleton` arm (issue #168) and [`Self::project_sig_singleton_return`]
    /// once each has gated the receiver: a tuple return, else the flat
    /// bare class. A NILABLE return (`String?`, `instance?`) answers
    /// `Dynamic` — the instance arm's `method_return_nilable` decline; the
    /// flat slot alone would answer the non-nil class (rigor-rs#450). `None`
    /// when the signature names no return the index can mint.
    fn project_sig_singleton_answer(
        &self,
        qualified: &str,
        method: &str,
        interner: &mut Interner,
    ) -> Option<TypeId> {
        if let Some(shapes) = self.index.receiver_singleton_tuple_return(qualified, method) {
            return Some(self.intern_rbs_tuple(&shapes, interner));
        }
        let (ret, nilable) =
            self.index.receiver_singleton_method_return_nilable(qualified, method)?;
        if nilable {
            return Some(interner.untyped());
        }
        let class_id = self.index.class_id(ret).or_else(|| self.source.class_id(ret))?;
        Some(interner.intern(Type::Nominal { class: class_id, args: vec![] }))
    }

    /// Whether a refinement in effect at `site` (a `using M` of a project
    /// module, whose lexical region runs from the call to the end of its
    /// innermost class/module scope — or of the file) defines `method` on a
    /// root a class object reaches — `Object` / `Module` / `Kernel` /
    /// `BasicObject`, `Class` when the receiver is not itself a module
    /// (`refine(Class)` does not reach `Comparable.name` — rigor-rs#467's
    /// control), or a module the object may extend (an unreadable term
    /// answers `true` — the decline side). Mirrors the rules driver's
    /// `refinement_reaches` / `refined_using_regions` scan.
    fn refined_class_object_method(
        &self,
        ast: &LoweredAst,
        site: rigor_parse::Span,
        receiver_class: &str,
        method: &str,
    ) -> bool {
        // Prefer the driver-populated scope table; recompute only when this
        // Typer was built without one (sig-gen / annotate paths).
        let owned_scopes;
        let scopes: &[(rigor_parse::Span, Vec<String>)] = if self.lexical_scopes.is_empty() {
            owned_scopes = crate::lexical_scopes(ast);
            &owned_scopes
        } else {
            self.lexical_scopes
        };
        let file_end = ast.iter().map(|(_, n)| n.span().1).max().unwrap_or(0);
        // A QUALIFIED name (`File::Constants`) needs the qualified lookup —
        // `refine(Class)` must not silence a module object's `name`
        // (rigor-rs#467 review).
        let recv_is_module: bool = self.index.is_module(receiver_class)
            || self.index.is_qualified_module(receiver_class);
        for (_, n) in ast.iter() {
            let Node::Call {
                receiver: None,
                method: m,
                args,
                span,
                ..
            } = n
            else {
                continue;
            };
            if m != "using" || args.len() != 1 || span.1 > site.0 {
                continue;
            }
            let Node::ConstantRead {
                name,
                dynamic_base: false,
                self_anchored: false,
                rooted,
                ..
            } = ast.get(args[0])
            else {
                continue;
            };
            let enclosing = scopes
                .iter()
                .filter(|(s, _)| s.0 <= span.0 && span.1 <= s.1)
                .min_by_key(|(s, _)| s.1 - s.0);
            let (end, prefix): (usize, &[String]) = match enclosing {
                Some((s, p)) => (s.1, p.as_slice()),
                None => (file_end, &[]),
            };
            if !(span.1 <= site.0 && site.0 < end) {
                continue;
            }
            let raw = if *rooted { format!("::{name}") } else { name.to_string() };
            let Some(module) = self.source.resolve_declared_constant_at(prefix, &raw) else {
                continue;
            };
            for row in self.source.refinements_of(&module) {
                if !(row.names_wildcard || row.names.iter().any(|x| x == method)) {
                    continue;
                }
                match row.target.as_deref() {
                    None => return true,
                    Some(t)
                        if ["Object", "Module", "Kernel", "BasicObject"].contains(&t) =>
                    {
                        return true
                    }
                    Some("Class") => {
                        if !recv_is_module {
                            return true;
                        }
                    }
                    // A module the class object may extend; an unknown
                    // constant may be one.
                    Some(t) => {
                        let known_not_module =
                            match self.source.resolve_declared_constant_at(prefix, t) {
                                Some(q) => self.source.project_declares_module(&q) == Some(false),
                                None => {
                                    self.index.knows_class(t)
                                        || self.index.knows_qualified_class(t)
                                }
                            };
                        if !known_not_module {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// Reference `MethodDispatcher::ShapeDispatch` (`shape_dispatch.rb`)
    /// refinement projections over the carriers the port can name
    /// (rigor-rs#696):
    ///
    /// - `integer_to_s_projection` — `Integer#to_s` / `#inspect` on a bare
    ///   `Nominal[Integer]` answer `decimal-int-string` (no argument, or a
    ///   `Constant[10]` base); any other single argument answers
    ///   `non-empty-string` (`to_s(base)` never returns `""`, but a
    ///   non-decimal base's digits match no registered refinement). More
    ///   than one argument declines.
    /// - `float_to_s_projection` — `Float#to_s` / `#inspect` (no argument)
    ///   answer `non-empty-string`: a bare `Float` admits `NaN` / the
    ///   infinities, so the floor is the total non-empty conversion.
    /// - `dispatch_integer_range` — `to_s` on an `IntegerRange` whose lower
    ///   bound is provably `>= 0` (a signed range declines entirely — the
    ///   result could carry a `-`). The base argument must be statically
    ///   known: a non-`Constant` base declines to RBS, unlike the
    ///   `Nominal[Integer]` tier's non-empty floor.
    /// - `REFINED_STRING_PROJECTIONS` (the `decimal_int` rows) —
    ///   `upcase` / `downcase` on `decimal-int-string` are `refined_self`:
    ///   digit-only strings are case-invariant.
    /// - `dispatch_string_binary_from_arg` — `String + non-empty-string` is
    ///   `non-empty-string` (the argument guarantees it) and `String * 0` is
    ///   `""`; these apply to ANY `String`-typed receiver in the reference,
    ///   so the arms sit on the `Nominal`/`Refined`/`Difference` rows alike.
    /// - `empty_removal_projection` on `non-empty-string` —
    ///   `size`/`length`/`bytesize` are `positive-int`, `empty?` is `false`,
    ///   the `NON_EMPTY_STRING_PRESERVING_UNARY` family keeps the
    ///   difference, `+` keeps it for any argument, and `*` repeats it for a
    ///   provably positive count (`* 0` is `""`).
    ///
    /// Returns `None` (declines) for every other method or receiver — the
    /// caller then runs ordinary RBS dispatch on the carrier's erased base
    /// class, which is exactly what ShapeDispatch's own
    /// `dispatch_nominal_size` fallback does.
    fn refinement_projection(
        &self,
        ast: &LoweredAst,
        recv_ty: TypeId,
        method: &str,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<TypeId> {
        match interner.get(recv_ty).clone() {
            Type::Nominal { class, .. } => match self.index.class_name_for_id(class) {
                Some("Integer") => self
                    .integer_to_s_projection(ast, method, args, env, interner)
                    .or_else(|| {
                        self.integer_binary_projection(ast, method, args, env, interner)
                    }),
                Some("Float") => self.float_to_s_projection(method, args, interner),
                // `nominal_projection` + `dispatch_nominal_size`: the
                // arg-driven String rows first, then the size selectors.
                Some(name @ ("String" | "Array" | "Hash" | "Set" | "Range")) => {
                    (if name == "String" {
                        self.string_binary_projection(ast, method, args, env, interner)
                    } else {
                        None
                    })
                    .or_else(|| self.size_returning_projection(name, method, args, interner))
                }
                _ => None,
            },
            Type::IntegerRange { min, .. } => {
                // `dispatch_integer_range` handles `to_s` only, and only when
                // the range cannot produce a leading `-` (`inspect` declines).
                if method == "to_s" && min.is_some_and(|lo| lo >= 0) {
                    let base = self.to_s_base_argument(ast, args, env, interner)?;
                    Some(if base == 10 {
                        self.decimal_int_string(interner)?
                    } else {
                        self.non_empty_string(interner)?
                    })
                } else {
                    None
                }
            }
            Type::Refined { base, refinement } => {
                if !self.is_refined_string(interner, base, refinement) {
                    return None;
                }
                match method {
                    // `%i[decimal_int upcase|downcase] => :refined_self`.
                    "upcase" | "downcase" if args.is_empty() => Some(recv_ty),
                    // `dispatch_refined` delegates the size selectors to the
                    // String nominal tier (`non-negative-int`).
                    "size" | "length" | "bytesize" if args.is_empty() => {
                        Some(interner.intern(Type::IntegerRange { min: Some(0), max: None }))
                    }
                    // The `Refined` rows ShapeDispatch delegates to the
                    // String nominal fallback for.
                    "+" | "*" => {
                        self.string_binary_projection(ast, method, args, env, interner)
                    }
                    _ => None,
                }
            }
            Type::Difference { base, removed } => {
                if self.empty_witness_base_class(interner, base, removed) != Some("String") {
                    return None;
                }
                self.non_empty_string_projection(ast, recv_ty, method, args, env, interner)
            }
            _ => None,
        }
    }

    /// `integer_to_s_projection` (`shape_dispatch.rb`): `to_s` /
    /// `inspect` on a bare `Nominal[Integer]`.
    fn integer_to_s_projection(
        &self,
        ast: &LoweredAst,
        method: &str,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<TypeId> {
        match method {
            "inspect" if args.is_empty() => self.decimal_int_string(interner),
            "to_s" => match args.len() {
                0 => self.decimal_int_string(interner),
                1 => {
                    let arg_ty = self.type_of(ast, args[0], env, interner);
                    if Self::decimal_base(interner, arg_ty) {
                        self.decimal_int_string(interner)
                    } else {
                        self.non_empty_string(interner)
                    }
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// `float_to_s_projection` (`shape_dispatch.rb`): `to_s` / `inspect`
    /// (no argument) on a bare `Nominal[Float]` floor at `non-empty-string`.
    fn float_to_s_projection(
        &self,
        method: &str,
        args: &[NodeId],
        interner: &mut Interner,
    ) -> Option<TypeId> {
        if matches!(method, "to_s" | "inspect") && args.is_empty() {
            self.non_empty_string(interner)
        } else {
            None
        }
    }

    /// `dispatch_integer_binary_from_arg` (`shape_dispatch.rb`):
    /// `Integer * Constant[0]` is `Constant[0]` on any Integer receiver.
    fn integer_binary_projection(
        &self,
        ast: &LoweredAst,
        method: &str,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<TypeId> {
        if method != "*" || args.len() != 1 {
            return None;
        }
        let arg_ty = self.type_of(ast, args[0], env, interner);
        if matches!(interner.get(arg_ty), Type::Constant(Scalar::Int(0))) {
            Some(interner.intern(Type::Constant(Scalar::Int(0))))
        } else {
            None
        }
    }

    /// `SIZE_RETURNING_NOMINALS` + `dispatch_nominal_size` / the
    /// `Refined`/`Difference` delegation (`shape_dispatch.rb`): the
    /// `size`-family selectors on a nominal collection/String tighten the
    /// RBS `Integer` to `non_negative_int`. The `Refined[String]` delegate
    /// takes the same `0..` answer (a `decimal-int-string` may be `"0"`);
    /// the `non-empty-string` `Difference` has its own `positive-int` arm in
    /// [`Self::non_empty_string_projection`].
    fn size_returning_projection(
        &self,
        class_name: &str,
        method: &str,
        args: &[NodeId],
        interner: &mut Interner,
    ) -> Option<TypeId> {
        if !args.is_empty() {
            return None;
        }
        let selector = match class_name {
            "String" => matches!(method, "length" | "size" | "bytesize"),
            "Array" | "Hash" | "Set" | "Range" => matches!(method, "size" | "length" | "count"),
            _ => return None,
        };
        if selector {
            Some(interner.intern(Type::IntegerRange { min: Some(0), max: None }))
        } else {
            None
        }
    }

    /// `decimal_base?` (`shape_dispatch.rb`): the `to_s` base argument is
    /// decimal iff it pins `Constant[10]`.
    fn decimal_base(interner: &Interner, arg_ty: TypeId) -> bool {
        matches!(interner.get(arg_ty), Type::Constant(Scalar::Int(10)))
    }

    /// `base_argument` (`shape_dispatch.rb`) for the `IntegerRange` tier:
    /// the `to_s` base must be a statically known `Constant` Integer —
    /// anything else declines (`None`), including an untyped argument.
    fn to_s_base_argument(
        &self,
        ast: &LoweredAst,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<i64> {
        match args {
            [] => Some(10),
            [arg] => {
                let arg_ty = self.type_of(ast, *arg, env, interner);
                match interner.get(arg_ty) {
                    Type::Constant(Scalar::Int(v)) => Some(*v),
                    // A Bignum `Constant` is an Integer too, and never 10.
                    Type::Constant(Scalar::BigInt(_)) => Some(-1),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// `dispatch_string_binary_from_arg` (`shape_dispatch.rb`): the
    /// arg-driven `String` projections shared by every `String`-typed
    /// receiver — `+` on a `non-empty-string` argument and `* 0`.
    fn string_binary_projection(
        &self,
        ast: &LoweredAst,
        method: &str,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<TypeId> {
        let [arg] = args else { return None };
        let arg_ty = self.type_of(ast, *arg, env, interner);
        match method {
            "+" if self.non_empty_string_compatible(interner, arg_ty) => {
                self.non_empty_string(interner)
            }
            "*" if matches!(interner.get(arg_ty), Type::Constant(Scalar::Int(0))) => {
                Some(interner.intern(Type::Constant(Scalar::Str(String::new()))))
            }
            _ => None,
        }
    }

    /// `empty_removal_projection` (`shape_dispatch.rb`) over a
    /// `non-empty-string` (`Difference[String, Constant[""]]`) receiver.
    fn non_empty_string_projection(
        &self,
        ast: &LoweredAst,
        recv_ty: TypeId,
        method: &str,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<TypeId> {
        match (method, args.len()) {
            // `size_returning_for_empty_removal` — String selectors.
            ("size" | "length" | "bytesize", 0) => Some(interner.intern(
                Type::IntegerRange { min: Some(1), max: None },
            )),
            // `empty_predicate_projection` — a non-empty String's `empty?`.
            ("empty?", 0) => Some(interner.intern(Type::Constant(Scalar::Bool(false)))),
            // `NON_EMPTY_STRING_PRESERVING_UNARY` — case and order
            // transforms never reduce a string to `""`.
            ("upcase" | "downcase" | "capitalize" | "swapcase" | "reverse", 0) => {
                Some(recv_ty)
            }
            // `empty_string_binary`: concatenation keeps non-emptiness for
            // ANY argument type.
            ("+", 1) => Some(recv_ty),
            // `non_empty_string_repeat`: `* 0` is `""`, a provably positive
            // count keeps the difference, anything else declines to RBS.
            ("*", 1) => {
                let arg_ty = self.type_of(ast, args[0], env, interner);
                match interner.get(arg_ty) {
                    Type::Constant(Scalar::Int(0)) => {
                        Some(interner.intern(Type::Constant(Scalar::Str(String::new()))))
                    }
                    Type::Constant(Scalar::Int(v)) if *v > 0 => Some(recv_ty),
                    // A positive Bignum is past `i64` — always `>= 1`.
                    Type::Constant(Scalar::BigInt(d)) if !d.starts_with('-') => {
                        Some(recv_ty)
                    }
                    Type::IntegerRange { min: Some(0), max: Some(0) } => {
                        Some(interner.intern(Type::Constant(Scalar::Str(String::new()))))
                    }
                    Type::IntegerRange { min: Some(lo), .. } if *lo >= 1 => Some(recv_ty),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// `Type::Combinator.non_empty_string_compatible?` — the carrier is a
    /// `non-empty-string` `Difference` itself, an `Intersection` with such a
    /// member, or a `Union` whose members all are.
    fn non_empty_string_compatible(&self, interner: &Interner, ty: TypeId) -> bool {
        match interner.get(ty).clone() {
            Type::Difference { base, removed } => {
                self.empty_witness_base_class(interner, base, removed) == Some("String")
            }
            Type::Intersection(ms) => ms
                .iter()
                .any(|&m| self.non_empty_string_compatible(interner, m)),
            Type::Union(ms) => {
                !ms.is_empty()
                    && ms.iter()
                        .all(|&m| self.non_empty_string_compatible(interner, m))
            }
            _ => false,
        }
    }

    /// `Difference#removes_empty_witness?` (`type/difference.rb`): `removed`
    /// is the empty witness of a `Nominal` base (`""` for String, `0` for
    /// Integer, `Tuple[]` for Array, `HashShape{}` for Hash); answers the
    /// base's class name when it is.
    fn empty_witness_base_class(
        &self,
        interner: &Interner,
        base: TypeId,
        removed: TypeId,
    ) -> Option<&'static str> {
        let Type::Nominal { class, .. } = interner.get(base) else {
            return None;
        };
        let name = self.index.class_name_for_id(*class)?;
        let is_empty_witness = match name {
            "String" => {
                matches!(interner.get(removed), Type::Constant(Scalar::Str(s)) if s.is_empty())
            }
            "Integer" => matches!(interner.get(removed), Type::Constant(Scalar::Int(0))),
            "Array" => matches!(interner.get(removed), Type::Tuple(elems) if elems.is_empty()),
            "Hash" => {
                matches!(interner.get(removed), Type::HashShape(members) if members.is_empty())
            }
            _ => return None,
        };
        is_empty_witness.then_some(name)
    }

    /// The receiver is the `Refined[String, decimal_int]` carrier —
    /// `decimal-int-string`, the only `Refined` the port mints today.
    fn is_refined_string(&self, interner: &Interner, base: TypeId, refinement: RefinementId) -> bool {
        refinement == RefinementId::DECIMAL_INT
            && matches!(
                interner.get(base),
                Type::Nominal { class, .. }
                    if self.index.class_name_for_id(*class) == Some("String")
            )
    }

    /// `Type::Combinator.decimal_int_string` — `Refined[String, :decimal_int]`.
    fn decimal_int_string(&self, interner: &mut Interner) -> Option<TypeId> {
        let class = self.index.class_id("String")?;
        let base = interner.intern(Type::Nominal { class, args: vec![] });
        Some(interner.intern(Type::Refined {
            base,
            refinement: RefinementId::DECIMAL_INT,
        }))
    }

    /// `Type::Combinator.non_empty_string` —
    /// `Difference[String, Constant[""]]`.
    fn non_empty_string(&self, interner: &mut Interner) -> Option<TypeId> {
        let class = self.index.class_id("String")?;
        let base = interner.intern(Type::Nominal { class, args: vec![] });
        let empty = interner.intern(Type::Constant(Scalar::Str(String::new())));
        Some(rigor_types::Algebra::difference(interner, base, empty))
    }

    /// Issue #479 — reference `LiteralStringFolding.fold_concat`
    /// (`literal_string_folding.rb`, the tier after ConstantFolding, which
    /// never folds these two mutators): `<<` / `concat` with one argument, a
    /// literal-bearing receiver and a literal-bearing argument answers
    /// `literal-string` (`& non-empty-string`), a carrier the port does not
    /// model. The caller types the call untyped (silent), as the oversized-fold
    /// decline does, instead of the RBS `-> self` `String`, which witnessed
    /// `"a".concat("b").zzz`. (A `Dynamic[String]` would still dispatch a
    /// chained call on its `String` facet, `.upcase.zzz`, where the reference's
    /// carrier stays a refinement.) A Dynamic argument may be a
    /// `literal-string` the port lost (an oversized fold), so it declines too.
    /// The reference ignores a block here, and so does this check.
    ///
    /// A bare-local receiver typed plain `String` declines as well (and so
    /// does a self-returning call on one, see below): a rule
    /// re-types an enclosing call's receiver (`c.concat("d").zzz`) under the
    /// env AT that enclosing call, which already carries this very call's
    /// content-mutation widening of `c` (`"c"` → `String`), while the reference
    /// typed `c` before the mutation. The port cannot tell that widening from a
    /// genuine `String` binding, so both stay silent (a coverage loss on the
    /// genuine one, never a diagnostic).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn literal_concat_declines(
        &self,
        ast: &LoweredAst,
        receiver: NodeId,
        recv_ty: TypeId,
        method: &str,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> bool {
        if !matches!(method, "<<" | "concat") || args.len() != 1 {
            return false;
        }
        // The reference folds these to the receiver `Constant` (`ConstantFolding
        // CONSTANT_SELF_RETURNERS`, the unary `+@` / `-@`, `to_s` / `to_str`);
        // the port types them `String`, so `"a".dup.concat("b")` reads a plain
        // `String` receiver the reference keeps literal.
        let self_returner = |n: &Node| match n {
            Node::Call { receiver: Some(_), method, args, block_body, .. } => {
                args.is_empty()
                    && block_body.is_empty()
                    && matches!(
                        method.as_str(),
                        "freeze" | "itself" | "dup" | "clone" | "+@" | "-@" | "to_s" | "to_str"
                    )
            }
            _ => false,
        };
        let recv_node = ast.get(receiver);
        let local_string_receiver = (matches!(recv_node, Node::LocalVariableRead { .. })
            || self_returner(recv_node))
            && matches!(interner.get(recv_ty), Type::Nominal { args, .. } if args.is_empty())
            && self.index.class_name_of(interner, recv_ty) == Some("String");
        if !(literal_string_compatible(recv_ty, interner) || local_string_receiver) {
            return false;
        }
        let arg_ty = self.type_of(ast, args[0], env, interner);
        literal_string_compatible(arg_ty, interner)
            || matches!(interner.get(arg_ty), Type::Dynamic(_))
    }
}

/// Reference `Type::Combinator.literal_string_compatible?`, over the carriers
/// the port models: a `Constant<String>`, or a union whose every member is
/// one. (The `literal-string` refinement itself the port never mints.)
fn literal_string_compatible(ty: TypeId, interner: &Interner) -> bool {
    match interner.get(ty) {
        Type::Constant(rigor_types::Scalar::Str(_)) => true,
        Type::Union(members) => {
            !members.is_empty() && members.iter().all(|&m| literal_string_compatible(m, interner))
        }
        _ => false,
    }
}
