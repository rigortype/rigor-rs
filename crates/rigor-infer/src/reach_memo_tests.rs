//! rigor-rs#439: the `RIGOR_REACH_MEMO_AUDIT` gate's regression pins.
//!
//! Output diffs cannot see the memo's two validity rules (a `seen`
//! membership answer that no longer holds, a re-entered fold that has since
//! landed) — only the audit's recompute-and-compare catches a violation.
//! These tests run small shapes with the audit FORCED on; each was shown to
//! panic on a build with the corresponding rule broken (recorded in the PR),
//! so a later change that breaks the rule again fails here.
//!
//! The flag is thread-local ([`crate::reach_memo::set_audit_mode_for_test`])
//! so forcing it on here cannot audit — or re-time — tests running in
//! parallel.

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

/// The re-entry rule itself: an entry that recorded a still-folding dep
/// must stop answering once that fold lands on `Some`. On a build with the
/// `folding` guard removed this fails — the stale entry survives the hit
/// and its `folding` count never drops. Deterministic: it doesn't depend
/// on the stale answer actually diverging.
#[test]
fn folding_dep_entry_is_dropped_once_its_pred_lands() {
    crate::reach_memo::set_audit_mode_for_test(true);
    let src = b"def f(c)\n  q = 1\n  while c\n    x = 2 if q\n    q = x\n    y = 3 if q\n  end\nend\n";
    let ast = lower_src(src);
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let _ = typer.dead_positions(&ast, &mut Vec::new());
    // The scan's entries live under the scan's dead-set Rc — the first
    // post-scan reach resets the memo. Flush that reset, then repopulate:
    // post-scan folds still see re-entries and store `folding` deps under
    // the settled (None) dead context.
    let mut warmup = Vec::new();
    let _ = typer.local_reach(&ast, "q", (34, 35), &mut warmup, false);
    drive(&typer, &ast);
    let (root, span, skip, len, present, dep) = typer
        .reach_memo
        .borrow()
        .first_folding_entry()
        .expect("the shape must store at least one re-entry-dependent entry");
    // The dep's fold lands on Some — the recorded answer's validity window
    // is over.
    typer.truthy_landed.borrow_mut().insert(dep, Some(true));
    // Rebuild a stack that reproduces the entry's recorded membership
    // answers: its `present` keys verbatim, padded with keys no real
    // computation ever interned (they can't collide with `tested` ids).
    let mut seen = present;
    while seen.len() < len {
        seen.push(format!("zz-filler-{}", seen.len()));
    }
    let before = typer.reach_memo.borrow().folding_entry_counts().0;
    let _ = typer.local_reach(&ast, &root, span, &mut seen, skip);
    let after = typer.reach_memo.borrow().folding_entry_counts().0;
    assert!(
        after < before,
        "a hit on an entry whose fold dep has landed must drop it"
    );
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
    // an audited hit on it must leave the list unchanged.
    let before = typer.reach_memo.borrow().reach_entry_count((58, 59), false, 2);
    assert!(before >= 1, "setup: no entry stored for the x@58 read");
    let mut seen = vec!["q@73".to_string(), "x@58".to_string()];
    let _ = typer.local_reach(&ast, "x", (58, 59), &mut seen, false);
    assert_eq!(
        before,
        typer.reach_memo.borrow().reach_entry_count((58, 59), false, 2),
        "an audited hit stored a duplicate entry"
    );
}
