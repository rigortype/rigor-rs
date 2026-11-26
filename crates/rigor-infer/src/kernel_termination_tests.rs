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

/// rigor-rs#966 — a receiverless `raise` is SYNTACTIC only at the arm's
/// own tail (`branch_unconditionally_exits?`); in a value position (a
/// write, a call operand, a nested `if`'s arms) only its type speaks, and a
/// project toplevel `def raise; end` types it `nil`. Oracle rows on the
/// `x = "a"; … rescue; x = "e"; <tail>; end; x.w` template: a live arm
/// answers `for "a" | "e"`, a terminating one `for "a"`.
#[test]
fn toplevel_raise_def_vetoes_value_positions_only() {
    let user = "def raise(*); end";
    // The arm's own tail stays syntactic: still an exit.
    assert!(rescue_arm_exits(&[&top(user, "raise")], 0));
    assert!(rescue_arm_exits(&[&top(user, "(raise)")], 0));
    // Value positions read the user def's `nil`.
    for tail in [
        "@q = raise",
        "y = raise",
        "foo(raise)",
        "if rand > 0; raise; else; raise; end",
        "begin; raise; ensure; nil; end",
    ] {
        assert!(!rescue_arm_exits(&[&top(user, tail)], 0), "{tail}");
        // Without the user def every one of them exits.
        assert!(rescue_arm_exits(&[&top("", tail)], 0), "{tail} (no def)");
    }
    // A def on an unrelated class does not bind the toplevel call.
    assert!(rescue_arm_exits(&[&top("class C; def raise(*); end; end", "@q = raise")], 0));
    // A user def whose own body is `bot` keeps the call `bot`.
    assert!(rescue_arm_exits(&[&top("def raise(*); Kernel.raise; end", "@q = raise")], 0));
}

/// rigor-rs#966 — `def_exits` binds the toplevel def only where the call's
/// arity fits its parameters (`bind_params_from_call_types`); a mismatch
/// answers `Dynamic[Top]` on the oracle, so the arm stays live.
#[test]
fn def_exits_requires_a_fitting_arity() {
    let cases: &[(&str, &str, bool)] = &[
        ("def die; raise; end", "@q = die", true),
        ("def die; raise; end", "@q = die(1)", false),
        ("def die; raise; end", "die(1)", false),
        ("def die(a); raise; end", "@q = die(1)", true),
        ("def die(a); raise; end", "@q = die", false),
        ("def die(a, b = 1); raise; end", "@q = die(1)", true),
        ("def die(a, b = 1); raise; end", "@q = die(1, 2)", true),
        ("def die(a, b = 1); raise; end", "@q = die(1, 2, 3)", false),
        ("def die(*a); raise; end", "@q = die(1, 2, 3)", true),
        ("def die(*a, z); raise; end", "@q = die(1, 2, 3)", false),
        ("def die(k: 1); raise; end", "@q = die", true),
        ("def die(k:); raise; end", "@q = die", true),
        ("def die(&b); raise; end", "@q = die", true),
        ("def die; raise; end", "@q = die(*[])", false),
        // rigor-rs#979 — a trailing literal that never types a `HashShape`
        // is never popped as the keyword hash.
        ("def die(a, k: 1); raise; end", "@q = die(1)", true),
        ("def die(a, k: 1); raise; end", "@q = die(nil)", true),
        ("def die(a, k: 1); raise; end", "@q = die(1, nil)", false),
        // An untyped trailing argument may be one: both counts must fit
        // (the oracle binds `die(q)` with `q = 1`; the port declines).
        ("def die(a, k: 1); raise; end", "@q = die(q)", false),
        ("def die(a, k: 1); raise; end", "@q = die({k: 1})", false),
        // A static-keyed call-site keyword hash is the keyword argument of
        // a keyword-taking def, and a positional of any other.
        ("def die(a, k:); raise; end", "@q = die(1, k: 1)", true),
        ("def die(a, k: 1); raise; end", "@q = die(1, \"a\" => 1)", true),
        ("def die(a, k: 1); raise; end", "@q = die(k: 1)", false),
        ("def die(a, b); raise; end", "@q = die(1, a: 1)", true),
        ("def die(a); raise; end", "@q = die(k: 1)", true),
        // A dynamic key or a `**` declines (the oracle binds `die(1, **{})`:
        // it drops a literal empty double splat, which the port does not model).
        ("def die(a, k: 1); raise; end", "@q = die(1, q => 1)", false),
        ("def die(a, k: 1); raise; end", "@q = die(1, **{})", false),
        // `(...)` is a positional rest that takes keywords.
        ("def die(...); raise; end", "@q = die(1)", true),
        ("def die(a, ...); raise; end", "@q = die(1, 2)", true),
        ("def die(a, b, ...); raise; end", "@q = die(1)", false),
    ];
    for &(prelude, tail, exits) in cases {
        assert_eq!(rescue_arm_exits(&[&top(prelude, tail)], 0), exits, "{prelude} / {tail}");
    }
}

/// The first `if` in `src`, judged by the class-narrowing exit tests.
fn if_arms(src: &str) -> (bool, bool, bool) {
    let ast = lower(&parse(src.as_bytes()));
    let index = CoreIndex::new();
    let source = SourceIndex::build_project(&[&ast], &index);
    let typer = Typer::with_source(&index, &source);
    let (then_body, else_body) = ast
        .iter()
        .find_map(|(_, n)| match n {
            Node::If { then_body, else_body, .. } => Some((then_body.clone(), else_body.clone())),
            _ => None,
        })
        .expect("an if");
    (
        crate::class_narrowing::branch_terminates(&typer, &ast, &then_body),
        crate::class_narrowing::value_branch_terminates(&typer, &ast, &else_body),
        crate::class_narrowing::branch_certainly_exits(&typer, &ast, &then_body),
    )
}

/// rigor-rs#966 — the `if` frame: the body arm keeps the syntactic test,
/// the `else` arm is typed, and `branch_certainly_exits?` (then beside
/// else) vetoes on ANY project def of the name (`project_method_name?`).
#[test]
fn if_frame_exit_tests_follow_the_oracle() {
    // No project def: every test sees the exit.
    assert_eq!(if_arms("if rand > 0\n  raise\nelse\n  raise\nend\n"), (true, true, true));
    // A toplevel `def raise`: the body arm still exits syntactically; the
    // else arm types `nil`; the strict then-beside-else test is vetoed.
    assert_eq!(
        if_arms("def raise(*); end\nif rand > 0\n  raise\nelse\n  raise\nend\n"),
        (true, false, false)
    );
    // A def on a class: the else arm's implicit call is still Kernel's
    // `bot`, but `project_method_name?` vetoes the strict test.
    assert_eq!(
        if_arms("class C; def raise(*); end; end\nif rand > 0\n  raise\nelse\n  raise\nend\n"),
        (true, true, false)
    );
    // `exit!` is never syntactic.
    assert_eq!(
        if_arms("def exit!(*); end\nif rand > 0\n  exit!\nelse\n  exit!\nend\n"),
        (false, false, false)
    );
}

/// rigor-rs#1011 — `branch_terminates?`'s Bot half for the call shapes the
/// non-returning name table does not reach. `def_exits` is name-agnostic
/// (`def die; raise; end` answers for `die`), `exec`/`sleep`/`loop` ride
/// their own `bot` overloads, and a resolvable constant receiver's
/// all-`bot` member covers `Kernel.exit`, `::Kernel.exit`, `Process.exit`
/// (and `String.exit` — every class object inherits `Kernel#exit`). Every
/// expectation below is the oracle's answer on the `rescue q = 1; <call>`
/// arm template.
#[test]
fn arbitrary_bot_call_terminates() {
    // A receiverless name a toplevel `def` answers with a `bot` body.
    assert!(rescue_arm_exits(&[&top("def die; raise; end", "die")], 0));
    assert!(rescue_arm_exits(&[&in_def("def die; raise; end", "die")], 0));
    // A non-exiting body — or `self` answering the name first — keeps it
    // live.
    assert!(!rescue_arm_exits(&[&top("def die; nil; end", "die")], 0));
    assert!(!rescue_arm_exits(
        &[&in_def("def die; raise; end\nclass C; def die; end; end", "die")],
        0
    ));
    // The explicit-receiver Kernel exits — `Kernel.` and `::Kernel.` reach
    // the member directly, and every class object inherits `Kernel#exit`
    // through the class object's own ancestry (`Process`, `String`,
    // `Object`).
    for call in ["Kernel.exit", "::Kernel.exit", "Kernel.raise", "Process.exit", "String.exit", "Object.exit"] {
        assert!(rescue_arm_exits(&[&top("", call)], 0), "{call}");
    }
    // `Thread.exit` — Thread's own `def self.exit: untyped` shadows
    // `Kernel#exit` on the surface.
    assert!(!rescue_arm_exits(&[&top("", "Thread.exit")], 0));
    // The decline side: an unknown constant, a project class (not on the
    // RBS surface), a module's own `def self.exit`, a member nobody
    // declares, and a receiver the walk cannot resolve to a constant.
    for (prelude, call) in [
        ("", "UndefinedConst.exit"),
        ("class Foo; end", "Foo.exit"),
        ("module Bar; def self.exit; nil; end; end", "Bar.exit"),
        ("def die; raise; end", "String.die"),
        ("", "self.exit"),
        ("", "1.exit"),
        ("", "obj.exit"),
        ("", "String.loop"),
        ("", "catch"),
    ] {
        assert!(!rescue_arm_exits(&[&top(prelude, call)], 0), "{prelude} / {call}");
    }
}

/// rigor-rs#1011 — `Kernel#exec`, `Kernel#sleep` and `Kernel#loop` carry
/// `bot` signatures that are not in the non-returning name table. `exec`
/// is `bot` on every overload; `sleep` only off its arity-1 `(_Timeout)
/// -> Integer` overload (`sleep(1)` types `Integer` — a `bot | Integer`
/// disagreement that must not mark the member all-`bot`); `loop` is `bot`
/// only while its literal block cannot end the call.
#[test]
fn kernel_signature_bot_calls() {
    for call in ["exec", "Kernel.exec", "Process.exec", "sleep", "sleep(1, 2)", "sleep(nil)", "Kernel.sleep", "String.sleep", "String.sleep(1, 2)"] {
        assert!(rescue_arm_exits(&[&top("", call)], 0), "{call}");
    }
    for (prelude, call) in [
        ("", "sleep(1)"),
        ("", "String.sleep(1)"),
        ("", "sleep *a"),
        ("def exec; nil; end", "exec"),
        ("def sleep; nil; end", "sleep"),
    ] {
        assert!(!rescue_arm_exits(&[&top(prelude, call)], 0), "{prelude} / {call}");
    }
    // `class C; def exec` does not veto a toplevel or in-def `exec` — only
    // a TOPLEVEL def binds the implicit call.
    assert!(rescue_arm_exits(&[&in_def("class C; def exec; end; end", "exec")], 0));
}

/// rigor-rs#1011 (rigor#1107) — `loop`'s `bot` stands while the literal
/// block provably cannot raise `StopIteration` AND carries no block-level
/// `break`: `STOP_ITERATION_FREE_NODES` ported as
/// `stop_iteration_free` (any call — even `raise` — declines: the
/// receiver/argument could iterate), plus `Typer::block_level_break` for
/// `break`'s value join. A `&expr` pass has no body to prove, and a
/// declared parameter's default may raise.
#[test]
fn kernel_loop_bot_needs_a_stop_iteration_free_block() {
    for call in [
        "loop do nil end",
        "loop { nil; nil }",
        "loop { next }",
        "loop { redo }",
        "loop { return }",
        "loop { x = 1 }",
        "loop { @i = 1 }",
        "loop { next x = 1 }",
        "String.loop { foo }",
        "String.loop(&e)",
        "Kernel.loop { nil }",
        "::Kernel.loop { nil }",
    ] {
        assert!(rescue_arm_exits(&[&top("", call)], 0), "{call}");
    }
    for (prelude, call) in [
        ("", "loop { break }"),
        ("", "loop { break 5 }"),
        ("", "loop { break if rand > 0 }"),
        ("", "loop { |x| nil }"),
        ("", "loop { || nil }"),
        ("", "loop { |;q| nil }"),
        ("", "loop(&e)"),
        ("", "loop { foo }"),
        ("", "loop { raise }"),
        ("", "loop { exit }"),
        ("", "loop { 1 + 1 }"),
        ("", "loop { x.nope }"),
        ("", "loop { while true; end }"),
        ("", "loop { [1].each { break } }"),
        ("", "loop { @i += 1 }"),
        ("", "loop { Foo }"),
        ("", "loop { $g }"),
        ("", "loop"),
        ("", "String.loop"),
        ("def loop; nil; end", "loop {}"),
        // The `Kernel.` spellings ride the same StopIteration whitelist
        // through `kernel_sig_bot` — they do not bypass it (rigor-rs#1023
        // review).
        ("", "Kernel.loop { break }"),
        ("", "::Kernel.loop { break }"),
        ("", "Kernel.loop { foo }"),
        ("", "Kernel.loop { |x| nil }"),
        ("", "Kernel.loop { || nil }"),
        ("", "Kernel.loop(&e)"),
        ("", "String.loop { break }"),
        ("", "Object.loop { break }"),
    ] {
        assert!(!rescue_arm_exits(&[&top(prelude, call)], 0), "{prelude} / {call}");
    }
    // A `def` on an unrelated or own class does not bind the implicit
    // `loop` — `C#loop` is not the receiver's answer here either (the
    // oracle's `loop {}` in `def m` of `class C; def loop` still types
    // `bot`).
    assert!(rescue_arm_exits(&[&in_def("class C; def loop; nil; end; end", "loop {}")], 0));
}

/// rigor-rs#1023 review — a literal block carrying a block-level `break`
/// joins the break's value into the CALL's type: `<call> { break }` types
/// `nil`, not `bot`, whatever the call resolves to. Every `bot` path
/// declines it on the unfolded walk; the receiverless `EXIT_CALLS` names
/// keep answering the SYNTACTIC arm (`raise("x") { break }` still exits a
/// rescue clause — `branch_unconditionally_exits?` does not look at the
/// block).
#[test]
fn block_level_break_union_keeps_every_bot_call_live() {
    for (prelude, call) in [
        ("", "Kernel.exit { break }"),
        ("", "Kernel.raise { break }"),
        ("", "Kernel.exec { break }"),
        ("", "Kernel.sleep { break }"),
        ("", "Process.exit { break }"),
        ("", "String.exec(\"x\") { break }"),
        ("", "exit! { break }"),
        ("", "exec(\"ls\") { break }"),
        ("", "sleep { break }"),
        ("", "die { break }"),
        ("def die; raise; end", "die { break }"),
    ] {
        // Unfolded walk (the rescue template): the arm is LIVE.
        assert!(!rescue_arm_exits(&[&top(prelude, call)], 0), "{prelude} / {call}");
    }
    // … except the receiverless syntactic names — `raise`/`throw`/`exit`/
    // `abort`/`fail` exit the unfolded arm regardless of the block
    // (`branch_unconditionally_exits?` never looks at one).
    for call in [
        "raise(\"x\") { break }",
        "exit { break }",
        "abort { break }",
        "fail { break }",
        "throw(:x) { break }",
    ] {
        assert!(rescue_arm_exits(&[&top("", call)], 0), "{call} (syntactic)");
    }
}

/// rigor-rs#955 — `retry` is not in
/// `branch_unconditionally_exits?`'s syntactic jump set, but `type_of_jump`
/// types it `bot`, so a rescue arm ending in one terminates through
/// `branch_terminates?`'s type half. A CONDITIONAL retry still has a
/// completing edge, and a begin with a completing normal path (a `nil`
/// protected body, or any `else` value) is not `bot` however its rescue
/// arm ends.
#[test]
fn retry_tail_terminates_through_the_bot_type_half() {
    let retrying_begin = "begin\n    raise\n  rescue\n    retry\n  end";
    let (_, else_t, _) = if_arms(&format!(
        "if rand > 0\n  x = \"s\"\nelse\n  {retrying_begin}\nend\n"
    ));
    assert!(else_t);

    // `retry` nested under an `if` counts only when every arm of that
    // `if` is `bot` — `if c; retry; else; raise; end` terminates,
    // `retry if c` does not.
    let nested = "begin\n    raise\n  rescue\n    if rand > 0\n      retry\n    else\n      raise\n    end\n  end";
    let (_, else_t, _) =
        if_arms(&format!("if rand > 0\n  x = \"s\"\nelse\n  {nested}\nend\n"));
    assert!(else_t);
    let conditional = "begin\n    raise\n  rescue\n    retry if rand > 0\n  end";
    let (_, else_t, _) =
        if_arms(&format!("if rand > 0\n  x = \"s\"\nelse\n  {conditional}\nend\n"));
    assert!(!else_t);
    // The exact conditional shape `live_rescue_results`'s comment names:
    // the ternary is `bot` when both arms are.
    let ternary = "begin\n    raise\n  rescue\n    rand > 0 ? retry : raise\n  end";
    let (_, else_t, _) =
        if_arms(&format!("if rand > 0\n  x = \"s\"\nelse\n  {ternary}\nend\n"));
    assert!(else_t);
    // One retrying clause does not drop the begin while a second rescue
    // clause can still complete.
    let second_live =
        "begin\n    raise\n  rescue ArgumentError\n    retry\n  rescue\n    nil\n  end";
    let (_, else_t, _) =
        if_arms(&format!("if rand > 0\n  x = \"s\"\nelse\n  {second_live}\nend\n"));
    assert!(!else_t);

    // The begin's normal value path must terminate too: a `nil` protected
    // body or an `else` clause keeps the begin — and the outer arm — live.
    for begin in [
        "begin\n    nil\n  rescue\n    retry\n  end",
        "begin\n    raise\n  rescue\n    retry\n  else\n    nil\n  end",
        "begin\n    raise\n  rescue\n    retry\n  else\n  end",
    ] {
        let (_, else_t, _) =
            if_arms(&format!("if rand > 0\n  x = \"s\"\nelse\n  {begin}\nend\n"));
        assert!(!else_t, "{begin}");
    }
    // …while `else; raise` replaces the normal path with a `bot` one, and
    // `ensure` contributes no value path of its own.
    for begin in [
        "begin\n    nil\n  rescue\n    retry\n  else\n    raise\n  end",
        "begin\n    raise\n  rescue\n    retry\n  ensure\n    nil\n  end",
    ] {
        let (_, else_t, _) =
            if_arms(&format!("if rand > 0\n  x = \"s\"\nelse\n  {begin}\nend\n"));
        assert!(else_t, "{begin}");
    }
}

/// rigor-rs#1011 — the folded `if` arms reach the same predicate:
/// `branch_terminates?` (syntactic) and `value_branch_terminates?` (typed)
/// both see a `bot` call's termination.
#[test]
fn folded_if_arms_see_the_extended_bot_calls() {
    let (then_t, else_t, _) = if_arms("if rand > 0\n  Kernel.exit\nelse\n  x = \"t\"\n  Kernel.exit\nend\n");
    assert!(then_t);
    assert!(else_t);
    let (then_t, else_t, _) = if_arms("if rand > 0\n  Process.exit\nelse\n  x = \"t\"\n  loop do nil end\nend\n");
    assert!(then_t);
    assert!(else_t);
    let (then_t, else_t, _) =
        if_arms("def die; raise; end\nif rand > 0\n  die\nelse\n  x = \"t\"\n  die\nend\n");
    assert!(then_t);
    assert!(else_t);
    // The decline side in the same frame.
    let (then_t, else_t, _) = if_arms("if rand > 0\n  Thread.exit\nelse\n  x = \"t\"\n  loop { break }\nend\n");
    assert!(!then_t);
    assert!(!else_t);
}
