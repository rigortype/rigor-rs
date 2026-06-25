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

use std::collections::HashMap;

use rigor_parse::{LoweredAst, Node, NodeId};
use rigor_types::{Interner, Scalar, Type, TypeId};

/// A flat name -> type binding environment, populated by `LocalVariableWrite`
/// as the statement sequence is walked in order. Intentionally not
/// flow-sensitive in this slice.
pub type TypeEnv = HashMap<String, TypeId>;

/// Type an owned-AST node against the current `env`, interning carriers into
/// `interner`. Pure dispatch by node variant (ADR-0023): never mutates the AST,
/// only reads `env`.
///
/// - `StringLit` -> `Constant["..."]`
/// - `IntegerLit` -> `Constant[n]`
/// - `LocalVariableRead` -> the env binding, else `Dynamic[top]`
/// - anything else -> `Dynamic[top]` (`Interner::untyped`)
///
/// Returning `untyped` (rather than guessing) on an unknown is the load-bearing
/// behaviour that keeps downstream rules zero-false-positive (ADR-0023 tier-5).
pub fn type_of(ast: &LoweredAst, id: NodeId, env: &TypeEnv, interner: &mut Interner) -> TypeId {
    match ast.get(id) {
        Node::StringLit { value, .. } => {
            interner.intern(Type::Constant(Scalar::Str(value.clone())))
        }
        Node::IntegerLit { value, .. } => {
            interner.intern(Type::Constant(Scalar::Int(*value)))
        }
        Node::LocalVariableRead { name, .. } => env
            .get(name)
            .copied()
            .unwrap_or_else(|| interner.untyped()),
        // Literals/locals are all the receiver typing this slice needs; every
        // other carrier is unknown -> Dynamic[top] (never guess).
        _ => interner.untyped(),
    }
}

/// Walk the top-level statement sequence in source order, binding each
/// `LocalVariableWrite`'s name to the type of its value expression, and return
/// the resulting [`TypeEnv`].
///
/// This is the minimal flow needed so a later `s.lenght` can see `s :
/// Constant["Hello"]`. Nested scopes / reassignment narrowing are out of scope
/// for the tracer bullet.
// TODO(spec): real flow-sensitive scoping + narrowing across branches (ADR-0022).
pub fn build_toplevel_env(ast: &LoweredAst, interner: &mut Interner) -> TypeEnv {
    let mut env = TypeEnv::new();
    let body = match ast.get(ast.root()) {
        Node::Program { body, .. } => body.clone(),
        _ => return env,
    };
    for stmt in body {
        // A program body may wrap statements directly or via a Statements node.
        bind_statement(ast, stmt, &mut env, interner);
    }
    env
}

/// Bind a single statement into `env` if it is a local write; recurse through a
/// `Statements` wrapper. Other statements have no binding effect here.
fn bind_statement(ast: &LoweredAst, id: NodeId, env: &mut TypeEnv, interner: &mut Interner) {
    match ast.get(id) {
        Node::LocalVariableWrite { name, value, .. } => {
            let (name, value) = (name.clone(), *value);
            let ty = type_of(ast, value, env, interner);
            env.insert(name, ty);
        }
        Node::Statements { body, .. } => {
            for s in body.clone() {
                bind_statement(ast, s, env, interner);
            }
        }
        _ => {}
    }
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
}
