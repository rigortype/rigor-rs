//! Call dispatch: typing a block-less method call ([`Typer::type_call`]) —
//! `X.new` instances, literal folding, and the RBS return lookup (singleton
//! and object-constant receivers included), with its untyped-argument and
//! bare-nominal-join declines and in-source parameter bounds.

use rigor_parse::{LoweredAst, Node, NodeId};
use rigor_types::{Interner, Type, TypeId};

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
    pub(crate) fn type_call(
        &self,
        ast: &LoweredAst,
        receiver: NodeId,
        method: &str,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
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
        if let Node::ConstantRead { name, .. } = ast.get(receiver) {
            if !name.is_empty() {
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
                        env.get(&crate::indexed_narrowing_key(name, &key))
                    {
                        return recorded;
                    }
                }
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
            if let Some(ty) = self.type_implicit_self_call(ast, method, args, env, interner) {
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
                    if let Some(shapes) =
                        self.index.receiver_singleton_tuple_return(class_name, method)
                    {
                        return self.intern_rbs_tuple(&shapes, interner);
                    }
                    if let Some(ret) =
                        self.index.receiver_singleton_method_return(class_name, method)
                    {
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
        if let Node::ConstantRead { name, span, .. } = ast.get(receiver) {
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
                // A String lookup can answer `nil`, so a stale value flips the
                // result's CLASS. The top-level flat env keeps a local's first
                // literal across `<<`, `+=`, branch and block writes, so a lookup
                // that reads a local declines to the RBS answer (#149 review:
                // `buf = ""; buf << "x"; buf[0].upcase` fired `for nil`).
                // `Float#<=>` joins them: its `nil` arm has the same problem.
                let stale_risk = folding::is_nilable_fold(&scalar, method)
                    && std::iter::once(receiver)
                        .chain(args.iter().copied())
                        .any(|id| ast.reads_local_within(ast.get(id).span()));
                if let Some(folded) =
                    (!stale_risk).then(|| folding::fold(&scalar, method, &arg_scalars)).flatten()
                {
                    return interner.intern(Type::Constant(folded));
                }
                // Issue #164: for these `Integer?` lookups an unfolded literal
                // call must not reach tier 3's bare `Integer` (see the predicate).
                if folding::declines_unfolded(&scalar, method) {
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
                            return interner.intern(Type::Constant(folded));
                        }
                    }
                }
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
}
