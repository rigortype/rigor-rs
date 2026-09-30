//! Owned, `NodeId`-indexed AST + the Prism lowering pass (ADR-0012).
//!
//! `ruby-prism` nodes borrow the parse buffer (a lifetime on every node).
//! Threading that lifetime through the inference engine reproduces the
//! pervasive-`'a` pain ADR-0005/0006 deliberately avoid, so [`lower`] walks the
//! borrowed Prism tree exactly once and produces *owned* nodes keyed by a dense
//! [`NodeId`]. Inference and rules then walk this owned arena, never the
//! borrowed Prism tree.
//!
//! The owned node *shape* mirrors Prism's (rather than normalizing to a
//! semantically different HIR) so node-level behaviour stays aligned with the
//! reference for diagnostic-set parity (ADR-0002/0012). The long tail of Prism
//! nodes still lowers to [`Node::Other`] so an unhandled construct never aborts
//! the walk.
//!
//! ## Recursive structural lowering
//!
//! Beyond the top-level literal/call subset, [`lower`] recurses into the bodies
//! of definitions (`def`/`class`/`module`/singleton class), control flow
//! (`if`/`unless`/ternary, `case`/`when`/`in`, `while`/`until`/`for`,
//! `begin`/`rescue`/`ensure`, `&&`/`||`), blocks (`foo { ... }`), and into the
//! receivers/values of variable, constant, array, hash, index, range and
//! string-interpolation nodes. The point is reachability: EVERY nested call
//! lands in the arena as a [`Node::Call`], so the single rule walk
//! (`ast.iter()` filtering `Node::Call`) analyses calls inside a method/branch
//! body, not just top-level ones. Structural variants carry child [`NodeId`]s so
//! the typer can recurse into a receiver/argument; constructs we don't type
//! precisely still get their children lowered (and so analysed).

mod block_params;
mod hash_keys;
mod multi_target;
mod definitions;
mod recovery;
mod constants;
mod lowered_ast;
mod node;
mod builder;

use crate::ruby_prism;
pub use block_params::BlockParamKind;
pub(crate) use block_params::*;
pub use hash_keys::{HashKey, HashKeyTag};
pub use multi_target::{IndexWrites, MultiTarget, MultiTargets};
pub(crate) use multi_target::*;
pub use definitions::{MethodBody, ParamShape, Visibility};
pub(crate) use definitions::*;
pub(crate) use recovery::*;
pub use constants::ConstMutation;
pub(crate) use constants::*;
pub use lowered_ast::{lower, lower_with_key, FileKey, LoweredAst};
pub use node::{IndexCompound, JumpKind, Node, RescueClause, StatementsKind};
pub(crate) use builder::*;

/// A dense handle into [`LoweredAst::nodes`]. Cheap to copy; stable for the
/// lifetime of the owned AST (ADR-0012: owned, `NodeId`-keyed nodes).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct NodeId(pub u32);

/// A source span as a half-open byte-offset range `[start, end)`.
///
/// Byte offsets (not line/col) are the load-bearing location per ADR-0030; the
/// CLI computes line/col lazily from the source when presenting (ADR-0030:
/// Prism columns are 0-based, the presenter adds 1).
pub type Span = (usize, usize);

/// Decode a Prism `ConstantId` byte slice (a method / variable name) to an
/// owned `String`. Names are UTF-8 in practice; lossy decode keeps the walk
/// total on exotic encodings.
fn constant_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Convert a Prism `Location` to a byte-offset [`Span`].
fn span_of(loc: &ruby_prism::Location<'_>) -> Span {
    (loc.start_offset(), loc.end_offset())
}

#[cfg(test)]
mod tests;
