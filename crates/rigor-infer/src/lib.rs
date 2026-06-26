//! The inference engine (ADR-0004/0005): flow-sensitive inference, narrowing,
//! RBS method-type translation, typed dispatch. Pure query functions take the
//! db explicitly (ADR-0006 — Salsa-ready, not Salsa-bound). Constant folding
//! splits between a conservative Rust core and the cached Ruby sidecar
//! (ADR-0008); foldability is decided here from an embedded catalogue.
//!
//! ## Tracer-bullet expression typer
//!
//! This slice ships the smallest [`type_of`] able to type the *receiver* of a
//! call: string/integer literals fold to value-pinned `Constant` carriers, a
//! local read is resolved from a flat [`TypeEnv`] populated as statements are
//! walked in order, and everything else degrades to `Dynamic[top]` (ADR-0023
//! tier-5 fallback). The pure-function-dispatched-by-node-variant shape mirrors
//! the reference's `ExpressionTyper` (ADR-0023).
//!
// TODO(spec): flow sensitivity, narrowing, the full dispatch tier cascade
// (folding -> shape -> RBS -> in-source -> Dynamic) and budgets (ADR-0023/0024).
#![allow(dead_code)]

pub mod folding;
pub mod source_index;

use std::collections::HashMap;
use std::sync::OnceLock;

use rigor_index::CoreIndex;
use rigor_parse::{LoweredAst, Node, NodeId};
use rigor_types::{Interner, Scalar, Type, TypeId};

pub use source_index::{SourceIndex, SOURCE_CLASS_BASE};

/// A process-wide empty [`SourceIndex`], used as the default `source` for a
/// [`Typer`] built via [`Typer::new`] (callers that predate in-source typing).
/// Sharing one empty index keeps `Typer::new` allocation-free and infallible.
fn empty_source() -> &'static SourceIndex {
    static EMPTY: OnceLock<SourceIndex> = OnceLock::new();
    EMPTY.get_or_init(SourceIndex::default)
}

/// A flat name -> type binding environment, populated by `LocalVariableWrite`
/// as the statement sequence is walked in order. Intentionally not
/// flow-sensitive in this slice.
pub type TypeEnv = HashMap<String, TypeId>;

/// The expression typer (ADR-0023: the reference's `ExpressionTyper` /
/// `MethodDispatcher` split). Holds a borrow of the [`CoreIndex`] so it can
/// resolve a receiver's class and a method's return type — the data a CHAINED
/// call needs to type correctly (`s.downcase : String`, so the next `.lenght`
/// can be flagged).
///
/// The index is a *field*, not a per-call parameter, so the existing free
/// [`type_of`] / [`build_toplevel_env`] signatures stay source-compatible: they
/// are thin wrappers over a [`Typer`] built with an empty index. Callers that
/// want chained-call result typing construct a [`Typer`] with the real index.
pub struct Typer<'i> {
    index: &'i CoreIndex,
    /// The per-run in-source class index (ADR-0023 tier-4). Empty for a
    /// [`Typer::new`] caller; real for [`Typer::with_source`]. Lets `X.new` type
    /// to an instance of a project-defined class and a typo on it be witnessed.
    source: &'i SourceIndex,
}

impl<'i> Typer<'i> {
    /// Build a typer over a borrowed core index, with an EMPTY source index
    /// (no in-source typing). Kept for callers that predate tier-4.
    pub fn new(index: &'i CoreIndex) -> Self {
        Typer { index, source: empty_source() }
    }

    /// Build a typer over a borrowed core index AND a per-run [`SourceIndex`],
    /// enabling `X.new` instance typing and in-source method resolution.
    pub fn with_source(index: &'i CoreIndex, source: &'i SourceIndex) -> Self {
        Typer { index, source }
    }

    /// The borrowed source index (for the rules layer's method-resolution gate).
    pub fn source(&self) -> &SourceIndex {
        self.source
    }

    /// The borrowed core index.
    pub fn core(&self) -> &CoreIndex {
        self.index
    }

    /// Type an owned-AST node against the current `env`, interning carriers into
    /// `interner`. Pure dispatch by node variant (ADR-0023): never mutates the
    /// AST, only reads `env`.
    ///
    /// - `StringLit` -> `Constant["..."]`
    /// - `IntegerLit` -> `Constant[n]`
    /// - `LocalVariableRead` -> the env binding, else `Dynamic[top]`
    /// - `Call { receiver: Some(r), method, .. }` -> the dispatch cascade below
    /// - anything else -> `Dynamic[top]` (`Interner::untyped`)
    ///
    /// Returning `untyped` (rather than guessing) on an unknown is the
    /// load-bearing behaviour that keeps downstream rules zero-false-positive
    /// (ADR-0023 tier-5).
    pub fn type_of(&self, ast: &LoweredAst, id: NodeId, env: &TypeEnv, interner: &mut Interner) -> TypeId {
        match ast.get(id) {
            Node::StringLit { value, .. } => {
                interner.intern(Type::Constant(Scalar::Str(value.clone())))
            }
            Node::IntegerLit { value, .. } => {
                interner.intern(Type::Constant(Scalar::Int(*value)))
            }
            Node::FloatLit { value, .. } => {
                interner.intern(Type::Constant(Scalar::Float(*value)))
            }
            Node::SymbolLit { value, .. } => {
                interner.intern(Type::Constant(Scalar::Sym(value.clone())))
            }
            Node::NilLit { .. } => interner.intern(Type::Constant(Scalar::Nil)),
            Node::TrueLit { .. } => interner.intern(Type::Constant(Scalar::Bool(true))),
            Node::FalseLit { .. } => interner.intern(Type::Constant(Scalar::Bool(false))),
            Node::LocalVariableRead { name, .. } => env
                .get(name)
                .copied()
                .unwrap_or_else(|| interner.untyped()),
            Node::Call { receiver: Some(r), method, args, .. } => {
                let (r, method, args) = (*r, method.clone(), args.clone());
                self.type_call(ast, r, &method, &args, env, interner)
            }
            // An array/hash literal types to its bare nominal class so a typo'd
            // method on it (`[1,2].frist`, `{}.fetchh`) flags via the real
            // Array/Hash RBS — matching the reference. Element/shape precision is
            // deferred (TODO(spec): Tuple / HashShape per ADR-0023).
            Node::ArrayLit { .. } => self.nominal_or_untyped("Array", interner),
            Node::HashLit { .. } => self.nominal_or_untyped("Hash", interner),
            // A call with no receiver (implicit self) or any other carrier
            // (`@ivar`, constant, `self`, `if`/`case`-as-expression, index,
            // range, logical, variable read) is not precisely typed in this
            // slice -> Dynamic[top] (never guess; keeps the call rule silent).
            // TODO(spec): ivar typing (ADR-0022), constant resolution,
            // branch-union typing, container-element typing.
            _ => interner.untyped(),
        }
    }

    /// Intern a bare `Nominal { class }` for a registered core class name, or
    /// `Dynamic[top]` if the index doesn't register it. Used to type a literal
    /// container (array/hash) so a typo'd method on it resolves against the real
    /// RBS for that class, while staying silent if the class is somehow unknown.
    fn nominal_or_untyped(&self, class_name: &str, interner: &mut Interner) -> TypeId {
        match self.index.class_id(class_name) {
            Some(class) => interner.intern(Type::Nominal { class, args: vec![] }),
            None => interner.untyped(),
        }
    }

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
    fn type_call(
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
            if let Node::ConstantRead { name, .. } = ast.get(receiver) {
                if !name.is_empty() {
                    // Prefer a core (CORE_CLASSES) nominal id when the name maps
                    // to one — its method existence resolves via the core path.
                    if let Some(class_id) = self.index.class_id(name) {
                        return interner.intern(Type::Nominal { class: class_id, args: vec![] });
                    }
                    // Else a source class OR a registered RBS-only instance class
                    // (e.g. Pathname) carries a registry id in the high range.
                    if let Some(class_id) = self.source.class_id(name) {
                        return interner.intern(Type::Nominal { class: class_id, args: vec![] });
                    }
                    // Unknown constant ⇒ fall through to Dynamic (never guess).
                }
            }
        }

        let recv_ty = self.type_of(ast, receiver, env, interner);

        // Tier 1: constant folding on a value-pinned receiver. Fold only when
        // EVERY argument also types to a value-pinned `Constant` (ADR-0008
        // zero-FP: a non-pinned arg means we can't prove the result, so we
        // decline and widen to the nominal return / Dynamic below — never
        // guess). The nullary case (`args` empty) folds the no-arg core.
        if let Type::Constant(scalar) = interner.get(recv_ty).clone() {
            if let Some(arg_scalars) = self.pin_arg_scalars(ast, args, env, interner) {
                if let Some(folded) = folding::fold(&scalar, method, &arg_scalars) {
                    return interner.intern(Type::Constant(folded));
                }
            }
        }

        // Tier 3 (-ish): resolve receiver class -> method return class.
        if let Some(class_name) = self.index.class_name_of(interner, recv_ty) {
            if let Some(ret_class) = rigor_index::method_return(class_name, method) {
                if let Some(class_id) = self.index.class_id(ret_class) {
                    return interner.intern(Type::Nominal {
                        class: class_id,
                        args: vec![],
                    });
                }
            }
        }

        // Tier 5: unknown -> Dynamic[top].
        interner.untyped()
    }

    /// Type each argument and, if *every* one is a value-pinned `Constant`,
    /// return the owned scalars in order — the input [`folding::fold`] needs to
    /// compute a byte-exact result. Returns `None` the moment any argument is
    /// not a pinned `Constant` (Dynamic / Nominal / unknown), so the caller
    /// declines to fold rather than guessing (ADR-0008 zero-FP).
    fn pin_arg_scalars(
        &self,
        ast: &LoweredAst,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<Vec<Scalar>> {
        let mut out = Vec::with_capacity(args.len());
        for &arg in args {
            let ty = self.type_of(ast, arg, env, interner);
            match interner.get(ty) {
                Type::Constant(scalar) => out.push(scalar.clone()),
                _ => return None,
            }
        }
        Some(out)
    }

    /// Walk the top-level statement sequence in source order, binding each
    /// `LocalVariableWrite`'s name to the type of its value expression, and
    /// return the resulting [`TypeEnv`].
    ///
    /// This is the minimal flow needed so a later `s.lenght` can see `s :
    /// Constant["Hello"]`. Nested scopes / reassignment narrowing are out of
    /// scope for the tracer bullet.
    // TODO(spec): real flow-sensitive scoping + narrowing across branches (ADR-0022).
    pub fn build_toplevel_env(&self, ast: &LoweredAst, interner: &mut Interner) -> TypeEnv {
        let mut env = TypeEnv::new();
        let body = match ast.get(ast.root()) {
            Node::Program { body, .. } => body.clone(),
            _ => return env,
        };
        for stmt in body {
            // A program body may wrap statements directly or via a Statements node.
            self.bind_statement(ast, stmt, &mut env, interner);
        }
        env
    }

    /// Bind a single statement into `env` if it is a local write; recurse
    /// through a `Statements` wrapper. Other statements have no binding effect.
    fn bind_statement(&self, ast: &LoweredAst, id: NodeId, env: &mut TypeEnv, interner: &mut Interner) {
        match ast.get(id) {
            Node::LocalVariableWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                let ty = self.type_of(ast, value, env, interner);
                env.insert(name, ty);
            }
            Node::Statements { body, .. } => {
                for s in body.clone() {
                    self.bind_statement(ast, s, env, interner);
                }
            }
            _ => {}
        }
    }
}

/// Type an owned-AST node against the current `env`. Free-function wrapper kept
/// source-compatible for callers (e.g. rigor-rules) that predate [`Typer`]; it
/// runs over an *empty* index, so a `Call` receiver types via folding only and
/// otherwise degrades to `Dynamic[top]`. Migrate to [`Typer::type_of`] (with the
/// real index) to get chained-call result typing.
///
/// - `StringLit` -> `Constant["..."]`
/// - `IntegerLit` -> `Constant[n]`
/// - `LocalVariableRead` -> the env binding, else `Dynamic[top]`
/// - anything else -> `Dynamic[top]` (`Interner::untyped`)
pub fn type_of(ast: &LoweredAst, id: NodeId, env: &TypeEnv, interner: &mut Interner) -> TypeId {
    let empty = CoreIndex::new();
    Typer::new(&empty).type_of(ast, id, env, interner)
}

/// Walk the top-level statement sequence binding each local write. Free-function
/// wrapper over an empty-index [`Typer`], kept source-compatible (see
/// [`type_of`]).
// TODO(spec): real flow-sensitive scoping + narrowing across branches (ADR-0022).
pub fn build_toplevel_env(ast: &LoweredAst, interner: &mut Interner) -> TypeEnv {
    let empty = CoreIndex::new();
    Typer::new(&empty).build_toplevel_env(ast, interner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rigor_parse::{lower, parse};

    fn lower_src(src: &[u8]) -> LoweredAst {
        lower(&parse(src))
    }

    #[test]
    fn types_string_and_integer_literals() {
        let ast = lower_src(b"\"Hello\"\n42\n");
        let mut i = Interner::new();
        let env = TypeEnv::new();
        // Locate the two literal nodes and type them.
        let str_id = ast
            .iter()
            .find_map(|(id, n)| matches!(n, Node::StringLit { .. }).then_some(id))
            .unwrap();
        let int_id = ast
            .iter()
            .find_map(|(id, n)| matches!(n, Node::IntegerLit { .. }).then_some(id))
            .unwrap();
        let str_ty = type_of(&ast, str_id, &env, &mut i);
        assert_eq!(i.get(str_ty), &Type::Constant(Scalar::Str("Hello".into())));
        let int_ty = type_of(&ast, int_id, &env, &mut i);
        assert_eq!(i.get(int_ty), &Type::Constant(Scalar::Int(42)));
    }

    #[test]
    fn local_read_resolves_from_env() {
        let ast = lower_src(b"s = \"Hello\"\ns.length\n");
        let mut i = Interner::new();
        let env = build_toplevel_env(&ast, &mut i);
        assert_eq!(
            env.get("s").copied().map(|t| i.get(t).clone()),
            Some(Type::Constant(Scalar::Str("Hello".into())))
        );
    }

    #[test]
    fn unknown_receiver_is_dynamic_top() {
        // In Ruby, a bare `x` with no prior assignment parses as the
        // implicit-self call `x()`, so the receiver of `.foo` is a `Call`, not
        // a local read. Either way, an unknown carrier types as Dynamic[top],
        // which is what keeps the call rule silent (ADR-0023 tier-5).
        let ast = lower_src(b"x.foo\n");
        let mut i = Interner::new();
        let env = build_toplevel_env(&ast, &mut i);
        // The receiver node of the outer `.foo` call.
        let recv_id = ast
            .iter()
            .find_map(|(_, n)| match n {
                Node::Call { receiver: Some(r), method, .. } if method == "foo" => Some(*r),
                _ => None,
            })
            .unwrap();
        let ty = type_of(&ast, recv_id, &env, &mut i);
        assert_eq!(ty, i.untyped());
    }

    /// Find the `Call` node whose method matches `name`, returning its id.
    fn find_call(ast: &LoweredAst, name: &str) -> NodeId {
        ast.iter()
            .find_map(|(id, n)| match n {
                Node::Call { method, .. } if method == name => Some(id),
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected a call to `{name}`"))
    }

    #[test]
    fn folds_integer_addition_to_constant() {
        // `1 + 2` lowers to a Call `+` on receiver `1` with positional arg `2`;
        // now that args are lowered, binary folding runs and pins Constant[3].
        let ast = lower_src(b"1 + 2\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let call = find_call(&ast, "+");
        let ty = typer.type_of(&ast, call, &env, &mut i);
        assert_eq!(i.get(ty), &Type::Constant(Scalar::Int(3)));
    }

    #[test]
    fn folds_nullary_integer_succ_to_constant() {
        // Nullary folding still works with the new arg threading.
        let ast = lower_src(b"42.succ\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let call = find_call(&ast, "succ");
        let ty = typer.type_of(&ast, call, &env, &mut i);
        assert_eq!(i.get(ty), &Type::Constant(Scalar::Int(43)));
    }

    #[test]
    fn typed_literals_pin_constants() {
        let ast = lower_src(b"nil\ntrue\nfalse\n:foo\n2.5\n");
        let mut i = Interner::new();
        let env = TypeEnv::new();
        let pick = |ast: &LoweredAst, pred: fn(&Node) -> bool| {
            ast.iter().find_map(|(id, n)| pred(n).then_some(id)).unwrap()
        };
        let nil = pick(&ast, |n| matches!(n, Node::NilLit { .. }));
        let t = pick(&ast, |n| matches!(n, Node::TrueLit { .. }));
        let f = pick(&ast, |n| matches!(n, Node::FalseLit { .. }));
        let sym = pick(&ast, |n| matches!(n, Node::SymbolLit { .. }));
        let fl = pick(&ast, |n| matches!(n, Node::FloatLit { .. }));
        let ty_of = |i: &mut Interner, id| {
            let t = type_of(&ast, id, &env, i);
            i.get(t).clone()
        };
        assert_eq!(ty_of(&mut i, nil), Type::Constant(Scalar::Nil));
        assert_eq!(ty_of(&mut i, t), Type::Constant(Scalar::Bool(true)));
        assert_eq!(ty_of(&mut i, f), Type::Constant(Scalar::Bool(false)));
        assert_eq!(ty_of(&mut i, sym), Type::Constant(Scalar::Sym("foo".into())));
        assert_eq!(ty_of(&mut i, fl), Type::Constant(Scalar::Float(2.5)));
    }

    #[test]
    fn non_pinned_argument_declines_folding() {
        // `x` is never assigned -> Dynamic, so `"a" + x` can't fold; the call
        // widens to the nominal String return rather than minting a Constant.
        let ast = lower_src(b"\"a\" + x\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let call = find_call(&ast, "+");
        let ty = typer.type_of(&ast, call, &env, &mut i);
        // String#+ -> String nominal (return-type path), NOT a folded Constant.
        assert_eq!(idx.class_name_of(&i, ty), Some("String"));
        assert!(!matches!(i.get(ty), Type::Constant(_)));
    }

    #[test]
    fn folds_string_upcase_to_constant() {
        // `"hi".upcase` -> Constant["HI"].
        let ast = lower_src(b"\"hi\".upcase\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let call = find_call(&ast, "upcase");
        let ty = typer.type_of(&ast, call, &env, &mut i);
        assert_eq!(i.get(ty), &Type::Constant(Scalar::Str("HI".into())));
    }

    #[test]
    fn folds_string_length_to_constant() {
        // `"hello".length` -> Constant[5] (value-pinned; the core folds it).
        let ast = lower_src(b"\"hello\".length\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let call = find_call(&ast, "length");
        let ty = typer.type_of(&ast, call, &env, &mut i);
        assert_eq!(i.get(ty), &Type::Constant(Scalar::Int(5)));
    }

    #[test]
    fn chained_call_result_types_to_string_nominal() {
        // `s = "Hello"; s.downcase` types to a String Nominal (folding pins the
        // value, but to exercise the return-type path we check the class
        // resolves to "String" via the index regardless). Then `.lenght` on a
        // String would be undefined.
        let ast = lower_src(b"s = \"Hello\"\ns.downcase\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let call = find_call(&ast, "downcase");
        let ty = typer.type_of(&ast, call, &env, &mut i);
        // Folding pins "hello"; its class still resolves to String, so a later
        // `.lenght` on the result is checkable as undefined.
        assert_eq!(idx.class_name_of(&i, ty), Some("String"));
        assert!(!idx.class_has_method("String", "lenght"));
    }

    #[test]
    fn return_type_resolves_when_receiver_not_folded() {
        // A receiver typed as a (non-constant) String Nominal exercises the
        // return-type table path: `String#downcase -> String`, and that result
        // resolves back to "String" so a chained typo is flagged.
        //
        // `s` must lower to a `LocalVariableRead` (which it does once assigned),
        // so we assign then override the env binding to a bare String Nominal
        // (no value pin) — defeating folding and forcing the return-type path.
        let ast = lower_src(b"s = \"Hello\"\ns.downcase\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let mut env = typer.build_toplevel_env(&ast, &mut i);
        let string_id = idx.class_id("String").unwrap();
        let recv = i.intern(Type::Nominal { class: string_id, args: vec![] });
        env.insert("s".into(), recv);

        let call = find_call(&ast, "downcase");
        let ty = typer.type_of(&ast, call, &env, &mut i);
        // Not folded (receiver isn't a Constant), so we get the Nominal return.
        assert_eq!(i.get(ty), &Type::Nominal { class: string_id, args: vec![] });
        assert_eq!(idx.class_name_of(&i, ty), Some("String"));
    }

    #[test]
    fn array_literal_types_to_array_nominal() {
        // `[1, 2]` types to a bare Array Nominal so a typo (`.frist`) is
        // checkable against the real Array RBS.
        let ast = lower_src(b"[1, 2]\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let arr = ast
            .iter()
            .find_map(|(id, n)| matches!(n, Node::ArrayLit { .. }).then_some(id))
            .unwrap();
        let ty = typer.type_of(&ast, arr, &env, &mut i);
        assert_eq!(idx.class_name_of(&i, ty), Some("Array"));
        assert!(!idx.class_has_method("Array", "frist"));
    }

    #[test]
    fn hash_literal_types_to_hash_nominal() {
        let ast = lower_src(b"{ a: 1 }\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let hash = ast
            .iter()
            .find_map(|(id, n)| matches!(n, Node::HashLit { .. }).then_some(id))
            .unwrap();
        let ty = typer.type_of(&ast, hash, &env, &mut i);
        assert_eq!(idx.class_name_of(&i, ty), Some("Hash"));
    }

    #[test]
    fn method_param_read_is_dynamic_top() {
        // Inside `def foo(x); x.bar; end`, the receiver `x` is a param read with
        // no top-level binding -> Dynamic[top] -> the call rule stays silent.
        // This is the zero-FP keystone for lowering def bodies.
        let ast = lower_src(b"def foo(x)\n  x.bar\nend\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let recv = ast
            .iter()
            .find_map(|(_, n)| match n {
                Node::Call { receiver: Some(r), method, .. } if method == "bar" => Some(*r),
                _ => None,
            })
            .unwrap();
        let ty = typer.type_of(&ast, recv, &env, &mut i);
        assert_eq!(ty, i.untyped());
    }

    #[test]
    fn ivar_and_self_and_const_reads_are_dynamic_top() {
        // `@x`, `self`, and a constant read all type to Dynamic[top] (silent).
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        for src in [b"@x.foo\n".as_slice(), b"self.foo\n".as_slice(), b"Foo.foo\n".as_slice()] {
            let ast = lower_src(src);
            let mut i = Interner::new();
            let env = typer.build_toplevel_env(&ast, &mut i);
            let recv = ast
                .iter()
                .find_map(|(_, n)| match n {
                    Node::Call { receiver: Some(r), method, .. } if method == "foo" => Some(*r),
                    _ => None,
                })
                .unwrap();
            let ty = typer.type_of(&ast, recv, &env, &mut i);
            assert_eq!(ty, i.untyped(), "receiver of {src:?} must be Dynamic[top]");
        }
    }

    #[test]
    fn non_deterministic_or_unknown_call_is_dynamic_top() {
        // `Array#sample` is non-deterministic: never folded, no modeled return
        // -> Dynamic[top]. Drive it on a value-pinned Integer receiver whose
        // unknown method has no return: `42.sample` (sample isn't on Integer).
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let ast = lower_src(b"42.sample\n");
        let env = typer.build_toplevel_env(&ast, &mut i);
        let call = find_call(&ast, "sample");
        let ty = typer.type_of(&ast, call, &env, &mut i);
        assert_eq!(ty, i.untyped());
    }

    // --- in-source class typing (ADR-0023 tier-4) ---------------------------

    #[test]
    fn source_class_new_types_to_source_instance() {
        // `class Point; def x; end; end; p = Point.new` — `Point.new` types to a
        // Nominal instance whose ClassId resolves back to "Point" via the source
        // index, and the source index witnesses `y` absent (chain complete:
        // implicit Object super, fully RBS-loaded).
        let ast = lower_src(b"class Point\n  def x\n  end\nend\np = Point.new\np.y\n");
        let idx = CoreIndex::new();
        let source = SourceIndex::build(&ast, &idx);
        let typer = Typer::with_source(&idx, &source);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        // `p` binds to the Point.new instance.
        let p_ty = *env.get("p").expect("p should be bound");
        let class = match i.get(p_ty) {
            Type::Nominal { class, .. } => *class,
            other => panic!("expected Nominal instance, got {other:?}"),
        };
        assert_eq!(source.class_name_for_id(class), Some("Point"));
        // `x` is defined, `y` is not — and the chain is complete.
        assert!(source.class_has_method(&idx, "Point", "x"));
        assert!(!source.class_has_method(&idx, "Point", "y"));
        // Inherited Object method is present (no false absence).
        assert!(source.class_has_method(&idx, "Point", "frozen?"));
    }

    #[test]
    fn unknown_superclass_makes_chain_incomplete_and_silent() {
        // `class User < ApplicationRecord; end` — ApplicationRecord is neither
        // source nor RBS ⇒ chain INCOMPLETE ⇒ any method is assumed present.
        let ast = lower_src(b"class User < ApplicationRecord\nend\nu = User.new\nu.anything\n");
        let idx = CoreIndex::new();
        let source = SourceIndex::build(&ast, &idx);
        assert!(source.knows_class("User"));
        // Even a clearly-bogus method is assumed present (zero-FP keystone).
        assert!(source.class_has_method(&idx, "User", "totally_made_up_xyz"));
        assert!(source.class_has_method(&idx, "User", "anything"));
    }

    #[test]
    fn reopened_source_class_unions_methods() {
        // Two `class C` bodies: the SourceIndex unions their methods.
        let ast = lower_src(b"class C\n  def a\n  end\nend\nclass C\n  def b\n  end\nend\n");
        let idx = CoreIndex::new();
        let source = SourceIndex::build(&ast, &idx);
        assert!(source.class_has_method(&idx, "C", "a"));
        assert!(source.class_has_method(&idx, "C", "b"));
        // A method on neither reopen is witnessed absent (complete chain).
        assert!(!source.class_has_method(&idx, "C", "c"));
    }

    #[test]
    fn source_superclass_chain_resolves_inherited_method() {
        // `class Animal; def speak; end; end; class Dog < Animal; end` —
        // Dog.new.speak is inherited (present); Dog.new.fly is absent (the whole
        // chain Dog -> Animal -> Object is known).
        let ast = lower_src(
            b"class Animal\n  def speak\n  end\nend\nclass Dog < Animal\nend\n",
        );
        let idx = CoreIndex::new();
        let source = SourceIndex::build(&ast, &idx);
        assert!(source.class_has_method(&idx, "Dog", "speak"));
        assert!(!source.class_has_method(&idx, "Dog", "fly"));
    }

    #[test]
    fn rbs_class_new_types_to_rbs_instance() {
        // `Pathname.new("a")` — Pathname is RBS-known (with the stdlib tree) but
        // outside CORE_CLASSES; the source registry carries its instance id and
        // method existence defers to RBS. Under the stub fallback Pathname is not
        // registered, so this test only asserts the registry/typing wiring when
        // Pathname is actually loaded.
        let ast = lower_src(b"p = Pathname.new(\"a\")\np.foo\n");
        let idx = CoreIndex::new();
        let source = SourceIndex::build(&ast, &idx);
        if idx.knows_class("Pathname") {
            let typer = Typer::with_source(&idx, &source);
            let mut i = Interner::new();
            let env = typer.build_toplevel_env(&ast, &mut i);
            let p_ty = *env.get("p").expect("p should be bound");
            let class = match i.get(p_ty) {
                Type::Nominal { class, .. } => *class,
                other => panic!("expected Nominal instance, got {other:?}"),
            };
            assert_eq!(source.class_name_for_id(class), Some("Pathname"));
            // A real Pathname method is present; a typo is absent (via RBS).
            assert!(source.class_has_method(&idx, "Pathname", "basename"));
            assert!(!source.class_has_method(&idx, "Pathname", "nonexist"));
        }
    }

    #[test]
    fn unknown_constant_new_is_dynamic() {
        // `Widget.new` where Widget is neither source nor RBS ⇒ Dynamic (silent).
        let ast = lower_src(b"w = Widget.new\nw.foo\n");
        let idx = CoreIndex::new();
        let source = SourceIndex::build(&ast, &idx);
        let typer = Typer::with_source(&idx, &source);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let w_ty = *env.get("w").expect("w should be bound");
        assert_eq!(w_ty, i.untyped(), "unknown-constant .new must be Dynamic[top]");
    }
}
