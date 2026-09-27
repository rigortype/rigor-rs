//! Diagnostic rules + the structured `Diagnostic` type (ADR-0014: rule id,
//! severity, primary/secondary annotations, subdiagnostics). All rules run in a
//! single converged AST walk (ADR-0005), not one pass per rule. The tracer
//! bullet's first rule is `call.undefined-method`.
//!
//! ## Module map
//!
//! One module per rule family, plus the shared infrastructure; `lib.rs` holds
//! only declarations and re-exports. A new rule usually touches its family's
//! module, `rule_catalog` (its id and entry) and `driver` (its pass).
//!
//! | module | holds |
//! |---|---|
//! | `driver` | [`analyze`] and friends: the single walk that runs every pass |
//! | `rule_catalog` | every rule id and [`catalog`] entry |
//! | `diagnostic` | [`Diagnostic`], [`Severity`], [`NO_RULE`], receiver rendering |
//! | `scope` | `ScopedEnv`, span containment, lexical class qualification |
//! | `suppression` | `# rigor:disable` / `disable:` and the rule-token tables |
//! | `suppression_markers` | the `suppression.*` rules |
//! | `call_receiver` | `call.undefined-method`, `call.possible-nil-receiver` |
//! | `call_arguments` | `call.wrong-arity`, `call.argument-type-mismatch` |
//! | `call_raise` | `call.raise-non-exception` |
//! | `call_toplevel` | `call.unresolved-toplevel` |
//! | `flow` | the `flow.*` rules except `flow.shadowed-rescue-clause` |
//! | `shadowed_rescue` | `flow.shadowed-rescue-clause` |
//! | `def` | the `def.*` rules |
//! | `void_value_use` | `static.value-use.void` |
//! | `dead_version_guard` | the dead version-guard arm filter (public) |
//!
//! Unit tests live in the `*tests.rs` modules beside them (`use super::*`).
#![allow(dead_code)]

pub mod dead_version_guard;

mod call_arguments;
mod call_raise;
mod call_receiver;
mod call_toplevel;
mod def;
mod diagnostic;
mod driver;
mod flow;
mod rule_catalog;
mod scope;
mod shadowed_rescue;
mod suppression;
mod suppression_markers;
mod void_value_use;

pub use dead_version_guard::{
    filter_dead_version_guard_arms, filter_dead_version_guard_arms_with, RubyRuntime,
};
pub use diagnostic::{Diagnostic, Severity, NO_RULE};
pub use driver::{analyze, analyze_with_source, analyze_with_source_and_folder};
pub use rule_catalog::{
    catalog, RuleEntry, CALL_ARGUMENT_TYPE_MISMATCH, CALL_POSSIBLE_NIL_RECEIVER,
    CALL_RAISE_NON_EXCEPTION, CALL_UNDEFINED_METHOD, CALL_UNRESOLVED_TOPLEVEL, CALL_WRONG_ARITY,
    DEF_IVAR_WRITE_MISMATCH, DEF_OVERRIDE_VISIBILITY_REDUCED, FLOW_ALWAYS_RAISES,
    FLOW_ALWAYS_TRUTHY_CONDITION, FLOW_DEAD_ASSIGNMENT, FLOW_DUPLICATE_HASH_KEY,
    FLOW_RETURN_IN_ENSURE, FLOW_SHADOWED_RESCUE_CLAUSE, FLOW_UNREACHABLE_BRANCH,
    STATIC_VALUE_USE_VOID, SUPPRESSION_EMPTY, SUPPRESSION_UNKNOWN_MARKER, SUPPRESSION_UNKNOWN_RULE,
};
pub use shadowed_rescue::shadowed_rescue_diagnostics;
pub use suppression::{
    filter_suppressed, implemented_rules, is_inert_builtin_token, known_suppression_token,
    SuppressSet,
};
pub use suppression_markers::suppression_marker_diagnostics;
pub use void_value_use::void_value_use_diagnostics;

// Crate-internal names the sibling modules reach as `crate::NAME` (the rule
// passes the driver calls, the shared helpers), and the test modules through
// `use super::*`.
pub(crate) use call_arguments::*;
pub(crate) use call_raise::*;
pub(crate) use call_receiver::*;
pub(crate) use call_toplevel::*;
pub(crate) use def::*;
pub(crate) use diagnostic::*;
pub(crate) use flow::*;
pub(crate) use scope::*;
pub(crate) use suppression::*;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;

#[cfg(test)]
mod void_value_use_tests;

#[cfg(test)]
mod rbs_tuple_witness_tests;
