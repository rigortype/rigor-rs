//! Diagnostic rules + the structured `Diagnostic` type (ADR-0014: rule id,
//! severity, primary/secondary annotations, subdiagnostics). All rules run in a
//! single converged AST walk (ADR-0005), not one pass per rule. The tracer
//! bullet's first rule is `call.undefined-method`.
#![allow(dead_code)]

mod shadowed_rescue;
pub use shadowed_rescue::shadowed_rescue_diagnostics;

pub mod dead_version_guard;
mod call_toplevel;
mod void_value_use;
mod suppression_markers;
mod call_receiver;
mod call_arguments;
mod flow;
mod def;
mod call_raise;
mod suppression;
mod scope;
mod driver;
mod rule_catalog;
mod diagnostic;
pub use dead_version_guard::{
    filter_dead_version_guard_arms, filter_dead_version_guard_arms_with, RubyRuntime,
};
pub(crate) use call_toplevel::*;
pub use void_value_use::void_value_use_diagnostics;
pub use suppression_markers::suppression_marker_diagnostics;
pub(crate) use call_receiver::*;
pub(crate) use call_arguments::*;
pub(crate) use flow::*;
pub(crate) use def::*;
pub(crate) use call_raise::*;
pub use suppression::{
    filter_suppressed, implemented_rules, is_inert_builtin_token, known_suppression_token,
    SuppressSet,
};
pub(crate) use suppression::*;
pub(crate) use scope::*;
pub use driver::{analyze, analyze_with_source, analyze_with_source_and_folder};
pub use rule_catalog::{
    catalog, RuleEntry, CALL_ARGUMENT_TYPE_MISMATCH, CALL_POSSIBLE_NIL_RECEIVER,
    CALL_RAISE_NON_EXCEPTION, CALL_UNDEFINED_METHOD, CALL_UNRESOLVED_TOPLEVEL, CALL_WRONG_ARITY,
    DEF_IVAR_WRITE_MISMATCH, DEF_OVERRIDE_VISIBILITY_REDUCED, FLOW_ALWAYS_RAISES,
    FLOW_ALWAYS_TRUTHY_CONDITION, FLOW_DEAD_ASSIGNMENT, FLOW_DUPLICATE_HASH_KEY,
    FLOW_RETURN_IN_ENSURE, FLOW_SHADOWED_RESCUE_CLAUSE, FLOW_UNREACHABLE_BRANCH,
    STATIC_VALUE_USE_VOID, SUPPRESSION_EMPTY, SUPPRESSION_UNKNOWN_MARKER, SUPPRESSION_UNKNOWN_RULE,
};
pub use diagnostic::{Diagnostic, Severity, NO_RULE};
pub(crate) use diagnostic::*;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;

#[cfg(test)]
mod void_value_use_tests;

#[cfg(test)]
mod rbs_tuple_witness_tests;
