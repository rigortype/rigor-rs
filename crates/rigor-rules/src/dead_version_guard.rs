//! The span filter twin of ADR-47 WD5 / upstream #627 (`d20d6f90`) — the
//! **dead arm of a decidable Ruby version guard reports nothing**.
//!
//! ```ruby
//! if RUBY_VERSION < "2.7."
//!   append_cflags("-std=gnu99")     # never runs on the Ruby being checked with
//! end
//! ```
//!
//! The dead arm is honest code for an older Ruby, so a diagnostic inside it is a
//! false positive. This module ports the reference's
//! `lib/rigor/analysis/check_rules/dead_version_guard_arms.rb` —
//! [`filter_dead_version_guard_arms`], a SPAN FILTER dropping every diagnostic
//! that lands inside a dead arm. The VERDICT half (the port of
//! `lib/rigor/inference/version_guard.rb`) lives one crate down in
//! `rigor_infer::version_guard`, where the flow folds that consume it sit
//! (`guard_truthiness` — the reference consults the guard from
//! `StatementEvaluator#branch_certainty`, so the dead arm's writes never join
//! the post-`if` scope either); everything public is re-exported here so the
//! pre-split paths (`rigor_rules::dead_version_guard::*`,
//! [`RubyRuntime`]) keep resolving.
//!
//! `RIGOR_RUBY_VERSION` / `RIGOR_RUBY_ENGINE` and the folded spellings are
//! documented on the verdict module (`rigor_infer::version_guard`); a
//! curated-but-unreadable `Psych::VERSION` guard kills BOTH arms here —
//! whichever arm the reference killed, this filter has already dropped it.
//!
//! The reference ALSO elides the dead arm's writes from the post-`if` scope
//! (`StatementEvaluator#eval_if`) — ported as `guard_truthiness` in the
//! `rigor-infer` flow folds (rigor-rs#909).

use crate::Diagnostic;
use rigor_infer::version_guard::{self, Verdict};
pub use rigor_infer::version_guard::{
    process_runtime, verdict, GemVersion, RubyRuntime, HOST_RUBY_ENGINE, HOST_RUBY_VERSION,
};
use rigor_parse::{LoweredAst, Node, NodeId, Span};

// ---------------------------------------------------------------------------
// The filter (reference `DeadVersionGuardArms`)
// ---------------------------------------------------------------------------

/// Drop every diagnostic that lands inside the dead arm of a decidable version
/// guard, against the process's reference Ruby ([`version_guard::process_runtime`]).
///
/// MUST run BEFORE `suppression_marker_diagnostics` joins the list: a malformed
/// `# rigor:disable` marker inside a dead arm is still a real authoring error, so
/// `suppression.*` is never dropped (the reference gets that from ORDER alone;
/// the explicit rule-id guard here makes the contract independent of the call
/// site's ordering).
pub fn filter_dead_version_guard_arms(
    diagnostics: Vec<Diagnostic>,
    ast: &LoweredAst,
) -> Vec<Diagnostic> {
    filter_dead_version_guard_arms_with(diagnostics, ast, process_runtime())
}

/// [`filter_dead_version_guard_arms`] against an explicit reference Ruby.
pub fn filter_dead_version_guard_arms_with(
    diagnostics: Vec<Diagnostic>,
    ast: &LoweredAst,
    runtime: &RubyRuntime,
) -> Vec<Diagnostic> {
    // The scan is a whole-file walk, so it is paid only when there is something
    // to drop (reference `filter`'s two early returns).
    if diagnostics.is_empty() {
        return diagnostics;
    }
    let arms = dead_arm_spans(ast, runtime);
    if arms.is_empty() {
        return diagnostics;
    }
    diagnostics
        .into_iter()
        .filter(|d| {
            d.rule_id.starts_with("suppression.")
                || !arms.iter().any(|arm| covers(*arm, d.start_offset))
        })
        .collect()
}

/// The source ranges of every dead version-guard arm in the file (reference
/// `DeadVersionGuardArms.scan`).
///
/// The reference's walk does not DESCEND into a dead arm; this one visits every
/// node, which is equivalent for the filter — a guard nested inside a dead arm
/// can only contribute ranges the dead arm already covers.
pub fn dead_arm_spans(ast: &LoweredAst, runtime: &RubyRuntime) -> Vec<Span> {
    let mut arms = Vec::new();
    for (_, node) in ast.iter() {
        let Node::If { predicate, then_body, else_body, is_unless, .. } = node else {
            continue;
        };
        let Some(verdict) = version_guard::verdict(ast, *predicate, runtime) else {
            continue;
        };
        // `unless` runs its body on the FALSEY edge, so the arms are swapped.
        // `else_body` holds AT MOST one lowered node — the `elsif` chain's
        // `IfNode` or the `else` clause's carrier — so its hull IS the
        // reference's `node.subsequent` / `node.else_clause` location.
        let (body_dead, subsequent_dead) = match (verdict, *is_unless) {
            (Verdict::Falsey, false) | (Verdict::Truthy, true) => (true, false),
            (Verdict::Truthy, false) | (Verdict::Falsey, true) => (false, true),
            (Verdict::Unreadable, _) => (true, true),
        };
        if body_dead {
            arms.extend(span_hull(ast, then_body));
        }
        if subsequent_dead {
            arms.extend(span_hull(ast, else_body));
        }
    }
    arms
}

/// The smallest span covering every node in `ids` (the reference's
/// `StatementsNode#location`), or `None` for an absent arm — `foo if
/// RUBY_VERSION >= "3.1"` has no `else`, and `if COND; end` has no body.
fn span_hull(ast: &LoweredAst, ids: &[NodeId]) -> Option<Span> {
    let mut it = ids.iter().map(|&id| ast.get(id).span());
    let first = it.next()?;
    Some(it.fold(first, |acc, s| (acc.0.min(s.0), acc.1.max(s.1))))
}

/// Whether a diagnostic anchored at `offset` falls inside `arm` (end EXCLUSIVE,
/// exactly like the reference's `covers?` — so a one-line guard
/// `RUBY_VERSION >= "3.1" ? a(1) : a(1, 2)` drops only the dead half).
fn covers(arm: Span, offset: usize) -> bool {
    arm.0 <= offset && offset < arm.1
}

#[cfg(test)]
mod tests {
    use super::*;
    use rigor_parse::{lower, parse};

    fn ast_of(src: &str) -> (LoweredAst, String) {
        let bytes = src.as_bytes().to_vec();
        let result = parse(&bytes);
        (lower(&result), src.to_string())
    }

    fn host() -> RubyRuntime {
        RubyRuntime::default()
    }

    /// The one-line ternary drops only the dead HALF — the reason `covers` is an
    /// offset range and not a line number.
    #[test]
    fn a_ternary_drops_only_the_dead_half() {
        let src = "RUBY_VERSION < \"3.4\" ? a : b\n";
        let (ast, _source) = ast_of(src);
        let arms = dead_arm_spans(&ast, &host());
        assert_eq!(arms.len(), 1);
        let (start, end) = arms[0];
        assert_eq!(&src[start..end], "a");
    }

    #[test]
    fn an_elsif_chain_decides_each_link_on_its_own() {
        // f14: `>= "3.4"` is TRUE on the host, so the whole `elsif` chain is dead.
        let src = "if RUBY_VERSION >= \"3.4\"\n  a\nelsif RUBY_VERSION >= \"3.0\"\n  b\nend\n";
        let (ast, _source) = ast_of(src);
        let arms = dead_arm_spans(&ast, &host());
        assert_eq!(arms.len(), 1);
        let (start, end) = arms[0];
        assert!(src[start..end].starts_with("elsif"), "{:?}", &src[start..end]);
        assert!(src[start..end].contains("b"));
    }

    #[test]
    fn suppression_diagnostics_survive_a_dead_arm() {
        let src = "if RUBY_VERSION < \"3.4\"\n  a.b\nend\n";
        let (ast, _source) = ast_of(src);
        let inside = src.find("a.b").expect("offset");
        let diag = |rule: &'static str| Diagnostic {
            rule_id: rule,
            start_offset: inside,
            end_offset: inside + 3,
            message: String::new(),
            severity: crate::Severity::Warning,
            source_family: "builtin",
            receiver_type: None,
            receiver_classes: None,

            method_name: None,
        };
        let kept = filter_dead_version_guard_arms_with(
            vec![diag("call.undefined-method"), diag("suppression.unknown-rule")],
            &ast,
            &host(),
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].rule_id, "suppression.unknown-rule");
    }
}
