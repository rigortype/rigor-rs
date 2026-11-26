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

#[test]
fn plain_reader_unbinds_last_line_on_the_primary_path() {
    for source in [
        b"$_ = 1\ngets\n$_.chomp\n".as_slice(),
        b"$_ = 1\nif rand > 0\n  gets\nend\n$_.chomp\n".as_slice(),
        b"$_ = 1\nbegin\n  gets\nend\n$_.chomp\n".as_slice(),
        b"$_ = 1\nbegin\n  gets\nrescue\n  nil\nend\n$_.chomp\n".as_slice(),
        b"$_ = 1\nbegin\n  gets\nrescue\nend\n$_.chomp\n".as_slice(),
        b"$_ = 1\nbegin\n  gets\nrescue\n  nil\nelse\n  $_.chomp\nend\n".as_slice(),
        b"$_ = 1\nbegin\n  gets\nrescue\n  nil\nensure\n  $_.chomp\nend\n".as_slice(),
        b"$_ = 1\ngets\nbegin\n  0\nrescue\n  $_.chomp\nend\n".as_slice(),
        b"$_ = 1\nIO.foreach(\"x\") { |line| $_.chomp }\n".as_slice(),
        b"$_ = 1\nIO.foreach(\"x\") { |line| nil }\n$_.chomp\n".as_slice(),
        b"$_ = 1\ncase rand\nwhen 1\n  gets\n  $_.chomp\nend\n".as_slice(),
        b"$_ = 1\nwhile rand > 0\n  gets\n  $_.chomp\nend\n".as_slice(),
        b"$_ = 1\nfor x in gets\n  $_.chomp\nend\n".as_slice(),
        b"$_ = 1\nif gets\n  $_.chomp\nend\n".as_slice(),
        b"$_ = 1\nwhile gets\n  $_.chomp\nend\n".as_slice(),
        b"$_ = 1\ncase rand\nwhen gets\n  $_.chomp\nend\n".as_slice(),
        b"$_ = 1\ncase gets\nwhen String\n  $_.chomp\nend\n".as_slice(),
        b"$_ = 1\nbegin\n  0\nrescue\n  nil\nelse\n  gets\n  $_.chomp\nend\n".as_slice(),
        b"$_ = 1\nbegin\n  0\nrescue\n  nil\nensure\n  gets\n  $_.chomp\nend\n".as_slice(),
    ] {
        assert!(undefined_methods(source).is_empty(), "{}", String::from_utf8_lossy(source));
    }
}

#[test]
fn reads_before_the_plain_reader_keep_the_last_line_binding() {
    for source in [
        "$_ = 1\n$_.chomp\ngets\n",
        "$_ = 1\ncase rand\nwhen 1\n  nil\n  $_.chomp\nend\n",
        "$_ = 1\nwhile rand > 0\n  nil\n  $_.chomp\nend\n",
    ] {
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
fn rescue_binds_error_info_and_backtrace() {
    for (source, method, expected) in [
        (
            "begin\n  0\nrescue\n  $!.chomp\nend\n",
            "chomp",
            "undefined method `chomp' for StandardError",
        ),
        (
            "begin\n  0\nrescue ArgumentError\n  $!.chomp\nend\n",
            "chomp",
            "undefined method `chomp' for ArgumentError",
        ),
        (
            "begin\n  0\nrescue\n  $@.even?\nend\n",
            "even?",
            "undefined method `even?' for Array[String]",
        ),
    ] {
        assert_eq!(
            undefined_methods(source.as_bytes()),
            [(source.find(method).unwrap(), expected.into())],
            "{source}",
        );
    }
}

#[test]
fn rescue_modifier_fallback_does_not_inherit_the_outer_error_info() {
    for source in [
        b"begin\n  0\nrescue\n  x = foo rescue $!.chomp\nend\n".as_slice(),
        b"begin\n  0\nrescue\n  x = (foo rescue $@.even?)\nend\n".as_slice(),
    ] {
        assert!(undefined_methods(source).is_empty(), "{}", String::from_utf8_lossy(source));
    }

    // The raising-expression half still runs in the clause's frame.
    for (source, method, expected) in [
        (
            "begin\n  0\nrescue\n  x = (($!.chomp) rescue nil)\nend\n",
            "chomp",
            "undefined method `chomp' for StandardError",
        ),
        (
            "begin\n  0\nrescue\n  x = (($@.even?) rescue nil)\nend\n",
            "even?",
            "undefined method `even?' for Array[String]",
        ),
    ] {
        assert_eq!(
            undefined_methods(source.as_bytes()),
            [(source.find(method).unwrap(), expected.into())],
            "{source}",
        );
    }
}

#[test]
fn ensure_reads_no_error_info_while_else_keeps_the_outer_exception() {
    for source in [
        b"begin\n  0\nrescue\n  begin\n    1\n  rescue\n    nil\n  ensure\n    $!.chomp\n  end\nend\n".as_slice(),
        b"begin\n  0\nrescue\n  begin\n    1\n  rescue\n    nil\n  ensure\n    $@.even?\n  end\nend\n".as_slice(),
    ] {
        assert!(undefined_methods(source).is_empty(), "{}", String::from_utf8_lossy(source));
    }

    let source = "begin\n  0\nrescue\n  begin\n    1\n  rescue\n    nil\n  else\n    $!.chomp\n  end\nend\n";
    assert_eq!(
        undefined_methods(source.as_bytes()),
        [(
            source.find("chomp").unwrap(),
            "undefined method `chomp' for StandardError".into()
        )],
    );
}

#[test]
fn stored_and_fresh_frame_closures_do_not_inherit_the_error_info() {
    for body in [
        "-> { $!.chomp }.call",
        "lambda { $!.chomp }.call",
        "proc { $!.chomp }.call",
        "Proc.new { $!.chomp }.call",
        "Hash.new { $!.chomp }[:x]",
        "Enumerator.new { $!.chomp }.to_a",
        "Thread.new { $!.chomp }",
        "define_method(:x) { $!.chomp }",
    ] {
        let source = format!("begin\n  0\nrescue\n  {body}\nend\n");
        assert!(undefined_methods(source.as_bytes()).is_empty(), "{source}");
    }

    // An ordinary yielding block runs while the clause's exception is still
    // in flight and keeps the binding.
    let source = "begin\n  0\nrescue\n  [1].each { $!.chomp }\nend\n";
    assert_eq!(
        undefined_methods(source.as_bytes()),
        [(
            source.find("chomp").unwrap(),
            "undefined method `chomp' for StandardError".into()
        )],
    );
}

#[test]
fn unrelated_instance_case_equality_does_not_veto_the_error_type() {
    let source = "class X\n  def ===(o) = true\nend\nbegin\n  0\nrescue ArgumentError\n  $!.chomp\nend\n";
    assert_eq!(
        undefined_methods(source.as_bytes()),
        [(
            source.find("chomp").unwrap(),
            "undefined method `chomp' for ArgumentError".into()
        )],
    );
}

#[test]
fn error_info_binding_declines_on_guards_and_backtrace_vetoes() {
    for source in [
        b"begin\n  0\nrescue Integer\n  $!.chomp\nend\n".as_slice(),
        b"begin\n  0\nrescue\n  $!.is_a?(StandardError)\n  $!.chomp\nend\n".as_slice(),
        b"begin\n  0\nrescue => e\n  e.is_a?(StandardError)\n  $!.chomp\nend\n".as_slice(),
        b"begin\n  0\nrescue\n  case $!\n  when StandardError then nil\n  end\n  $!.chomp\nend\n".as_slice(),
        b"begin\n  0\nrescue\n  StandardError === $!\n  $!.chomp\nend\n".as_slice(),
        b"define_method(:===) { |o| true }\nbegin\n  0\nrescue ArgumentError\n  $!.chomp\nend\n".as_slice(),
        b"class ArgumentError\n  def self.===(o) = true\nend\nbegin\n  0\nrescue ArgumentError\n  $!.chomp\nend\n".as_slice(),
        b"begin\n  0\nrescue\n  $!.set_backtrace(nil)\n  $@.even?\nend\n".as_slice(),
        b"def backtrace; end\nbegin\n  0\nrescue\n  $@.even?\nend\n".as_slice(),
        b"begin\n  0\nrescue\n  $@.even?\nend\ndef backtrace; end\n".as_slice(),
    ] {
        assert!(undefined_methods(source).is_empty(), "{}", String::from_utf8_lossy(source));
    }
}

#[test]
fn handler_write_still_restores_the_outer_backtrace() {
    let source = "$@ = [1]\nif true\n  begin\n    0\n  rescue\n    $@ = [2]\n  end\nend\n$@.first.chomp\n";
    assert_eq!(
        undefined_methods(source.as_bytes()),
        [(
            source.rfind("chomp").unwrap(),
            "undefined method `chomp' for 1".into()
        )],
    );
}
