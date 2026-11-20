//! rigor-rs#439: the `RIGOR_REACH_MEMO_AUDIT` gate's regression pins.
//!
//! Output diffs cannot see the memo's two validity rules (a `seen`
//! membership answer that no longer holds, a re-entered fold that has since
//! landed) — only the audit's recompute-and-compare catches a violation.
//! These tests run small shapes with the audit FORCED on; each was shown to
//! panic on a build with the corresponding rule broken (recorded in the PR),
//! so a later change that breaks the rule again fails here.
//!
//! The flag is process-global ([`crate::reach_memo::set_audit_mode_for_test`]):
//! parallel tests are audited too, which is sound — just slower.

use super::*;
use rigor_parse::{lower, parse, Node};

fn lower_src(src: &[u8]) -> LoweredAst {
    lower(&parse(src))
}

/// Drive the paths a `check` takes through the memo: the `dead_positions`
/// scan (which folds predicates and populates every table), then a second
/// pass of folds and reach queries so stored entries are HIT — under the
/// audit every hit recomputes and compares.
fn drive(typer: &Typer, ast: &LoweredAst) -> Vec<String> {
    let mut log = Vec::new();
    let _ = typer.dead_positions(ast, &mut Vec::new());
    for (id, n) in ast.iter() {
        match n {
            Node::If { predicate, .. } | Node::Loop { predicate: Some(predicate), .. } => {
                let a = typer.expr_truthiness(ast, *predicate, &mut Vec::new());
                let b = typer.expr_truthy_memo(ast, *predicate, &mut Vec::new());
                log.push(format!("truthy {id:?} -> {a:?} {b:?}"));
            }
            Node::LocalVariableRead { name, span } => {
                let mut seen = Vec::new();
                let scalar = typer.expr_scalar(ast, id, &mut seen);
                let reach = typer.local_reach(ast, name, *span, &mut Vec::new(), false);
                log.push(format!("reach {name}@{span:?} -> {scalar:?} {reach:?}"));
            }
            _ => {}
        }
    }
    log
}

/// The shape the #433 review used to prove the re-entry rule load-bearing
/// (rigor-rs#439's g01): a `local_reach` computation that runs while the
/// `y = 2 if q` predicate is still folding sees it answer `None`; its entry
/// is reusable only while that is still true.
#[test]
fn audit_is_silent_on_the_reentry_shape() {
    let src = b"def f(c)\n  q = 1\n  x = 1\n  while c\n    x = 1 if q\n    q = x\n    y = 2 if q\n  end\nend\n";
    let ast = lower_src(src);
    let idx = CoreIndex::new();
    // Control run first (audit off), then the audited run: identical output.
    let control = drive(&Typer::new(&idx), &ast);
    crate::reach_memo::set_audit_mode_for_test(true);
    let typer = Typer::new(&idx);
    let audited = drive(&typer, &ast);
    assert_eq!(audited, control, "audit changed the answers");
    // The shape must actually exercise the rule: the scan stores entries
    // whose answer depends on a fold they saw re-entered.
    let (with_folding, _) = typer.reach_memo.borrow().folding_entry_counts();
    assert!(with_folding > 0, "no re-entry-dependent entries were stored");
}

/// A `seen`-membership violation: two reads' chains reach the same
/// expression fold under same-length stacks whose membership answers
/// differ. The entry recorded under one stack must not answer for the
/// other. Minimized from the #439 fuzz.
#[test]
fn audit_is_silent_on_the_membership_shape() {
    let src = b"def f(c)\n  y = 1\n  while c\n    q = y if y\n    y = q if q\n  end\nend\n";
    let ast = lower_src(src);
    let idx = CoreIndex::new();
    let control = drive(&Typer::new(&idx), &ast);
    crate::reach_memo::set_audit_mode_for_test(true);
    let typer = Typer::new(&idx);
    let audited = drive(&typer, &ast);
    assert_eq!(audited, control, "audit changed the answers");
}

/// rigor-rs#439 item 4: an audited hit's recompute must not store — a
/// duplicate at the front of the per-key LRU halves the effective cap (and
/// could shadow the very entry being audited).
#[test]
fn audited_hit_does_not_store_a_duplicate() {
    crate::reach_memo::set_audit_mode_for_test(true);
    let src = b"def f(c)\n  q = 1\n  x = 1\n  while c\n    x = 1 if q\n    q = x\n    y = 2 if q\n  end\nend\n";
    let ast = lower_src(src);
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let _ = typer.dead_positions(&ast, &mut Vec::new());
    // g01 leaves an entry for local_reach(x, (58,59)) under a len-2 stack;
    // an audited hit on it must leave the list at one entry.
    assert_eq!(
        1,
        typer.reach_memo.borrow().reach_entry_count((58, 59), false, 2),
        "setup: expected one stored entry for the x@58 read"
    );
    let mut seen = vec!["q@73".to_string(), "x@58".to_string()];
    let _ = typer.local_reach(&ast, "x", (58, 59), &mut seen, false);
    assert_eq!(
        1,
        typer.reach_memo.borrow().reach_entry_count((58, 59), false, 2),
        "an audited hit stored a duplicate entry"
    );
}
