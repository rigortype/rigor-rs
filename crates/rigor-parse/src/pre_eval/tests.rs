use super::*;

fn defs(src: &str) -> Vec<(String, String, bool, PatchVisibility, bool)> {
    scan_pre_eval(src.as_bytes())
        .expect("parses")
        .into_iter()
        .filter_map(|i| match i {
            PreEvalItem::Def { class_name, method, singleton, visibility, guarded, .. } => {
                Some((class_name, method, singleton, visibility, guarded))
            }
            _ => None,
        })
        .collect()
}

fn d(c: &str, m: &str, s: bool, v: PatchVisibility, g: bool) -> (String, String, bool, PatchVisibility, bool) {
    (c.to_string(), m.to_string(), s, v, g)
}

use PatchVisibility::*;

#[test]
fn records_defs_in_class_and_module_bodies_only() {
    assert_eq!(
        defs("def top = 1\nclass String; def a = 1; def self.b = 2; end\nmodule Outer; class String; def c = 3; end; end\nclass ::Float; def f = 1; end\nclass Integer; class << self; def g = 1; end; end\n"),
        vec![
            d("String", "a", false, Public, false),
            d("String", "b", true, Public, false),
            d("Outer::String", "c", false, Public, false),
            d("Float", "f", false, Public, false),
            d("Integer", "g", true, Public, false),
        ]
    );
}

#[test]
fn does_not_descend_into_def_bodies() {
    assert_eq!(defs("class A; def m; def nested_in_def = 1; end; end\n"), vec![d("A", "m", false, Public, false)]);
}

#[test]
fn block_bodies_are_walked() {
    assert_eq!(defs("[1].each { class String; def in_block = 1; end }\n"), vec![d("String", "in_block", false, Public, false)]);
}

#[test]
fn visibility_readings() {
    assert_eq!(
        defs("module Kernel; private def kp = 1; end\nclass Numeric; private; def nprv = 1; end\nclass Numeric; protected def np = 2; end\nmodule Kernel; module_function def kmf = 3; end\nclass Integer; if x; private; end; def after = 1; end\n"),
        vec![
            d("Kernel", "kp", false, Private, false),
            d("Numeric", "nprv", false, Private, false),
            d("Numeric", "np", false, Protected, false),
            d("Kernel", "kmf", false, Private, false),
            d("Kernel", "kmf", true, Public, false),
            d("Integer", "after", false, Public, false),
        ]
    );
}

#[test]
fn definedness_guard_marks_the_undefined_branch() {
    assert_eq!(
        defs("class String\n  def gg = 1 unless method_defined?(:gg)\n  def gh = 1 if method_defined?(:gh)\n  unless instance_methods.include?(:gi)\n    def gi = 1\n  end\nend\n"),
        vec![
            d("String", "gg", false, Public, true),
            d("String", "gh", false, Public, false),
            d("String", "gi", false, Public, true),
        ]
    );
}

#[test]
fn mixins_and_guarded_mixins() {
    let items = scan_pre_eval(
        b"module GM; def gm = 1; end\nNumeric.include(GM) unless Numeric.include?(GM)\nNumeric.include(GM) if ENV['X']\nclass Integer; include GM; end\ninclude TopExt\nNumeric.prepend(::Pre)\n",
    )
    .unwrap();
    let mixins: Vec<(String, String, bool)> = items
        .into_iter()
        .filter_map(|i| match i {
            PreEvalItem::Mixin { owner, module_name, prepend, .. } => Some((owner, module_name, prepend)),
            _ => None,
        })
        .collect();
    assert_eq!(
        mixins,
        vec![
            ("Numeric".into(), "GM".into(), false),
            ("Integer".into(), "GM".into(), false),
            ("Object".into(), "TopExt".into(), false),
            ("Numeric".into(), "Pre".into(), true),
        ]
    );
}

#[test]
fn syntax_error_skips_the_file() {
    assert_eq!(scan_pre_eval(b"class String; def x = 1; end\ndef (\n"), Err(PreEvalScanError::Syntax));
}
