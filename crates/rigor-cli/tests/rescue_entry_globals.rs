//! Rescue clause entry: keep unrelated bindings while declining stale globals.

use rigor_index::CoreIndex;
use rigor_parse::{lower, parse};
use rigor_rules::{CALL_UNDEFINED_METHOD, analyze};
use rigor_types::Interner;

fn undefined_methods(source: &[u8]) -> Vec<(usize, String)> {
    let ast = lower(&parse(source));
    analyze(&ast, &mut Interner::new(), &CoreIndex::new())
        .into_iter()
        .filter(|diag| diag.rule_id == CALL_UNDEFINED_METHOD)
        .map(|diag| (diag.start_offset, diag.message))
        .collect()
}

#[test]
fn rescue_keeps_last_line_when_protected_body_cannot_set_it() {
    for protected in [
        "0",
        "def reader; gets; end",
        "Thread.new { gets }",
        "send(:puts, 'message')",
    ] {
        let source = format!("$_ = 1\nbegin\n  {protected}\nrescue\n  $_.chomp\nend\n");
        assert_eq!(
            undefined_methods(source.as_bytes()),
            [(
                source.find("chomp").unwrap(),
                "undefined method `chomp' for 1".into()
            )],
            "{source}",
        );
    }
}

#[test]
fn rescue_forgets_last_line_after_a_possible_setter() {
    for protected in [
        "gets",
        "$_ = 'new'",
        "$_ ||= 'new'",
        "$_, other = 'new', 0",
        "for $_ in ['new']; end",
        "begin; 0; rescue => $_; nil; end",
        "send(:readline)",
        "[STDIN].each(&:gets)",
        "Thread.new(*[gets]) { 0 }",
        "-> { gets }",
        "if false; gets; end",
    ] {
        let source = format!("$_ = 1\nbegin\n  {protected}\nrescue\n  $_.chomp\nend\n");
        assert!(undefined_methods(source.as_bytes()).is_empty(), "{source}");
    }
}

#[test]
fn rescue_designators_and_bodies_see_the_right_last_line() {
    for source in [
        b"$_ = 1\nbegin\n  gets\nrescue ($_.chomp; StandardError)\n  nil\nend\n".as_slice(),
        b"$_ = 1\nbegin\n  0\nrescue => $_\n  $_.message\nend\n",
        b"$_ = 1\nout = (begin\n  gets\nrescue\n  $_.chomp\nend)\n",
        b"$_ = 1\nbegin\n  gets\nrescue TypeError\n  $_.chomp\nrescue StandardError\n  $_.chomp\nend\n",
    ] {
        assert!(undefined_methods(source).is_empty(), "{}", String::from_utf8_lossy(source));
    }
}

#[test]
fn other_globals_and_new_assignments_still_reach_the_handler() {
    let source = "$_ = 1\n$g = 1\nbegin\n  gets\nrescue\n  $_.chomp\n  $g.also_missing\n  $_ = 'new'\n  $_.even?\nend\n";
    assert_eq!(
        undefined_methods(source.as_bytes()),
        [
            (
                source.find("also_missing").unwrap(),
                "undefined method `also_missing' for 1".into()
            ),
            (
                source.find("even?").unwrap(),
                "undefined method `even?' for \"new\"".into()
            ),
        ],
    );
}

#[test]
fn stale_backtrace_is_not_used_by_the_rescue_or_lost_after_it() {
    let source = "$@ = [1]\nif true\n  begin\n    0\n  rescue\n    $@.first.chomp\n  end\nend\n$@.first.chomp\n";
    assert_eq!(
        undefined_methods(source.as_bytes()),
        [(
            source.rfind("chomp").unwrap(),
            "undefined method `chomp' for 1".into()
        )],
    );
}

#[test]
fn folded_rescue_join_does_not_reintroduce_forgotten_last_line() {
    let source = b"$_ = 1\nif true\n  begin\n    gets\n  rescue\n    x = $_\n  end\nend\n$_.chomp\nx.chomp\n";
    assert!(undefined_methods(source).is_empty());
}
