//! rigor-rs#398 — `branch_terminates?`'s Bot half for a Kernel
//! non-returning call reads the reference's dispatch, not block-call
//! timing's `project_defines_anywhere?` veto. Each row's expectation is the
//! oracle's (`harness/probe.py -f` on the
//! `begin 1 rescue q = 1; <call> end; Float(q).w` template: a terminating
//! arm is oracle-silent, a live one fires `w for Float`).

use super::*;
use rigor_parse::{lower, parse, Node};

/// Whether the FIRST rescue clause of the first `begin` in `files[at]`
/// terminates, with every file in one project index.
fn rescue_arm_exits(files: &[&str], at: usize) -> bool {
    let asts: Vec<LoweredAst> = files.iter().map(|f| lower(&parse(f.as_bytes()))).collect();
    let refs: Vec<&LoweredAst> = asts.iter().collect();
    let index = CoreIndex::new();
    let source = SourceIndex::build_project(&refs, &index);
    let typer = Typer::with_source(&index, &source);
    let ast = &asts[at];
    let body = ast
        .iter()
        .find_map(|(_, n)| match n {
            Node::BeginRescue { clauses, .. } if !clauses.is_empty() => {
                Some(clauses[0].body.clone())
            }
            _ => None,
        })
        .expect("a begin/rescue");
    typer.arm_exits(ast, &body, &mut Vec::new())
}

fn top(prelude: &str, call: &str) -> String {
    format!("{prelude}\nbegin\n  1\nrescue\n  q = 1\n  {call}\nend\nFloat(q).w\n")
}

fn in_def(prelude: &str, call: &str) -> String {
    format!(
        "{prelude}\nclass C\n  def m\n    begin\n      1\n    rescue\n      q = 1\n      {call}\n    \
         end\n    Float(q).w\n  end\nend\n"
    )
}

/// An explicit `Kernel.` / `::Kernel.` call reaches Kernel's own singleton
/// whatever the project defines — an unrelated class, a toplevel `def`, a
/// reopened `Kernel` (oracle-silent on every row).
#[test]
fn explicit_kernel_call_terminates_under_project_defs() {
    for (prelude, call) in [
        ("class C; def raise(*); end; end", "Kernel.raise"),
        ("class C; def raise(*); end; end", "::Kernel.raise"),
        ("def raise(*); end", "Kernel.raise"),
        ("module Kernel; def self.raise(*); end; end", "Kernel.raise"),
        ("module Kernel; def raise(*); end; end", "Kernel.raise"),
        ("class C; def exit(*); end; end", "Kernel.exit"),
        ("def exit!(*); end", "Kernel.exit!"),
    ] {
        assert!(rescue_arm_exits(&[&top(prelude, call)], 0), "{prelude} / {call}");
    }
    assert!(rescue_arm_exits(
        &[&in_def("class C; def raise(*); end; end", "Kernel.raise")],
        0
    ));
}

/// An implicit-self `exit!` (not in the syntactic `EXIT_CALL_NAMES`, so the
/// Bot half decides) stays Kernel's `bot` beside a `def` on an unrelated
/// class, a reopened `Object`, or the caller's own class — Kernel's RBS
/// outranks `C#exit!` (oracle `type-of`: `bot`, check silent).
#[test]
fn implicit_exit_bang_terminates_under_non_toplevel_defs() {
    assert!(rescue_arm_exits(&[&top("class C; def exit!(*); end; end", "exit!")], 0));
    assert!(rescue_arm_exits(&[&top("class Object; def exit!(*); end; end", "exit!")], 0));
    assert!(rescue_arm_exits(&[&top("Object.class_eval do\n  def exit!(*); end\nend", "exit!")], 0));
    assert!(rescue_arm_exits(&[&in_def("class C; def exit!(*); end; end", "exit!")], 0));
    assert!(rescue_arm_exits(&[&in_def("class D; def exit!(*); end; end", "exit!")], 0));
    // The caller's `self` answers `exit!` first, so the toplevel `def`
    // never binds there either.
    assert!(rescue_arm_exits(
        &[&in_def("def exit!(*); end\nclass C; def exit!(*); end; end", "exit!")],
        0
    ));
}

/// A project TOPLEVEL `def exit!` takes the implicit call off Kernel — same
/// file or cross-file, from a class body's method or a block — and its
/// non-exiting body keeps the arm live (oracle FIRES `w for Float`). A body
/// that itself raises keeps it terminating.
#[test]
fn toplevel_def_keeps_implicit_call_live() {
    assert!(!rescue_arm_exits(&[&top("def exit!(*); end", "exit!")], 0));
    assert!(!rescue_arm_exits(&[&in_def("def exit!(*); end", "exit!")], 0));
    assert!(!rescue_arm_exits(
        &[&top("", "exit!"), "def exit!(*); end\n"],
        0
    ));
    assert!(rescue_arm_exits(&[&top("def exit!(*); raise \"x\"; end", "exit!")], 0));
    // No project def at all: Kernel's `bot`.
    assert!(rescue_arm_exits(&[&top("", "exit!")], 0));
}

/// Block-call timing keeps its coarse veto: ANY project `def raise`
/// disables the `tap { raise }` never-completes proof, while termination
/// does not consult it.
#[test]
fn block_timing_keeps_project_defines_veto() {
    let src = "class C; def raise(*); end; end\nx = 1\nx.tap { Kernel.raise \"y\" }\n";
    let ast = lower(&parse(src.as_bytes()));
    let index = CoreIndex::new();
    let source = SourceIndex::build_project(&[&ast], &index);
    let typer = Typer::with_source(&index, &source);
    let call = ast
        .iter()
        .find_map(|(id, n)| {
            matches!(n, Node::Call { method, receiver: Some(_), .. } if method == "raise")
                .then_some(id)
        })
        .expect("Kernel.raise");
    assert!(!typer.call_declares_bot(&ast, call));
    assert!(typer.kernel_call_terminates(&ast, call));
}
