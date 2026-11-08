use super::*;

fn candidates_tagged(tag: &str, src: &str, include_private: bool) -> Vec<Candidate> {
    // Write to a UNIQUE temp file per test so parallel runs never race on a
    // shared path (write to `generate_run`'s read path is the point).
    let dir = std::env::temp_dir().join(format!("rigor_siggen_test_{tag}"));
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("t.rb");
    std::fs::write(&file, src).unwrap();
    // An EMPTY sig env: no project RBS ⇒ every candidate is `new_method`
    // (these tests exercise inference/rendering, not env classification —
    // that has its own `sig_env` unit tests + the oracle E2E gate).
    let env = SigEnv::build(&[]);
    let out = generate_run(&[file.to_str().unwrap().to_string()], include_private, &env, &writes).candidates;
    let _ = std::fs::remove_file(&file);
    out
}

/// Generate candidates for `rb_src` with a project sig env built from
/// `rbs_src` (the sig-gen-local [`SigEnv`]) — the env-classification unit
/// harness. A fresh unique dir per tag isolates parallel runs.
fn candidates_with_sig(tag: &str, rb_src: &str, rbs_src: &str) -> Vec<Candidate> {
    let dir = std::env::temp_dir().join(format!("rigor_siggen_env_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::create_dir_all(dir.join("sig")).unwrap();
    let rb = dir.join("lib/foo.rb");
    std::fs::write(&rb, rb_src).unwrap();
    std::fs::write(dir.join("sig/foo.rbs"), rbs_src).unwrap();
    let env = SigEnv::build(&[dir.join("sig")]);
    let out = generate_run(&[rb.to_str().unwrap().to_string()], false, &env, &writes).candidates;
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn env_no_sig_class_is_new_method() {
    // Probe A: the class is absent from the env ⇒ NEW_METHOD (`# [new]`).
    let cs = candidates_with_sig("no_env", "class Foo\n  def hash\n    1\n  end\nend\n", "class Bar\nend\n");
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].classification, "new_method");
    assert_eq!(cs[0].declared_return_rbs, None);
    assert_eq!(cs[0].rbs, "def hash: () -> 1");
}

#[test]
fn env_empty_decl_resolves_inherited_tighter() {
    // Probe N: an EMPTY `class Foo` in sig ⇒ `hash` resolves through Object
    // (declared `Integer`), so it is not new. The literal `1` does not tighten
    // it (#837, `pins_literal_over_declaration?`) ⇒ DROP. Unresolved, it would
    // have been a `# [new]` row.
    let cs = candidates_with_sig("empty_decl", "class Foo\n  def hash\n    1\n  end\nend\n", "class Foo\nend\n");
    assert!(cs.is_empty(), "{cs:?}");
}

#[test]
fn env_fqn_gate_resolves_nested_class() {
    // Probe Q1: the gate is FQN-keyed — a nested `M::Foo` resolves (rigor-index
    // short-name folding cannot; this is why SigEnv exists).
    let cs = candidates_with_sig(
        "fqn",
        "module M\n  class Foo\n    def hash\n      1\n    end\n  end\nend\n",
        "module M\n  class Foo\n  end\nend\n",
    );
    // Resolved (not `# [new]`); the literal does not tighten it (#837).
    assert!(cs.is_empty(), "{cs:?}");
}

#[test]
fn env_project_superclass_resolves_tighter() {
    // Probe O: `class Foo < Base`, Base declares `greeting: () -> String`.
    let cs = candidates_with_sig(
        "super",
        "class Foo\n  def greeting\n    \"hi\"\n  end\nend\n",
        "class Base\n  def greeting: () -> String\nend\n\nclass Foo < Base\nend\n",
    );
    // Resolved through Base (not `# [new]`); `"hi"` does not tighten (#837).
    assert!(cs.is_empty(), "{cs:?}");
}

#[test]
fn env_inherited_equivalent_drops() {
    // Probe P: `def hash; [1].size; end` folds `1` but the raw tail `[1].size`
    // is not a literal ⇒ computed_literal_tightening ⇒ DROP (No candidates).
    let cs = candidates_with_sig("equiv", "class Foo\n  def hash\n    [1].size\n  end\nend\n", "class Foo\nend\n");
    assert!(cs.is_empty(), "computed-literal drop: {cs:?}");
}

#[test]
fn env_initialize_bypasses_classification() {
    // Probe K: an identical declared `initialize` still emits `# [new]`.
    let cs = candidates_with_sig(
        "init",
        "class Foo\n  def initialize(a)\n    @a = a\n  end\nend\n",
        "class Foo\n  def initialize: (untyped) -> void\nend\n",
    );
    assert_eq!(cs[0].classification, "new_method");
    assert_eq!(cs[0].rbs, "def initialize: (untyped) -> void");
}

#[test]
fn env_attr_reader_classifies_like_method() {
    // Probe I: `attr_reader name: String` declares `def name` (not `# [new]`);
    // the literal `"n"` does not tighten it (#837).
    let cs = candidates_with_sig("attr", "class Foo\n  def name\n    \"n\"\n  end\nend\n", "class Foo\n  attr_reader name: String\nend\n");
    assert!(cs.is_empty(), "{cs:?}");
}

#[test]
fn env_singleton_own_and_inherited() {
    // Probe Q2: own `def self.build: () -> Integer` ⇒ tighter.
    let own = candidates_with_sig(
        "sing_own",
        "class Foo\n  def self.build\n    1\n  end\nend\n",
        "class Foo\n  def self.build: () -> Integer\nend\n",
    );
    // Declared (not `# [new]`); the literal does not tighten it (#837).
    assert!(own.is_empty(), "{own:?}");

    // Probe Q3: `def self.hash` on an empty `class Foo` inherits `Object#hash`
    // through the class object's `Class`/`Module`/`Object` ancestry.
    let inh = candidates_with_sig(
        "sing_inh",
        "class Foo\n  def self.hash\n    1\n  end\nend\n",
        "class Foo\nend\n",
    );
    assert!(inh.is_empty(), "{inh:?}");
}

#[test]
fn env_literal_tightening_emits_but_computed_drops() {
    // Since #837 a literal never tightens, directly authored or not.
    let lit = candidates_with_sig("lit", "class Foo\n  def m\n    1\n  end\nend\n", "class Foo\n  def m: () -> Integer\nend\n");
    assert!(lit.is_empty(), "{lit:?}");
    // Assignment tail: `x = 1` types Constant<1> but the RAW tail is a write,
    // NOT a literal ⇒ computed_literal_tightening ⇒ DROP (no assignment unwrap).
    let asn = candidates_with_sig("asn", "class Foo\n  def m\n    x = 1\n  end\nend\n", "class Foo\n  def m: () -> Integer\nend\n");
    assert!(asn.is_empty(), "assignment tail is not a direct literal: {asn:?}");
}

#[test]
fn env_collection_to_shape_drops() {
    // Declared bare `Array`, inferred `[1, 2]` (Tuple) ⇒
    // narrows_collection_to_shape ⇒ DROP.
    let cs = candidates_with_sig("coll", "class Foo\n  def m\n    [1, 2]\n  end\nend\n", "class Foo\n  def m: () -> Array\nend\n");
    assert!(cs.is_empty(), "collection→shape drop: {cs:?}");
}

#[test]
fn env_unresolvable_declared_returns_drop() {
    // optional / union / untyped / multi-overload / generic-args declared ⇒
    // Declared(None) or not-tighter ⇒ DROP (No candidates).
    for (tag, decl) in [
        ("opt", "def m: () -> String?"),
        ("uni", "def m: () -> (String | Integer)"),
        ("unt", "def m: () -> untyped"),
        ("gen", "def m: () -> Array[Integer]"),
        ("wid", "def m: () -> Integer"), // wider: inferred "hi" is String
    ] {
        let rbs = format!("class Foo\n  {decl}\nend\n");
        let cs = candidates_with_sig(tag, "class Foo\n  def m\n    \"hi\"\n  end\nend\n", &rbs);
        assert!(cs.is_empty(), "{tag}: expected DROP, got {cs:?}");
    }
    // Declared String, inferred "hi": a literal does not tighten (#837).
    let lit = candidates_with_sig("tight", "class Foo\n  def m\n    \"hi\"\n  end\nend\n", "class Foo\n  def m: () -> String\nend\n");
    assert!(lit.is_empty(), "{lit:?}");
}

#[test]
fn env_incomplete_chain_drops_not_new() {
    // Pitfall g: an unknown superclass (`< Unknown`, no sig) makes the chain
    // incomplete ⇒ DROP (conservative), NOT `# [new]` — a wrong `# [new]` on a
    // method the reference tags `# [tighter]` would be the hard-guarantee break.
    let cs = candidates_with_sig(
        "incomplete",
        "class Foo\n  def zzz\n    \"hi\"\n  end\nend\n",
        "class Foo < Unknown\nend\n",
    );
    assert!(cs.is_empty(), "incomplete-chain drop: {cs:?}");
}

#[test]
fn emits_value_pinned_returns_for_public_instance_methods() {
    let src = "class Foo\n  def greeting\n    \"hello\"\n  end\n\n  def count\n    42\n  end\nend\n";
    let cs = candidates_tagged("emit", src, false);
    let rbs: Vec<&str> = cs.iter().map(|c| c.rbs.as_str()).collect();
    assert_eq!(rbs, vec!["def greeting: () -> \"hello\"", "def count: () -> 42"]);
    assert!(cs.iter().all(|c| c.class_name == "Foo" && c.kind == "instance"));
}

#[test]
fn skips_private_methods_by_default_and_includes_with_flag() {
    let src = "class Foo\n  def pub\n    1\n  end\n\n  private\n\n  def secret\n    2\n  end\nend\n";
    let public_only = candidates_tagged("priv", src, false);
    assert_eq!(public_only.iter().map(|c| c.method_name.as_str()).collect::<Vec<_>>(), vec!["pub"]);
    let with_private = candidates_tagged("priv", src, true);
    assert_eq!(
        with_private.iter().map(|c| c.method_name.as_str()).collect::<Vec<_>>(),
        vec!["pub", "secret"]
    );
}

#[test]
fn skips_complex_parameter_shapes() {
    // A keyword / splat / block param declines (params = None) → skipped.
    let src = "class Foo\n  def kw(a:)\n    1\n  end\n\n  def splat(*a)\n    2\n  end\nend\n";
    assert!(candidates_tagged("cplx", src, false).is_empty());
}

#[test]
fn emits_required_positional_params_as_untyped() {
    let src = "class Foo\n  def add(a, b)\n    1\n  end\nend\n";
    let cs = candidates_tagged("pos", src, false);
    assert_eq!(cs[0].rbs, "def add: (untyped, untyped) -> 1");
}

#[test]
fn nests_qualified_class_name() {
    let src = "module A\n  class B\n    def m\n      1\n    end\n  end\nend\n";
    let cs = candidates_tagged("nest", src, false);
    assert_eq!(cs[0].class_name, "A::B");
}

#[test]
fn skips_untyped_return() {
    // A def-local binding types Dynamic against the top-level env → skipped.
    let src = "class Foo\n  def m(x)\n    x\n  end\nend\n";
    assert!(candidates_tagged("untyped", src, false).is_empty());
}

#[test]
fn unions_explicit_returns_with_tail_in_describe_order() {
    // Oracle-probed matrix (2026-07-10): members sort by describe(:short) —
    // `"s"` < `1` — and the union paren-wraps in method position.
    let src = "class A\n  def m(x)\n    return 1 if x\n    \"s\"\n  end\nend\n";
    let cs = candidates_tagged("union", src, false);
    assert_eq!(cs[0].rbs, "def m: (untyped) -> (\"s\" | 1)");
}

#[test]
fn bare_return_contributes_nil() {
    let src = "class A\n  def m(x)\n    return if x\n    \"s\"\n  end\nend\n";
    let cs = candidates_tagged("bareret", src, false);
    assert_eq!(cs[0].rbs, "def m: (untyped) -> (\"s\" | nil)");
}

#[test]
fn tail_return_types_as_its_value_and_dedups() {
    // A tail `return 42` types 42 (oracle) and dedups against the collected
    // return; a same-value return collapses to the single member.
    let src = "class A\n  def t\n    return 42\n  end\n  def s(x)\n    return 1 if x\n    1\n  end\nend\n";
    let cs = candidates_tagged("tailret", src, false);
    assert_eq!(cs[0].rbs, "def t: () -> 42");
    assert_eq!(cs[1].rbs, "def s: (untyped) -> 1");
}

#[test]
fn block_return_is_collected_and_multi_return_skips() {
    // A return inside an ORDINARY block exits the method and the reference
    // collects it (`ReturnBarrier`, issue #1382 — oracle-probed at the pin:
    // `("s" | 5)`); only a nested def / lambda / define_method body is a
    // barrier. A multi-value return skips the method (the reference silently
    // drops its type and emits `"s"`, an unsound emit we do not adopt).
    let src = "class A\n  def b(x)\n    [1].each { return 5 }\n    \"s\"\n  end\n  def m(x)\n    return 1, 2 if x\n    \"s\"\n  end\nend\n";
    let cs = candidates_tagged("blockret", src, false);
    assert_eq!(cs.len(), 1, "only the block-return method emits: {cs:?}");
    assert_eq!(cs[0].rbs, "def b: (untyped) -> (\"s\" | 5)");
}

#[test]
fn nested_source_class_instance_renders_fully_qualified() {
    // A NESTED class's instance return is QUALIFIED to its FQN via Ruby
    // constant resolution from the enclosing scope (`Inner` written in
    // `Outer::Maker` resolves to `Outer::Inner`) — byte-identical to the
    // reference, which names a source nominal fully-qualified.
    let src = "module Outer\n  class Inner\n  end\n  class Maker\n    def make\n      Inner.new\n    end\n  end\nend\n";
    let cs = candidates_tagged("nestcls", src, false);
    assert_eq!(cs.len(), 1, "{cs:?}");
    assert_eq!(cs[0].rbs, "def make: () -> Outer::Inner");
}

#[test]
fn data_define_constant_return_is_fully_qualified() {
    // `Const = Data.define(...)` typed to a source nominal renders the
    // reference's fully-qualified constant name (`Rigor::Triage::Selector`),
    // NOT the written short `Selector`. `Selector` resolves as a source class
    // here only because it collides with a core RBS name; the qualification
    // logic is what this asserts.
    let src = "module Rigor\n  class Triage\n    Selector = Data.define(:a)\n    def make\n      Selector.new(a: 1)\n    end\n  end\nend\n";
    let cs = candidates_tagged("datadef", src, false);
    let make = cs.iter().find(|c| c.method_name == "make").expect("make emitted");
    assert_eq!(make.rbs, "def make: () -> Rigor::Triage::Selector");
}

/// The RBS lines a source generates, as `<class> <kind> <rbs>` rows — the
/// shape the meta-class assertions below read.
fn rows(cs: &[Candidate]) -> Vec<String> {
    cs.iter().map(|c| format!("{} {} {}", c.class_name, c.kind, c.rbs)).collect()
}

#[test]
fn data_define_constant_emits_readers_and_both_constructor_forms() {
    // `::Data.new` is declared `() -> bot`, so an undeclared `.new` on the
    // subclass would make every construction an arity error; `.[]` is absent
    // from `::Data`'s RBS entirely. Every Data member is required (no `?`).
    let src = "module Geo\n  Pair = Data.define(:left, :right)\nend\n";
    let cs = candidates_tagged("metadata", src, false);
    assert_eq!(
        rows(&cs),
        vec![
            "Geo::Pair instance def left: () -> untyped",
            "Geo::Pair instance def right: () -> untyped",
            "Geo::Pair singleton def self.new: (left: untyped, right: untyped) -> instance | (untyped left, untyped right) -> instance",
            "Geo::Pair singleton def self.[]: (left: untyped, right: untyped) -> instance | (untyped left, untyped right) -> instance",
        ]
    );
    assert_eq!(cs[0].decl_header, "class Geo::Pair < ::Data");
}

#[test]
fn struct_new_adds_writers_and_optional_positions() {
    // A Struct's members are mutable (writers) and an omitted one fills with
    // `nil` (every position optional). `::Struct` is generic, so the emitted
    // ancestry must carry an explicit type argument.
    let src = "Point = Struct.new(:x, :y)\n";
    let cs = candidates_tagged("metastruct", src, false);
    assert_eq!(
        rows(&cs),
        vec![
            "Point instance def x: () -> untyped",
            "Point instance def y: () -> untyped",
            "Point instance def x=: (untyped) -> untyped",
            "Point instance def y=: (untyped) -> untyped",
            "Point singleton def self.new: (?x: untyped, ?y: untyped) -> instance | (?untyped x, ?untyped y) -> instance",
            "Point singleton def self.[]: (?x: untyped, ?y: untyped) -> instance | (?untyped x, ?untyped y) -> instance",
        ]
    );
    assert_eq!(cs[0].decl_header, "class Point < ::Struct[untyped]");
}

#[test]
fn struct_keyword_init_true_drops_the_positional_overload() {
    // Only a LITERAL `keyword_init: true` narrows to keywords: a `false` flag
    // means "absent or literal false", and since Ruby 3.2 the absent case
    // accepts both forms, so both are emitted.
    let cs = candidates_tagged("metakw", "Keyed = Struct.new(:a, keyword_init: true)\n", false);
    let ctor = cs.iter().find(|c| c.method_name == "new").expect("ctor emitted");
    assert_eq!(ctor.rbs, "def self.new: (?a: untyped) -> instance");
    let cs = candidates_tagged("metakwf", "Loose = Struct.new(:a, keyword_init: false)\n", false);
    let ctor = cs.iter().find(|c| c.method_name == "new").expect("ctor emitted");
    assert_eq!(ctor.rbs, "def self.new: (?a: untyped) -> instance | (?untyped a) -> instance");
}

#[test]
fn named_subclass_and_qualified_receiver_forms_are_recognised() {
    // `class Point < Data.define(...)` is the second spelling, and its
    // COMPUTED superclass is exactly the one the class walk refuses to guess
    // at; `::Data` is the same constant as `Data`. The class's own defs follow
    // the synthesised members.
    let src = "module Geo\n  class Named < Data.define(:tag)\n    def shout\n      \"hi\"\n    end\n  end\n  Aliased = ::Data.define(:only)\nend\n";
    let cs = candidates_tagged("metanamed", src, false);
    assert_eq!(
        rows(&cs),
        vec![
            "Geo::Named instance def tag: () -> untyped",
            "Geo::Named singleton def self.new: (tag: untyped) -> instance | (untyped tag) -> instance",
            "Geo::Named singleton def self.[]: (tag: untyped) -> instance | (untyped tag) -> instance",
            "Geo::Aliased instance def only: () -> untyped",
            "Geo::Aliased singleton def self.new: (only: untyped) -> instance | (untyped only) -> instance",
            "Geo::Aliased singleton def self.[]: (only: untyped) -> instance | (untyped only) -> instance",
            "Geo::Named instance def shout: () -> \"hi\"",
        ]
    );
    assert_eq!(cs[0].decl_header, "class Geo::Named < ::Data");
}

#[test]
fn meta_block_defs_bind_on_the_new_class_and_leave_the_module_a_module() {
    // The runtime `class_eval`s the block into the class it just stamped, so
    // `describe` is `Geo::Blocked#describe`, NOT `Geo#describe` — and `Geo`
    // must still print as a `module` (a method-bearing leaf otherwise defaults
    // to `class`, which raises DuplicatedDeclarationError beside the real
    // module). `private` inside the block still hides a def.
    let src = "module Geo\n  Blocked = Data.define(:label) do\n    def describe\n      \"d\"\n    end\n\n    private\n\n    def hidden\n      \"h\"\n    end\n  end\n\n  def self.origin\n    \"0,0\"\n  end\nend\n";
    let cs = candidates_tagged("metablock", src, false);
    assert_eq!(
        rows(&cs),
        vec![
            "Geo::Blocked instance def label: () -> untyped",
            "Geo::Blocked singleton def self.new: (label: untyped) -> instance | (untyped label) -> instance",
            "Geo::Blocked singleton def self.[]: (label: untyped) -> instance | (untyped label) -> instance",
            "Geo::Blocked instance def describe: () -> \"d\"",
            "Geo singleton def self.origin: () -> \"0,0\"",
        ]
    );
    let module_row = cs.iter().find(|c| c.class_name == "Geo").unwrap();
    assert_eq!(module_row.decl_header, "module Geo");
    // `--include-private` recovers the hidden def, proving the skip above is
    // the visibility scan and not the block walk missing the def entirely.
    let all = candidates_tagged("metablockp", src, true);
    assert!(all.iter().any(|c| c.method_name == "hidden" && c.class_name == "Geo::Blocked"));
}

#[test]
fn degenerate_meta_forms_declare_nothing() {
    // A layout needs at least one LITERAL Symbol member: a member-less
    // `Data.define`, the `Struct.new("Legacy", :a)` named-factory form, and a
    // splatted member list are all un-describable, so no class is declared for
    // them at all (declaring one without members is worse than not declaring it).
    for (tag, src) in [
        ("metaempty", "Empty = Data.define\n"),
        ("metalegacy", "NamedStruct = Struct.new(\"Legacy\", :a)\n"),
        ("metasplat", "Dyn = Data.define(*MEMBERS)\n"),
    ] {
        assert!(candidates_tagged(tag, src, false).is_empty(), "{src}");
    }
}

#[test]
fn meta_member_declared_on_the_class_itself_is_suppressed_but_inherited_is_not() {
    // Suppression is gated on the declaration sitting on THIS class: `left` is
    // declared here and drops, while `right` / `.new` / `.[]` survive — the
    // inherited `::Data.new` must NOT count as the user's own, or the arity
    // false positive it causes stays.
    let cs = candidates_with_sig(
        "metaown",
        "Pair = Data.define(:left, :right)\n",
        "class Pair < ::Data\n  def left: () -> Integer\nend\n",
    );
    let names: Vec<&str> = cs.iter().map(|c| c.method_name.as_str()).collect();
    assert_eq!(names, vec!["right", "new", "[]"], "{cs:?}");
}

#[test]
fn plain_subclass_group_prints_its_written_ancestry() {
    // The print header carries the source's own superclass too (upstream
    // `declaration_header` reads the same per-file map the writer does).
    // v0.4.7: written rooted (`SuperclassSpelling.absolute`).
    let cs = candidates_tagged(
        "hdrsuper",
        "class Foo < StandardError\n  def m\n    \"m\"\n  end\nend\n",
        false,
    );
    assert_eq!(cs[0].decl_header, "class Foo < ::StandardError");
}

#[test]
fn a_class_whose_superclass_resolves_nowhere_is_dropped() {
    // Reference #735/#2478: `Bar` is neither RBS-known nor written by this
    // run, so `class Foo < ::Bar` would fail the next build — Foo is dropped.
    let cs = candidates_tagged("hdrdrop", "class Foo < Bar\n  def m = 1\nend\n", false);
    assert!(cs.is_empty(), "{cs:?}");
    // A project superclass this run writes keeps the subclass, and a
    // superclass whose only method is untyped is not written (#2478).
    let cs = candidates_tagged(
        "hdrkeep",
        "class Base\n  def b = 1\nend\nclass Kid < Base\n  def k = 2\nend\nclass U\n  def u = zork\nend\nclass V < U\n  def v = 3\nend\n",
        false,
    );
    let names: Vec<(&str, &str)> =
        cs.iter().map(|c| (c.class_name.as_str(), c.decl_header.as_str())).collect();
    assert_eq!(names, vec![("Base", "class Base"), ("Kid", "class Kid < ::Base")]);
}

#[test]
fn nested_same_name_superclass_and_rooted_header() {
    // #2661: `class B; class B < B` names the outer `::B`; a rooted
    // `class ::Box` inside `module A` declares the top-level `Box`.
    let cs = candidates_tagged(
        "hdrnest",
        "class B\n  class B < B\n    def x = 1\n  end\nend\nmodule A\n  class ::Box\n    def y = 2\n  end\nend\n",
        false,
    );
    let names: Vec<(&str, &str)> =
        cs.iter().map(|c| (c.class_name.as_str(), c.decl_header.as_str())).collect();
    assert_eq!(names, vec![("B::B", "class B::B < ::B"), ("Box", "class Box")]);
}

#[test]
fn qualify_source_name_walks_enclosing_scope_outward() {
    let mut fqns = std::collections::HashSet::new();
    fqns.insert("Rigor::Triage".to_string());
    fqns.insert("Rigor::Triage::Selector".to_string());
    fqns.insert("Top".to_string());
    // same-scope constant
    assert_eq!(qualify_source_name("Selector", "Rigor::Triage", &fqns), "Rigor::Triage::Selector");
    // outer sibling: `Triage` referenced from `Rigor::Sibling` → `Rigor::Triage`
    assert_eq!(qualify_source_name("Triage", "Rigor::Sibling", &fqns), "Rigor::Triage");
    // top-level self-reference
    assert_eq!(qualify_source_name("Top", "Top", &fqns), "Top");
    // unknown name is left bare (external / not in file)
    assert_eq!(qualify_source_name("Unknown", "Rigor::Triage", &fqns), "Unknown");
}

#[test]
fn bare_module_function_makes_subsequent_defs_dual() {
    // Position matters: a def BEFORE the bare call stays instance; after it,
    // the def is dual (`def self?.`). Applies in a CLASS body too.
    let src = "module U\n  def before\n    1\n  end\n  module_function\n  def after\n    2\n  end\nend\n";
    let cs = candidates_tagged("mfpos", src, false);
    assert_eq!(
        cs.iter().map(|c| c.rbs.as_str()).collect::<Vec<_>>(),
        vec!["def before: () -> 1", "def self?.after: () -> 2"]
    );
    // Kind stays `instance` — only the rbs prefix changes.
    assert!(cs.iter().all(|c| c.kind == "instance"));

    let clssrc = "class C\n  module_function\n  def helper\n    1\n  end\nend\n";
    let cc = candidates_tagged("mfcls", clssrc, false);
    assert_eq!(cc[0].rbs, "def self?.helper: () -> 1");
}

#[test]
fn module_function_dup_slots_last_definer_wins() {
    // rigor-rs#493 item 9 (reference `live_definitions`): a `module_function`
    // `def u` owns BOTH slots; a later `def self.u` steals the singleton one,
    // so the dual row is dropped outright — emitting both is an `rbs
    // validate` duplicate-member failure.
    let src = "module N15\n  module_function\n  def u = 1\n  def self.u = 2\nend\n";
    let cs = candidates_tagged("mfdup", src, false);
    assert_eq!(
        cs.iter().map(|c| c.rbs.as_str()).collect::<Vec<_>>(),
        vec!["def self.u: () -> 2"]
    );
    // A later INSTANCE-only redefinition (a `public` reset ends the dual
    // mode) steals the instance slot; the first def's surviving singleton
    // copy is written `def self.u` — with the FIRST def's return.
    let src2 = "module M\n  module_function\n  def u = 1\n  public\n  def u = 9\nend\n";
    let cs2 = candidates_tagged("mfdup2", src2, false);
    assert_eq!(
        cs2.iter().map(|c| c.rbs.as_str()).collect::<Vec<_>>(),
        vec!["def self.u: () -> 1", "def u: () -> 9"]
    );
    // A plain redefinition keeps the LAST def, as before.
    let src3 = "class C\n  def y = 1\n  def y = 2\nend\n";
    let cs3 = candidates_tagged("mfdup3", src3, false);
    assert_eq!(
        cs3.iter().map(|c| c.rbs.as_str()).collect::<Vec<_>>(),
        vec!["def y: () -> 2"]
    );
}

#[test]
fn module_function_def_survives_private_visibility() {
    // rigor-rs#493 item 10 (reference `visibility_excludes?` returns false for
    // a @module_function_methods def): `private` marks `h` private in the
    // visibility table, but the re-armed module_function still writes its
    // `self?.h` row.
    let src = "module T\n  module_function\n  private\n  def g = 1\n  module_function\n  def h = 2\nend\n";
    let cs = candidates_tagged("mfvis", src, false);
    assert_eq!(
        cs.iter().map(|c| c.rbs.as_str()).collect::<Vec<_>>(),
        vec!["def self?.h: () -> 2"]
    );
}

#[test]
fn module_function_def_survives_later_private_redefinition() {
    // rigor-rs#523: the name-keyed visibility table reads `u -> private` from
    // the SECOND def's mark, yet the first (module_function) def's surviving
    // singleton copy is still written `def self.u` with the FIRST def's return
    // (reference `visibility_excludes?` exempts @module_function_methods;
    // #526 added the guard, this pins the issue row). Same for `protected`.
    for vis in ["private", "protected"] {
        let src = format!("module M\n  module_function\n  def u = 1\n  {vis}\n  def u = 9\nend\n");
        let cs = candidates_tagged("mfpriv", &src, false);
        assert_eq!(
            cs.iter().map(|c| c.rbs.as_str()).collect::<Vec<_>>(),
            vec!["def self.u: () -> 1"],
            "{vis}"
        );
    }
    // Must-still-fire controls: a plain class / non-module_function module
    // keeps skipping the private redefinition.
    for src in [
        "class C\n  def u = 1\n  private\n  def u = 9\nend\n",
        "module M\n  def u = 1\n  private\n  def u = 9\nend\n",
    ] {
        assert!(candidates_tagged("mfpriv_ctl", src, false).is_empty(), "{src}");
    }
}

#[test]
fn module_function_with_args_does_not_flip_mode() {
    // `module_function :sym` (args form) neither flips the mode nor marks the
    // method (oracle-probed) — both defs stay plain instance methods.
    let src = "module N\n  def a\n    4\n  end\n  module_function :a\n  def b\n    5\n  end\nend\n";
    let cs = candidates_tagged("mfargs", src, false);
    assert_eq!(
        cs.iter().map(|c| c.rbs.as_str()).collect::<Vec<_>>(),
        vec!["def a: () -> 4", "def b: () -> 5"]
    );
}

#[test]
fn singleton_prefix_wins_over_module_function() {
    // reference `method_def_prefix` checks singleton FIRST.
    let src = "class C\n  module_function\n  def self.s\n    7\n  end\nend\n";
    let cs = candidates_tagged("mfsing", src, false);
    assert_eq!(cs[0].rbs, "def self.s: () -> 7");
    assert_eq!(cs[0].kind, "singleton");
}

#[test]
fn emits_singleton_methods_both_forms() {
    // `def self.x` and a `class << self` inner def both render `def self.NAME`.
    let src = "class A\n  def self.build\n    \"b\"\n  end\n  class << self\n    def via\n      :s\n    end\n  end\nend\n";
    let cs = candidates_tagged("sing", src, false);
    let rbs: Vec<&str> = cs.iter().map(|c| c.rbs.as_str()).collect();
    assert!(rbs.contains(&"def self.build: () -> \"b\""), "{rbs:?}");
    assert!(rbs.contains(&"def self.via: () -> :s"), "{rbs:?}");
    assert!(cs.iter().all(|c| c.kind == "singleton"));
}

#[test]
fn instance_and_singleton_emit_in_source_order() {
    // `def self.build` (line 2) precedes `def inst` (line 5): source order,
    // not instance-then-singleton.
    let src = "class A\n  def self.build\n    1\n  end\n  def inst\n    2\n  end\nend\n";
    let cs = candidates_tagged("order", src, false);
    assert_eq!(
        cs.iter().map(|c| c.method_name.as_str()).collect::<Vec<_>>(),
        vec!["build", "inst"]
    );
}

#[test]
fn untyped_inside_composite_member_skips() {
    // `[x, 0]` with x untyped erases `[untyped, 0]` — an inference hole the
    // reference reads differently (sweep-proven mismatch source) → skip.
    let src = "class A\n  def m(x)\n    [x, 0]\n  end\nend\n";
    assert!(candidates_tagged("untycomp", src, false).is_empty());
}

#[test]
fn diff_string_emits_header_and_plus_line_per_candidate() {
    let c = |cls: &str, m: &str, rbs: &str| Candidate {
        file: "lib/f.rb".into(),
        class_name: cls.into(),
        method_name: m.into(),
        kind: "instance",
        rbs: rbs.into(),
        inferred_return: String::new(),
        classification: "new_method",
        declared_return_rbs: None,
        decl_header: String::new(),
        skip_reason: None,
        definer: None,
    };
    let cands = [
        c("Foo", "greeting", "def greeting: () -> \"h\""),
        c("Foo", "build", "def self.build: () -> 42"),
    ];
    assert_eq!(
        diff_string(&cands),
        "--- lib/f.rb: Foo#greeting\n+ def greeting: () -> \"h\"\n\n\
         --- lib/f.rb: Foo#build\n+ def self.build: () -> 42\n\n"
    );
}

#[test]
fn mirror_target_maps_lib_to_sig() {
    let root = Path::new("/proj");
    assert_eq!(mirror_target("lib/foo.rb", "lib", "sig", root), PathBuf::from("/proj/sig/foo.rbs"));
    assert_eq!(
        mirror_target("lib/a/b.rb", "lib", "sig", root),
        PathBuf::from("/proj/sig/a/b.rbs")
    );
    // A path not under the source root keeps its full relative path.
    assert_eq!(mirror_target("app/x.rb", "lib", "sig", root), PathBuf::from("/proj/sig/app/x.rbs"));
}

#[test]
fn target_for_consults_layout_index_before_mirror() {
    let root = Path::new("/proj");
    let mut layout = LayoutIndex { map: HashMap::new() };
    layout.map.insert("Foo".to_string(), PathBuf::from("/proj/sig/consolidated.rbs"));
    // Class in the index → routed to the consolidated file.
    assert_eq!(
        target_for("lib/foo.rb", "Foo", "lib", "sig", root, &layout),
        PathBuf::from("/proj/sig/consolidated.rbs")
    );
    // Class NOT in the index → 1:1 mirror.
    assert_eq!(
        target_for("lib/bar.rb", "Bar", "lib", "sig", root, &layout),
        PathBuf::from("/proj/sig/bar.rbs")
    );
}

#[test]
fn render_new_file_wraps_nested_namespaces_with_kinds_and_super() {
    let cand = |class: &str, rbs: &str| Candidate {
        file: "lib/x.rb".into(),
        class_name: class.into(),
        method_name: "m".into(),
        kind: "instance",
        rbs: rbs.into(),
        inferred_return: String::new(),
        classification: "new_method",
        declared_return_rbs: None,
        decl_header: String::new(),
        skip_reason: None,
        definer: None,
    };
    let mut info = NamespaceInfo::default();
    info.kinds.insert("Outer".into(), "module");
    info.kinds.insert("Outer::Inner".into(), "class");
    info.supers.insert("Outer::Inner".into(), "Base".into());
    let out = render_new_file(&[cand("Outer::Inner", "def m: () -> :s")], &[], &info);
    assert_eq!(out, "module Outer\n  class Inner < ::Base\n    def m: () -> :s\n  end\nend\n");
}

#[test]
fn render_new_file_appends_data_struct_shells_after_real_children() {
    // Shells (empty `class`) append AFTER methods + real nested classes, in
    // the given (source) order — the reference `@class_shells` tree order.
    let cand = Candidate {
        file: "lib/x.rb".into(),
        class_name: "Host".into(),
        method_name: "m".into(),
        kind: "instance",
        rbs: "def m: () -> Host::A".into(),
        inferred_return: String::new(),
        classification: "new_method",
        declared_return_rbs: None,
        decl_header: String::new(),
        skip_reason: None,
        definer: None,
    };
    let mut info = NamespaceInfo::default();
    info.kinds.insert("Host".into(), "class");
    info.kinds.insert("Host::A".into(), "class");
    info.kinds.insert("Host::B".into(), "class");
    let out = render_new_file(
        &[cand],
        &["Host::A".to_string(), "Host::B".to_string()],
        &info,
    );
    assert_eq!(
        out,
        "class Host\n  def m: () -> Host::A\n  class A\n  end\n  class B\n  end\nend\n"
    );
}

#[test]
fn render_new_file_shell_only_target_creates_the_class() {
    // A file whose only sig-worthy content is a `Data.define` still gets a
    // shell class (a shell-only target).
    let mut info = NamespaceInfo::default();
    info.kinds.insert("Standalone".into(), "class");
    let out = render_new_file(&[], &["Standalone".to_string()], &info);
    assert_eq!(out, "class Standalone\nend\n");
}

#[test]
fn render_new_file_leaf_class_defaults_to_class_keyword() {
    let cand = Candidate {
        file: "lib/x.rb".into(),
        class_name: "Foo".into(),
        method_name: "g".into(),
        kind: "instance",
        rbs: "def g: () -> \"h\"".into(),
        inferred_return: String::new(),
        classification: "new_method",
        declared_return_rbs: None,
        decl_header: String::new(),
        skip_reason: None,
        definer: None,
    };
    // No kinds recorded → a leaf with methods defaults to `class`.
    let out = render_new_file(&[cand], &[], &NamespaceInfo::default());
    assert_eq!(out, "class Foo\n  def g: () -> \"h\"\nend\n");
}

#[test]
fn dynamic_return_member_skips_method() {
    // `return bar` (an unresolved call → Dynamic) poisons the union →
    // dynamic_top? → skip, matching the reference's untyped-return skip
    // (the Nominal#erase_to_rbs / DiffCommand#run over-emit fix).
    let src = "class A\n  def m(c)\n    return bar if c\n    \"tail\"\n  end\nend\n";
    assert!(candidates_tagged("dynret", src, false).is_empty());
}

#[test]
fn trivial_initialize_is_excluded() {
    // An all-empty-param initialize is EXCLUDED (Object#initialize covers it).
    let src = "class Foo\n  def initialize\n    @x = 1\n  end\nend\n";
    assert!(candidates_tagged("init0", src, false).is_empty());
}

#[test]
fn initialize_stub_renders_full_param_shape_as_void() {
    // Oracle-probed matrix: requireds/optionals/rest/keywords/kwrest/block →
    // the reference's `render_initialize_param_list` spelling, `-> void`.
    let cases = [
        ("class B\n  def initialize(a, b)\n    @a = a\n  end\nend\n", "def initialize: (untyped, untyped) -> void"),
        ("class C\n  def initialize(a, b = 1)\n    @a = a\n  end\nend\n", "def initialize: (untyped, ?untyped) -> void"),
        ("class D\n  def initialize(name:, age: 0)\n    @n = name\n  end\nend\n", "def initialize: (name: untyped, ?age: untyped) -> void"),
        ("class E\n  def initialize(*a, **o, &b)\n    @a = a\n  end\nend\n", "def initialize: (*untyped, **untyped) ?{ (*untyped) -> untyped } -> void"),
        ("class F\n  def initialize(a, b = 1, *r, c:, d: 2)\n    @a = a\n  end\nend\n", "def initialize: (untyped, ?untyped, *untyped, c: untyped, ?d: untyped) -> void"),
    ];
    for (i, (src, want)) in cases.iter().enumerate() {
        let cs = candidates_tagged(&format!("initm{i}"), src, false);
        assert_eq!(cs.len(), 1, "case {i}: {cs:?}");
        assert_eq!(&cs[0].rbs, want, "case {i}");
        assert_eq!(cs[0].kind, "instance");
    }
}

#[test]
fn def_self_initialize_is_an_ordinary_singleton() {
    // `def self.initialize` is NOT a constructor — a normal singleton method.
    let src = "class Foo\n  def self.initialize(a)\n    \"x\"\n  end\nend\n";
    let cs = candidates_tagged("initsing", src, false);
    assert_eq!(cs[0].rbs, "def self.initialize: (untyped) -> \"x\"");
}

#[test]
fn emits_sound_project_class_instance_return() {
    // `Bar.new` types as a source-class `Bar` instance (ADR-0023 tier-4). The
    // reference degrades a project-class `.new` to `Dynamic` and skips, but
    // rigor-rs emits the SOUND `-> Bar` — coverage excess we track, not encode
    // (AGENTS.md "Generative-tool parity"; the reference converges as it gains
    // project-instance return typing).
    let src = "class Bar\nend\n\nclass Foo\n  def make\n    Bar.new\n  end\nend\n";
    let cs = candidates_tagged("srccls", src, false);
    let make = cs.iter().find(|c| c.method_name == "make").expect("make emitted");
    assert_eq!(make.rbs, "def make: () -> Bar");
}

#[test]
fn folds_literal_tuple_map_and_still_skips_bare_generic_nominal() {
    // rigor-rs#194: `[1, 2, 3].map { |x| x }` keeps the per-position fold —
    // `Tuple[1, 2, 3]` erases to the record spelling the reference emits.
    let src = "class Foo\n  def mapped\n    [1, 2, 3].map { |x| x }\n  end\nend\n";
    let cs = candidates_tagged("folded", src, false);
    let mapped = cs.iter().find(|c| c.method_name == "mapped").expect("mapped emitted");
    assert_eq!(mapped.rbs, "def mapped: () -> [1, 2, 3]");

    // A non-tuple receiver keeps the pre-fold answer: `Array.new.map { }` is
    // the bare `Array` the reference would elaborate to `Array[untyped]`, so
    // rigor-rs skips it (FP-safe) rather than emit an under-elaborated
    // `-> Array`.
    let src = "class Foo\n  def mapped\n    Array.new.map { |x| x }\n  end\nend\n";
    assert!(candidates_tagged("bare", src, false).is_empty());
}

#[test]
fn is_bare_generic_name_only_matches_bare_generics() {
    assert!(is_bare_generic_name("Array"));
    assert!(is_bare_generic_name("Hash"));
    // Value-pinned / parameterised / scalar forms still emit.
    assert!(!is_bare_generic_name("Array[Integer]"));
    assert!(!is_bare_generic_name("[1, 2]"));
    assert!(!is_bare_generic_name("String"));
    assert!(!is_bare_generic_name("42"));
}

// -- UPDATE/merge + LayoutIndex ------------------------------------------

/// A merge-candidate factory for the `apply_merge` tests.
#[test]
fn group_by_class_orders_groups_by_class_name() {
    // Reference `merge_candidates`: `groups.keys.sort.each { merge_class }` —
    // class-name order, which is also the `skipped` report's line order
    // (rigor-rs#493 item 5).
    let groups = group_by_class(vec![
        mc("Zebra", "z", "instance", "def z: () -> 1"),
        mc("Apple", "a", "instance", "def a: () -> 2"),
        mc("Apple", "b", "instance", "def b: () -> 3"),
    ]);
    assert_eq!(
        groups.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        vec!["Apple", "Zebra"]
    );
}

fn mc(class: &str, method: &str, kind: &'static str, rbs: &str) -> Candidate {
    Candidate {
        file: "lib/f.rb".into(),
        class_name: class.into(),
        method_name: method.into(),
        kind,
        rbs: rbs.into(),
        inferred_return: String::new(),
        classification: "new_method",
        declared_return_rbs: None,
        decl_header: String::new(),
        skip_reason: None,
        definer: None,
    }
}

/// A `tighter_return` candidate (the classifier proved a strict subtype), the
/// only kind `--overwrite` replaces from generation classification.
fn mc_tighter(
    class: &str,
    method: &str,
    kind: &'static str,
    rbs: &str,
    declared: &str,
) -> Candidate {
    let mut c = mc(class, method, kind, rbs);
    c.classification = "tighter_return";
    c.declared_return_rbs = Some(declared.into());
    c
}

#[test]
fn member_pairs_cover_attr_writer_accessor_and_kind_rules_but_not_alias() {
    // attr_writer → `name=`; attr_accessor → both; alias never counts;
    // def kinds map instance / singleton / singleton_instance.
    let rbs = "class Foo\n  def a: () -> Integer\n  def self.s: () -> String\n  def self?.d: () -> bool\n  attr_reader r: String\n  attr_writer w: Integer\n  attr_accessor acc: String\n  alias al a\nend\n";
    let sig = ruby_rbs::node::parse(rbs).unwrap();
    let RbsNode::Class(c) = sig.declarations().iter().next().unwrap() else { panic!() };
    let pairs = collect_member_pairs(rbs, c.members().iter());
    let got: Vec<(String, &str)> =
        pairs.iter().map(|m| (m.name.clone(), m.kind)).collect();
    assert_eq!(
        got,
        vec![
            ("a".into(), "instance"),
            ("s".into(), "singleton"),
            ("d".into(), "singleton_instance"),
            ("r".into(), "instance"),
            ("w=".into(), "instance"),
            ("acc".into(), "instance"),
            ("acc=".into(), "instance"),
        ],
        "alias `al` must not appear; attr rules applied"
    );
}

#[test]
fn return_text_extraction_takes_last_depth_zero_arrow() {
    assert_eq!(extract_method_return_text("def m: () -> String"), Some("String".into()));
    // A block-typed param carries an inner `->` at depth > 0 — ignored.
    assert_eq!(
        extract_method_return_text("def m: () ?{ () -> void } -> Integer"),
        Some("Integer".into())
    );
    // Union return, wrapped.
    assert_eq!(
        extract_method_return_text("def m: (untyped) -> (\"s\" | 1)"),
        Some("(\"s\" | 1)".into())
    );
    // No arrow → extraction failure.
    assert_eq!(extract_method_return_text("attr_reader name: String"), None);
}

#[test]
fn member_pair_return_text_for_method_and_attr() {
    let rbs = "class Foo\n  def m: (untyped) -> Integer\n  attr_reader r: String?\nend\n";
    let sig = ruby_rbs::node::parse(rbs).unwrap();
    let RbsNode::Class(c) = sig.declarations().iter().next().unwrap() else { panic!() };
    let pairs = collect_member_pairs(rbs, c.members().iter());
    assert_eq!(pairs[0].return_text.as_deref(), Some("Integer"));
    assert_eq!(pairs[1].return_text.as_deref(), Some("String?"));
}

#[test]
fn merge_inserts_new_method_flat_before_end() {
    let src = "class Foo\n  def existing: () -> String\nend\n".to_string();
    let out =
        apply_merge(src, vec![mc("Foo", "newm", "instance", "def newm: () -> 1")], &HashMap::new(), false);
    assert_eq!(out.action, "updated");
    assert_eq!(out.applied.len(), 1);
    assert_eq!(
        out.source,
        "class Foo\n  def existing: () -> String\n  def newm: () -> 1\nend\n"
    );
}

#[test]
fn merge_inserts_new_method_nested_at_member_depth() {
    // `insert_into_class` at `member_indent_depth`: the inserted line renders
    // one level below the inner decl and its `end` keeps its own line
    // (oracle-verified; an earlier token-start splice stranded `end` at
    // column 0, rigor-rs#493 item 11 review).
    let src = "module Outer\n  class Inner\n    def existing: () -> String\n  end\nend\n"
        .to_string();
    let out = apply_merge(
        src,
        vec![mc("Outer::Inner", "newm", "instance", "def newm: () -> 5")],
        &HashMap::new(),
        false,
    );
    assert_eq!(
        out.source,
        "module Outer\n  class Inner\n    def existing: () -> String\n    def newm: () -> 5\n  end\nend\n"
    );
}

#[test]
fn merge_equivalent_conflict_skips_user_authored() {
    // A conflict reaching the writer is skipped whatever its text (reference
    // `merge_into_existing_class`): an equal return was already dropped as
    // equivalent at generation, so one arriving here is one generation could
    // not compare (#482). Not applied → noop, byte-untouched.
    let src = "class Foo\n  def greeting: () -> \"hi\"\nend\n".to_string();
    let out = apply_merge(
        src.clone(),
        vec![mc("Foo", "greeting", "instance", "def greeting: () -> \"hi\"")],
        &HashMap::new(),
        false,
    );
    assert_eq!(out.action, "noop");
    assert!(out.applied.is_empty());
    assert_eq!(out.skipped.len(), 1);
    assert_eq!(out.skipped[0].classification, "new_method");
    assert_eq!(out.source, src, "byte-untouched when nothing applies");
}

#[test]
fn merge_conflict_keeps_the_candidate_classification() {
    let src = "class Foo\n  def greeting: () -> String\nend\n".to_string();
    let out = apply_merge(
        src.clone(),
        vec![mc("Foo", "greeting", "instance", "def greeting: () -> \"hi\"")],
        &HashMap::new(),
        false,
    );
    assert_eq!(out.action, "noop"); // nothing applied
    assert_eq!(out.skipped.len(), 1);
    assert_eq!(out.skipped[0].classification, "new_method");
    assert_eq!(out.skipped[0].declared_return_rbs, None);
    assert_eq!(out.source, src);
}

#[test]
fn overwrite_replaces_tighter_conflict_in_place_and_applies_it() {
    // Under --overwrite a tighter_return conflict REPLACES the declared line,
    // moves to `applied`, and leaves 0 skipped (reference `apply_replacement`).
    let src = "class Foo\n  def greeting: () -> String\nend\n".to_string();
    let out = apply_merge(
        src,
        vec![mc_tighter("Foo", "greeting", "instance", "def greeting: () -> \"hi\"", "String")],
        &HashMap::new(),
        true,
    );
    assert_eq!(out.action, "updated");
    assert_eq!(out.applied.len(), 1);
    assert!(out.skipped.is_empty());
    assert_eq!(out.source, "class Foo\n  def greeting: () -> \"hi\"\nend\n");
}

#[test]
fn overwrite_replaces_multiple_tighter_conflicts_offsets_stay_valid() {
    // Replacements apply highest-offset-first so earlier spans stay valid even
    // as line lengths change; all three land byte-correctly.
    let src =
        "class Foo\n  def a: () -> String\n  def b: () -> String\n  def c: () -> String\nend\n"
            .to_string();
    let out = apply_merge(
        src,
        vec![
            mc_tighter("Foo", "a", "instance", "def a: () -> \"aa\"", "String"),
            mc_tighter("Foo", "b", "instance", "def b: () -> \"bb\"", "String"),
            mc_tighter("Foo", "c", "instance", "def c: () -> \"cc\"", "String"),
        ],
        &HashMap::new(),
        true,
    );
    assert_eq!(out.applied.len(), 3);
    assert_eq!(
        out.source,
        "class Foo\n  def a: () -> \"aa\"\n  def b: () -> \"bb\"\n  def c: () -> \"cc\"\nend\n"
    );
}

#[test]
fn overwrite_off_still_preserves_tighter_conflict_as_skipped() {
    // Without --overwrite the same candidate is preserved (byte-untouched).
    let src = "class Foo\n  def greeting: () -> String\nend\n".to_string();
    let out = apply_merge(
        src.clone(),
        vec![mc_tighter("Foo", "greeting", "instance", "def greeting: () -> \"hi\"", "String")],
        &HashMap::new(),
        false,
    );
    assert_eq!(out.action, "noop");
    assert_eq!(out.skipped.len(), 1);
    assert_eq!(out.source, src);
}

#[test]
fn overwrite_new_method_only_replaces_when_it_removes_an_untyped_slot() {
    // A new_method conflict is eligible ONLY when its RBS has strictly fewer
    // `untyped` tokens than the existing decl (reference `tightens_untyped?`).
    // Tightening: existing `(untyped) -> void`, candidate `(String) -> void`.
    let src = "class Foo\n  def initialize: (untyped) -> void\nend\n".to_string();
    let tighten = apply_merge(
        src.clone(),
        vec![mc("Foo", "initialize", "instance", "def initialize: (String) -> void")],
        &HashMap::new(),
        true,
    );
    assert_eq!(tighten.applied.len(), 1, "removes one untyped ⇒ replaced");
    assert_eq!(tighten.source, "class Foo\n  def initialize: (String) -> void\nend\n");

    // Not tightening: same untyped count ⇒ preserved, not replaced.
    let same = apply_merge(
        src.clone(),
        vec![mc("Foo", "initialize", "instance", "def initialize: (untyped) -> void")],
        &HashMap::new(),
        true,
    );
    assert_eq!(same.action, "noop", "equal untyped count + equal return ⇒ drop");
    assert_eq!(same.source, src);
}

#[test]
fn count_untyped_is_word_boundary_matched() {
    assert_eq!(count_untyped("(untyped, untyped) -> void"), 2);
    assert_eq!(count_untyped("() -> void"), 0);
    // `untyped` inside an identifier is not a type token.
    assert_eq!(count_untyped("(my_untyped_thing) -> untyped"), 1);
}

#[test]
fn merge_instance_and_singleton_are_distinct_identities() {
    // An existing INSTANCE `def build` does NOT block a SINGLETON candidate.
    let src = "class Foo\n  def build: () -> String\nend\n".to_string();
    let out = apply_merge(
        src,
        vec![mc("Foo", "build", "singleton", "def self.build: () -> 1")],
        &HashMap::new(),
        false,
    );
    assert_eq!(out.action, "updated");
    assert_eq!(
        out.source,
        "class Foo\n  def build: () -> String\n  def self.build: () -> 1\nend\n"
    );
}

#[test]
fn merge_attr_reader_blocks_matching_method_candidate() {
    let src = "class Foo\n  attr_reader name: String\nend\n".to_string();
    let out = apply_merge(
        src.clone(),
        vec![mc("Foo", "name", "instance", "def name: () -> \"n\"")],
        &HashMap::new(),
        false,
    );
    // `attr_reader name` declares `name`: the candidate is skipped as
    // user-authored, with its own classification.
    assert_eq!(out.skipped.len(), 1);
    assert_eq!(out.skipped[0].classification, "new_method");
    assert_eq!(out.source, src);
}

#[test]
fn append_new_class_compact_header_and_leading_blank() {
    let src = "class Foo\n  def existing: () -> String\nend\n".to_string();
    let mut supers = HashMap::new();
    supers.insert("Bar".to_string(), "Base".to_string());
    let out = apply_merge(
        src,
        vec![mc("Bar", "added", "instance", "def added: () -> 7")],
        &supers,
        false,
    );
    assert_eq!(out.action, "updated");
    assert_eq!(
        out.source,
        "class Foo\n  def existing: () -> String\nend\n\nclass Bar < ::Base\n  def added: () -> 7\nend\n"
    );
}

#[test]
fn append_new_class_qualified_name_nests_the_missing_chain() {
    // Reference `insert_namespace_chain`: with no declared ancestor the
    // whole chain renders NESTED, not as a flat `class A::B` header
    // (rigor-rs#493 item 11).
    let src = "class Foo\nend\n".to_string();
    let out = apply_merge(
        src,
        vec![mc("A::B", "m", "instance", "def m: () -> 1")],
        &HashMap::new(),
        false,
    );
    assert_eq!(
        out.source,
        "class Foo\nend\n\nmodule A\n  class B\n    def m: () -> 1\n  end\nend\n"
    );
}

#[test]
fn insert_into_existing_ancestor_nests_the_leaf() {
    // `module A` already declared: `A::Base`/`A::Sub2` merge INSIDE it, at
    // member depth (rigor-rs#493 item 11 — the reported FP).
    let src = "module A\nend\n".to_string();
    let mut supers = HashMap::new();
    supers.insert("A::Sub2".to_string(), "::A::Base".to_string());
    let mut kinds: HashMap<String, &'static str> = HashMap::new();
    kinds.insert("A".to_string(), "module");
    kinds.insert("A::Sub2".to_string(), "class");
    let out = apply_merge_with(
        src,
        vec![
            mc("A::Base", "b", "instance", "def b: () -> 1"),
            mc("A::Sub2", "m", "instance", "def m: () -> 1"),
        ],
        &kinds,
        &supers,
        &HashMap::new(),
        &[],
        false,
    );
    assert_eq!(
        out.source,
        "module A\n  class Base\n    def b: () -> 1\n  end\n  class Sub2 < ::A::Base\n    def m: () -> 1\n  end\nend\n"
    );
}

#[test]
fn append_repairs_missing_trailing_newline() {
    let src = "class Foo\nend".to_string(); // no trailing newline
    let out = apply_merge(
        src,
        vec![mc("Bar", "m", "instance", "def m: () -> 1")],
        &HashMap::new(),
        false,
    );
    assert_eq!(out.source, "class Foo\nend\n\nclass Bar\n  def m: () -> 1\nend\n");
}

#[test]
fn malformed_target_is_noop_and_untouched() {
    let src = "class Foo\n  def existing: (( -> \nend\n".to_string();
    let out = apply_merge(
        src.clone(),
        vec![mc("Foo", "newm", "instance", "def newm: () -> 1")],
        &HashMap::new(),
        false,
    );
    assert_eq!(out.action, "noop");
    assert!(out.applied.is_empty() && out.skipped.is_empty());
    assert_eq!(out.source, src);
}

#[test]
fn layout_index_first_found_wins_and_skips_parse_failures() {
    let dir = std::env::temp_dir().join(format!("rigor_layout_{}", std::process::id()));
    let sig = dir.join("sig");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(sig.join("nested")).unwrap();
    // Two files declare Foo; sorted walk means `a.rbs` (< `z.rbs`) wins.
    std::fs::write(sig.join("a.rbs"), "class Foo\nend\n").unwrap();
    std::fs::write(sig.join("z.rbs"), "class Foo\nend\nclass Bar\nend\n").unwrap();
    // A nested, consolidated declaration is indexed by its FQN.
    std::fs::write(sig.join("nested/x.rbs"), "module M\n  class Inner\n  end\nend\n").unwrap();
    // A malformed file is skipped silently (its Baz never appears).
    std::fs::write(sig.join("broken.rbs"), "class Baz (( bad\n").unwrap();

    let layout = LayoutIndex::build(&["sig".to_string()], &dir);
    assert_eq!(layout.file_for("Foo"), Some(&sig.join("a.rbs")));
    assert_eq!(layout.file_for("Bar"), Some(&sig.join("z.rbs")));
    assert_eq!(layout.file_for("M::Inner"), Some(&sig.join("nested/x.rbs")));
    assert_eq!(layout.file_for("Baz"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn generic_reopen_headers_and_core_subclass_shell() {
    // #2436/#2473: a core generic class is reopened with its declared
    // parameters; #2224/#735: a core subclass is a shell with `untyped` args.
    let dir = std::env::temp_dir().join(format!("rigor_siggen_generic_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("t.rb");
    std::fs::write(&file, "class Hash\n  def hk = :k\nend\nclass MyHash < Hash; end\nclass M\n  def m = 1\nend\n")
        .unwrap();
    let env = SigEnv::build(&[]);
    let Generated { candidates: cs, info, .. } =
        generate_run(&[file.to_str().unwrap().to_string()], false, &env, &writes);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(cs[0].decl_header, "class Hash[unchecked out K, unchecked out V]");
    assert!(info.shells.contains(&"MyHash".to_string()));
    assert_eq!(info.supers.get("MyHash").map(String::as_str), Some("Hash[untyped, untyped]"));
}

#[test]
fn merge_repairs_a_bare_generic_header_unless_a_body_names_a_parameter() {
    // #2580/#2602: the bare `class Array` gets the run's spelling.
    let params: HashMap<String, String> =
        [("Array".to_string(), "[unchecked out E]".to_string())].into_iter().collect();
    let out = apply_merge_with(
        "class Array\n  def old: () -> Integer\nend\n".to_string(),
        vec![mc("Array", "second_elem", "instance", "def second_elem: () -> 1")],
        &HashMap::new(),
        &HashMap::new(),
        &params,
        &[],
        false,
    );
    assert_eq!(out.source, "class Array[unchecked out E]\n  def old: () -> Integer\n  def second_elem: () -> 1\nend\n");
    assert_eq!(out.repaired, vec!["Array[unchecked out E]".to_string()]);
    // A body naming `E` would bind to the parameter: left bare.
    let out = apply_merge_with(
        "class Array\n  def old: () -> E\nend\n".to_string(),
        Vec::new(),
        &HashMap::new(),
        &HashMap::new(),
        &params,
        &[],
        false,
    );
    assert!(out.repaired.is_empty());
    assert_eq!(out.action, "noop");
}

/// The skip reasons of a run, in candidate order (#472: the stderr summary).
fn skips_with_sig(tag: &str, rb_src: &str, rbs_src: &str) -> (Vec<Candidate>, Vec<&'static str>) {
    let dir = std::env::temp_dir().join(format!("rigor_siggen_skips_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::create_dir_all(dir.join("sig")).unwrap();
    let rb = dir.join("lib/foo.rb");
    std::fs::write(&rb, rb_src).unwrap();
    std::fs::write(dir.join("sig/foo.rbs"), rbs_src).unwrap();
    let env = SigEnv::build(&[dir.join("sig")]);
    let Generated { candidates, skips, .. } =
        generate_run(&[rb.to_str().unwrap().to_string()], false, &env, &writes);
    let _ = std::fs::remove_dir_all(&dir);
    (candidates, skips)
}

#[test]
fn skip_reasons_follow_candidate_order() {
    // Oracle (v0.4.7): `(sig.skipped.overridden-by-unsigned-subclass: 1,
    // sig.skipped.untyped-return: 1, sig.skipped.uncertain-module-function: 1,
    // sig.skipped.top-level-def: 1)`.
    let src = "class Base\n  def name = 1\n  def other = 2\nend\n\
               class Sub < Base\n  def name(x) = x\nend\n\
               module MF\n  module_function\n  def a = 1\n  public\n  def b = 2\nend\n\
               module MF2\n  module_function\n  if rand > 0.5\n    private\n  end\n  def c = 3\nend\n\
               if true\n  def tl = 1\nend\n\
               foo do\n  def inblk = 1\nend\n\
               def self.sx = 1\n";
    let (cs, skips) = skips_with_sig("order", src, "");
    let emitted: Vec<String> =
        cs.iter().map(|c| format!("{}#{} {}", c.class_name, c.method_name, c.rbs)).collect();
    assert_eq!(
        emitted,
        vec![
            "Base#other def other: () -> 2",
            "MF#a def self?.a: () -> 1",
            "MF#b def b: () -> 2",
        ]
    );
    assert_eq!(
        skips,
        vec![
            "sig.skipped.overridden-by-unsigned-subclass",
            "sig.skipped.untyped-return",
            "sig.skipped.uncertain-module-function",
            "sig.skipped.top-level-def",
        ]
    );
}

#[test]
fn module_function_reset_readings() {
    // Each module's def reads: a direct reset ends the mode (`def b`), one in
    // `begin` ends it too but the visibility table keeps the `private` the
    // bare call set (dropped), one in control flow or a block makes the next
    // def uncertain, and `*_eval` / `Class.new do` blocks are another scope.
    let src = "module A\n  module_function\n  self.private\n  def a = 1\nend\n\
               module B\n  module_function\n  begin\n    private\n  end\n  def b = 1\nend\n\
               module C\n  module_function\n  [1].each { private }\n  def c = 1\n  module_function\n  def c2 = 2\nend\n\
               module D\n  module_function\n  String.class_eval { private }\n  def d = 1\nend\n\
               module E\n  module_function\n  X = Class.new(Object) do\n    private\n  end\n  def e = 1\nend\n\
               module F\n  module_function\n  case y\n  when 1 then private\n  end\n  private def f = 1\nend\n";
    let (cs, skips) = skips_with_sig("mf", src, "");
    let emitted: Vec<&str> = cs.iter().map(|c| c.rbs.as_str()).collect();
    assert_eq!(emitted, vec!["def self?.c2: () -> 2", "def self?.d: () -> 1", "def self?.e: () -> 1"]);
    assert_eq!(skips, vec!["sig.skipped.uncertain-module-function"; 2]);
}

#[test]
fn bare_core_reopening_reads_every_method_as_new() {
    // #482: `class Hash` without parameters fails Hash's definition build in
    // the reference (GenericParameterMismatchError), so a member `sig/`
    // already declares is NOT dropped as equivalent: it reaches the writer,
    // which skips it as user-authored.
    let (cs, _) = skips_with_sig(
        "bare_hash",
        "class Hash\n  def hfoo = 1\n  def hbar = 2\nend\nclass Foo\n  def ffoo = 1\nend\n",
        "class Hash\n  def hfoo: () -> 1\nend\nclass Foo\n  def ffoo: () -> 1\nend\n",
    );
    let emitted: Vec<String> = cs.iter().map(|c| format!("{}#{}", c.class_name, c.method_name)).collect();
    assert_eq!(emitted, vec!["Hash#hfoo", "Hash#hbar"], "Foo#ffoo stays equivalent");
    let src = "# c\nclass Hash\n  def hfoo: () -> 1\nend\n".to_string();
    let mut params = HashMap::new();
    params.insert("Hash".to_string(), "[unchecked out K, unchecked out V]".to_string());
    let out = apply_merge_with(src, cs, &HashMap::new(), &HashMap::new(), &params, &[], false);
    assert_eq!(out.skipped.len(), 1);
    assert_eq!(out.skipped[0].candidate.method_name, "hfoo");
    assert_eq!(out.repaired, vec!["Hash[unchecked out K, unchecked out V]".to_string()]);
    assert_eq!(
        out.source,
        "# c\nclass Hash[unchecked out K, unchecked out V]\n  def hfoo: () -> 1\n  def hbar: () -> 2\nend\n"
    );
}

/// `method name -> emitted rbs` for every def in `body` the port EMITS, under
/// `class K` (a declined method has no entry).
fn def_rows(tag: &str, body: &str) -> HashMap<String, String> {
    let src = format!("class K\n{body}\nend\n");
    candidates_tagged(tag, &src, false)
        .into_iter()
        .map(|c| (c.method_name, c.rbs))
        .collect()
}

#[test]
fn def_local_env_binds_straight_line_writes() {
    // rigor-rs#546 brief rows — byte-identical to the reference
    // (`sig-gen --print`, oracle-probed): a method-local read in the tail binds
    // the literal it was written with; branches join (an unrun arm adds the
    // entry binding, `nil` when the name was unbound).
    let rows = def_rows(
        "def_local_brief",
        "def z; a = 1; a; end\n\
         def j1(c); x = if c; 1; end; x; end\n\
         def j2(c); a = 1; if c; a = \"s\"; end; a; end\n\
         def j3(c); a = 1; unless c; a = :s; end; a; end\n\
         def j4(c); a = c ? 1 : 2; a; end\n\
         def j6(c); a = 1; a = \"s\" if c; a; end\n\
         def j9(c); a = 1; while c; a = \"s\"; end; a; end\n\
         def j11(c); a = 1; return a if c; a = 2; a; end\n\
         def j15(c); a = :initial; if c; return a; end; a = 100; a; end\n\
         def q5(c); a = \"x\"; a = a.upcase; a; end\n\
         def q6(c); r = []; r << 1; r = [2]; r; end",
    );
    assert_eq!(rows["z"], "def z: () -> 1");
    assert_eq!(rows["j1"], "def j1: (untyped) -> (1 | nil)");
    assert_eq!(rows["j2"], "def j2: (untyped) -> (\"s\" | 1)");
    assert_eq!(rows["j3"], "def j3: (untyped) -> (1 | :s)");
    assert_eq!(rows["j4"], "def j4: (untyped) -> (1 | 2)");
    assert_eq!(rows["j6"], "def j6: (untyped) -> (\"s\" | 1)");
    assert_eq!(rows["j9"], "def j9: (untyped) -> (\"s\" | 1)");
    assert_eq!(rows["j11"], "def j11: (untyped) -> (1 | 2)");
    assert_eq!(rows["j15"], "def j15: (untyped) -> (100 | :initial)");
    assert_eq!(rows["q5"], "def q5: (untyped) -> \"X\"");
    // A fresh literal after the mutation is a fresh carrier again.
    assert_eq!(rows["q6"], "def q6: (untyped) -> [2]");
}

#[test]
fn def_local_env_declines_a_literal_the_body_mutates() {
    // rigor-rs#546 review: the reference WIDENS a literal-bound local after an
    // in-place mutation (`Array[untyped]`, `String`, `Hash[untyped, untyped]`);
    // the port declines it (the stale `[]` / `"abc"` would be an unsound
    // signature — master skipped every one). A coverage loss, never a wrong
    // type. Every row below is a method the reference emits with the widened
    // type (`Array[untyped]`, `String`, `Hash[untyped, untyped]`, ...).
    let rows = def_rows(
        "def_local_mutated",
        "def c1(items); r = []; items.each { |i| r << i }; r; end\n\
         def c2(items); r = []; r << 1; r; end\n\
         def c3(items); s = \"\"; s << \"a\"; s; end\n\
         def c5(items); h = {}; h[:a] = 1; h; end\n\
         def c6(items); s = \"abc\"; s.upcase!; s; end\n\
         def c7(items); s = \"x\"; s.concat(\"d\"); s; end\n\
         def c8(items); a = [1]; a.push(2); a; end\n\
         def c9(items); a = [1]; a.map!(&:to_s); a; end\n\
         def c10(items); s = \"x\"; s[0] = \"y\"; s; end\n\
         def c13(items); s = \"x\"; items.each { |i| s << i }; s; end\n\
         def c14(items); a = [1, 2]; a.clear; a; end\n\
         def c15(items); h = {a: 1}; h.delete(:a); h; end\n\
         def c16(items); s = \"x\"; s.replace(\"yy\"); s; end\n\
         def e10(c); a = [[1]]; a[0] << 2; a; end\n\
         def e11(c); a = [1]; l = -> { a << 2 }; l.call; a; end\n\
         def e12(c); h = {a: 1}; h[:a] += 1; h; end\n\
         def e13(c); h = {a: 1}; h[:b] ||= 2; h; end\n\
         def e17(c); a = [1]; a.push(2) if c; a; end\n\
         def e18(c); a = [1]; while c; a << 2; end; a; end\n\
         def e32(c); a = [1]; a.concat([2]).size; a; end\n\
         def e34(c); a = [1]; a.tap { |t| t << 2 }; a; end\n\
         def e36(c); a = [1]; a&.push(2); a; end",
    );
    assert!(rows.is_empty(), "every mutated local must decline: {rows:?}");
}

#[test]
fn def_local_env_keeps_what_a_call_cannot_invalidate() {
    // Controls that still match the reference byte for byte: a non-mutating
    // call on the local, an alias the reference does not widen either, an
    // immutable scalar, and a bare `String` nominal.
    let rows = def_rows(
        "def_local_controls",
        "def c11(items); s = String.new; s << \"a\"; s; end\n\
         def e3(c); a = [1]; a.each { |x| x }; a; end\n\
         def e5(c); s = \"a\"; t = s.upcase; s; end\n\
         def e6(c); a = [1]; a.freeze; a; end\n\
         def e7(c); a = [1]; x = a.size; a; end\n\
         def e8(c); a = [1]; b = a.map { |i| i }; a; end\n\
         def e28(c); a = [1]; a.first; a; end\n\
         def n1(c); x = 1; x.succ!; x; end",
    );
    assert_eq!(rows["c11"], "def c11: (untyped) -> String");
    for name in ["e3", "e6", "e7", "e8", "e28"] {
        assert_eq!(rows[name], format!("def {name}: (untyped) -> [1]"), "{name}");
    }
    assert_eq!(rows["e5"], "def e5: (untyped) -> \"a\"");
}

#[test]
fn def_local_env_declines_a_local_handed_to_a_callee() {
    // rigor-rs#546 review 2: the reference's callee-mutated-parameter
    // analysis (`StatementEvaluator#argument_mutating_callees`) widens a
    // local passed to a project def (self-call, `Const.m`, `Const.new.m`),
    // to a lambda / proc / `method(:x)` run through `call` / `.()`, and to
    // a `send` hand-off (`Array[untyped]`, `String`, `Hash[untyped,
    // untyped]`, `Integer` for `r.size`). The port does not resolve callees:
    // a stale-able literal local passed as an argument (positional, keyword
    // value, splat, `&blk`) to any call declines. The reference emits the
    // widened type for every `r*` / `a1`, `a3`..`a8`-style row (probed); on
    // `a2` it emits `Array[untyped] | [1]`, and on `a9` (splat), `b4`
    // (`i[a]`), `b7` (undefined `foo`) and `b8` (unknown receiver) it keeps
    // `[1]` — those four are disclosed over-declines (a coverage loss).
    let src = "class K\n\
        class Helper\n\
          def self.fill(acc); acc << 1; end\n\
          def add(acc); acc << 1; end\n\
        end\n\
        def collect(acc, n); acc << n; end\n\
        def kw(a:, b: 1); a << b; end\n\
        def r1(items); r = []; collect(r, 1); r; end\n\
        def r2(items); r = []; items.each { |i| collect(r, i) }; r; end\n\
        def r3(items); r = []; Helper.fill(r); r; end\n\
        def r4(items); r = []; Helper.new.add(r); r; end\n\
        def r5(items); r = \"\"; collect(r, 1); r; end\n\
        def r6(items); r = {}; collect(r, 1); r; end\n\
        def r7(items); r = [1]; collect(r, 1); r.size; end\n\
        def r8(items); a = [1]; helper = ->(z) { z << 2 }; helper.(a); a; end\n\
        def r9(items); r = []; pr = proc { |x| x << 1 }; pr.call(r); r; end\n\
        def r10(items); r = []; send(:collect, r, 1); r; end\n\
        def a1(i); a = [1]; b = a; collect(b, 1); b; end\n\
        def a2(i); a = [1]; collect(a, 1) if i; a; end\n\
        def a3(i); a = [1]; kw(a: a); a; end\n\
        def a4(i); a = [1]; collect(a, &:x); a; end\n\
        def a5(i); a = [1]; method(:collect).call(a, 1); a; end\n\
        def a6(i); a = [1]; i.each { collect(a, 1) }; a; end\n\
        def a7(i); a = [1]; self.collect(a, 1); a; end\n\
        def a8(i); a = [1]; kw(a:); a; end\n\
        def a9(i); a = [1]; collect(*a); a; end\n\
        def b1(i); a = [1]; l = ->(x) { x }; l.call(a); a; end\n\
        def b2(i); a = [1]; i.call(a); a; end\n\
        def b3(i); a = [1]; i.(a); a; end\n\
        def b4(i); a = [1]; i[a]; a; end\n\
        def b5(i); a = [1]; send(:collect, a, 1); a; end\n\
        def b6(i); a = [1]; public_send(:collect, a, 1); a; end\n\
        def b7(i); a = [1]; foo(a); a; end\n\
        def b8(i); a = [1]; i.m(a); a; end\n\
      end\n";
    let rows: HashMap<String, String> = candidates_tagged("def_local_callee_args", src, false)
        .into_iter()
        .map(|c| (c.method_name, c.rbs))
        .collect();
    for n in [
        "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10", "a1", "a2", "a3", "a4", "a5",
        "a6", "a7", "a8", "a9", "b1", "b2", "b3", "b4", "b5", "b6", "b7", "b8",
    ] {
        assert!(!rows.contains_key(n), "{n} must decline: {rows:?}");
    }
}

#[test]
fn def_local_env_keeps_what_no_callee_receives() {
    // Controls that still match the reference byte for byte: the local is a
    // receiver (`a.size`), an operand of an operator, a container element
    // (`[a, 1]`, `{k: a}`), interpolated, or the argument of an immutable
    // type (`Integer`); a nested call is judged on its own arguments.
    let rows = def_rows(
        "def_local_callee_controls",
        "def collect(acc, n = 0); acc << n; end\n\
         def k1(i); a = [1]; x = a.size; a; end\n\
         def k2(i); a = [1]; x = [1] + a; a; end\n\
         def k3(i); a = [1]; x = [a, 1]; a; end\n\
         def k4(i); a = [1]; x = {k: a}; a; end\n\
         def k5(i); a = [1]; x = \"#{a}\"; a; end\n\
         def k6(i); a = [1]; x = a == a; a; end\n\
         def k7(i); n = 1; collect(n); n; end\n\
         def k8(i); a = [1]; collect(a.dup); a; end\n\
         def k9(i); a = [1]; collect(a.size); a; end\n\
         def k10(i); a = [1]; collect(i, a.first); a; end\n\
         def k11(i); a = [1]; b = [2]; collect(b); a; end",
    );
    for n in ["k1", "k2", "k3", "k4", "k5", "k6", "k8", "k9", "k10", "k11"] {
        assert_eq!(rows[n], format!("def {n}: (untyped) -> [1]"), "{n}");
    }
    assert_eq!(rows["k7"], "def k7: (untyped) -> 1");
}

#[test]
fn def_local_env_declines_a_loop_the_reference_folds() {
    // The reference folds a loop predicate over constant-bound locals
    // (`while i < 3` over `i = 0` is `bot`; `while i > 3` never enters and
    // keeps `i`); the zero-or-more join would emit `("s" | 0)`. A bare literal
    // predicate is not folded there, so the join stays. A `return` in a loop
    // body that reads a name the loop later writes or mutates is typed against
    // the loop-carried binding — declined, not the entry literal.
    let rows = def_rows(
        "def_local_loops",
        "def m14; i = 0; while i < 3; i = \"s\"; end; i; end\n\
         def w4(c); i = 0; while i > 3; i = \"s\"; end; i; end\n\
         def w2(c); a = 1; while true; a = \"s\"; end; a; end\n\
         def l1(c); a = 1; while c; return a if c.x; a = \"s\"; end; nil; end\n\
         def l2(c); a = [1]; while c; return a if c.x; a << 2; end; nil; end\n\
         def l4(c); a = 1; while c; return a if c.x; end; nil; end\n\
         def d1(c); x = 1; l = -> { x = \"s\" }; l.call; x; end",
    );
    for name in ["m14", "w4", "l1", "l2", "d1"] {
        assert!(!rows.contains_key(name), "{name} must decline: {rows:?}");
    }
    assert_eq!(rows["w2"], "def w2: (untyped) -> (\"s\" | 1)");
    assert_eq!(rows["l4"], "def l4: (untyped) -> (1 | nil)");
}

#[test]
fn def_local_env_parameter_shadows_a_file_level_local() {
    // A parameter is a fresh binding: `x = "top"; def b2(x); x; end` reads the
    // argument, not the file-level `"top"` (the reference skips it; pre-#546
    // the port leaked the file env into the def body).
    let cs = candidates_tagged(
        "def_local_param_shadow",
        "x = \"top\"\nclass K\n  def b2(x); x; end\n  def b3(y); x; end\nend\n",
        false,
    );
    assert!(cs.is_empty(), "{cs:?}");
}

#[test]
fn def_local_env_declined_override_demotes_the_base() {
    // A tail the def-local env declines types as `untyped` — the method skips as
    // an UNTYPED RETURN (a counted reason), not a silent port-only drop — so the
    // base-method demotion still reads the override as unsigned. (`for` is
    // rigor-rs#546 review 3 (B): the reference skips `for x in w; x.go; end`
    // too, and with it the base `A#e` — oracle: no candidates at all.)
    let src = "class A\n  def e(w); true; end\n  def f(w); 2; end\nend\nclass S < A\n  def e(w); for x in w; x.go; end; end\n  def f(w); a = 1; raise \"x\" if a.nil?; w.go; end\nend\n";
    let cs = candidates_tagged("def_local_override", src, false);
    assert!(cs.is_empty(), "{cs:?}");
}

#[test]
fn def_local_env_ensure_is_not_the_tail() {
    // rigor-rs#546 review 3 (A): an `ensure` body's value is discarded — the
    // method returns the protected body's tail; the port typed the ensure's last
    // statement (`def e3(v); 5; ensure; @m = false; end` → `false`). A tail that
    // reads an unmodelled protected body declines.
    // Rows marked `match` are byte-identical to the reference's `sig-gen --print`
    // (oracle-probed); rows marked `decline` the reference emits (or skips) and the
    // port declines — a coverage loss, never a wrong signature.
    let rows = def_rows(
        "def_local_env_ensure_is_not_the_tail",
        r##"
def e3(v); 5; ensure; @m = false; end
def e6(v); begin; 5; ensure; @m = false; end; end
def e1; @m = true; yield; ensure; @m = false; end
def e2(v); v.foo; ensure; @m = false; end
def e4(v); begin; 1; ensure; return 2; end; end
def e5(v); begin; begin; 1; ensure; 2; end; ensure; 3; end; end
def e7(v); a = 1; begin; a = 2; ensure; a = 3; end; a; end
def e8(c); a = 1; begin; a = 2; c.x; ensure; a; end; end
def e9(c); 5; rescue; 6; ensure; 7; end
def mute(v); @mute, old = true, @mute; v.go; ensure; @mute = old; end
def use(v); @in_use = true; yield v; ensure; @in_use = false; end
"##,
    );
    assert_eq!(rows["e3"], r##"def e3: (untyped) -> 5"##);
    assert_eq!(rows["e6"], r##"def e6: (untyped) -> 5"##);
    assert!(!rows.contains_key("e1"), "e1 must decline: {rows:?}");
    assert!(!rows.contains_key("e2"), "e2 must decline: {rows:?}");
    assert_eq!(rows["e4"], r##"def e4: (untyped) -> (1 | 2)"##);
    assert_eq!(rows["e5"], r##"def e5: (untyped) -> 1"##);
    assert_eq!(rows["e7"], r##"def e7: (untyped) -> 3"##);
    assert!(!rows.contains_key("e8"), "e8 must decline: {rows:?}");
    assert_eq!(rows["e9"], r##"def e9: (untyped) -> (5 | 6)"##);
    assert!(!rows.contains_key("mute"), "mute must decline: {rows:?}");
    assert!(!rows.contains_key("use"), "use must decline: {rows:?}");
}

#[test]
fn def_local_env_loop_tail_values() {
    // rigor-rs#546 review 3 (B): a loop tail is `nil` only for a plain
    // `while`/`until` — `break v` joins its value, `for` returns its collection.
    // The port typed every loop `nil`.
    // Rows marked `match` are byte-identical to the reference's `sig-gen --print`
    // (oracle-probed); rows marked `decline` the reference emits (or skips) and the
    // port declines — a coverage loss, never a wrong signature.
    let rows = def_rows(
        "def_local_env_loop_tail_values",
        r##"
def w0(v); while v; v.x; end; end
def w3(v); until v; v.x; end; end
def w9(v); while v; [1].each { |x| break }; end; end
def w1(v); while v; break 1; end; end
def w2(v); i = 0; while i < 3; i += 1; break :x if v; end; end
def w4(v); until v; break "s"; end; end
def w5(v); while v; break; end; end
def w8(v); a = [1]; for x in a; x; end; end
def w6(v); for x in [1, 2]; end; end
def w7(v); for x in 1..3; x; end; end
def w10(v); if v; for x in [1]; end; else; 2; end; end
def p1(v); for x in v do; x; end; end
def w11(v); begin; v.x; end while v; end
def w12(v); v.x while v; end
def dfs(v); v.each { |i| i }; for i in v; i.x; end; end
"##,
    );
    assert_eq!(rows["w0"], r##"def w0: (untyped) -> nil"##);
    assert_eq!(rows["w3"], r##"def w3: (untyped) -> nil"##);
    assert!(!rows.contains_key("w9"), "w9 must decline: {rows:?}");
    assert!(!rows.contains_key("w1"), "w1 must decline: {rows:?}");
    assert!(!rows.contains_key("w2"), "w2 must decline: {rows:?}");
    assert!(!rows.contains_key("w4"), "w4 must decline: {rows:?}");
    assert!(!rows.contains_key("w5"), "w5 must decline: {rows:?}");
    assert!(!rows.contains_key("w8"), "w8 must decline: {rows:?}");
    assert!(!rows.contains_key("w6"), "w6 must decline: {rows:?}");
    assert!(!rows.contains_key("w7"), "w7 must decline: {rows:?}");
    assert!(!rows.contains_key("w10"), "w10 must decline: {rows:?}");
    assert!(!rows.contains_key("p1"), "p1 must decline: {rows:?}");
    assert!(!rows.contains_key("w11"), "w11 must decline: {rows:?}");
    assert_eq!(rows["w12"], r##"def w12: (untyped) -> nil"##);
    assert!(!rows.contains_key("dfs"), "dfs must decline: {rows:?}");
}

#[test]
fn def_local_env_op_write_and_else_tails() {
    // A tail `a += 1` keeps the operand, not the operator (the reference folds `a
    // + 1`); a `begin` with an `else` returns the else's value.
    // Rows marked `match` are byte-identical to the reference's `sig-gen --print`
    // (oracle-probed); rows marked `decline` the reference emits (or skips) and the
    // port declines — a coverage loss, never a wrong signature.
    let rows = def_rows(
        "def_local_env_op_write_and_else_tails",
        r##"
def o1(c); a = 1; a += 1; end
def o2(c); a = 1; a -= 1; end
def o3(c); a = 1; a += 1; a; end
def x1(c); begin; 1; rescue; 2; else; 3; end; end
def x2(c); begin; 1; rescue; 2; else; end; end
def x3(c); begin; 1; rescue; 2; end; end
def x4(c); a = 1; begin; c.x; rescue; a = 2; end; a; end
def x5(c); a = 1; begin; a = 2; c.x; rescue; end; a; end
def x6(c); a = 1; begin; a = 2; c.x; rescue; a; end; end
def x7(c); a = 1; begin; c.x; rescue; a; end; end
def x8(c); begin; c.x; rescue => e; e; end; end
def x9(c); n = 0; begin; n += 1; c.x; 2; rescue; retry if n < 3; 3; end; end
def x10(c); a = 1; begin; c.x; rescue; 0; else; a = 2; end; a; end
"##,
    );
    assert!(!rows.contains_key("o1"), "o1 must decline: {rows:?}");
    assert!(!rows.contains_key("o2"), "o2 must decline: {rows:?}");
    // `a = 1; a += 1; a` folds to `2` (rigor-rs#548, oracle-probed).
    assert_eq!(rows["o3"], "def o3: (untyped) -> 2");
    assert!(!rows.contains_key("x1"), "x1 must decline: {rows:?}");
    assert!(!rows.contains_key("x2"), "x2 must decline: {rows:?}");
    assert_eq!(rows["x3"], r##"def x3: (untyped) -> (1 | 2)"##);
    assert_eq!(rows["x4"], r##"def x4: (untyped) -> (1 | 2)"##);
    assert!(!rows.contains_key("x5"), "x5 must decline: {rows:?}");
    assert!(!rows.contains_key("x6"), "x6 must decline: {rows:?}");
    assert!(!rows.contains_key("x7"), "x7 must decline: {rows:?}");
    assert!(!rows.contains_key("x8"), "x8 must decline: {rows:?}");
    assert!(!rows.contains_key("x9"), "x9 must decline: {rows:?}");
    assert!(!rows.contains_key("x10"), "x10 must decline: {rows:?}");
}

#[test]
fn def_local_env_def_level_rescue_and_ensure_returns() {
    // A def whose body is a `rescue` / `ensure` chain lowers to one region
    // spanning the whole def; `collect_explicit_returns` took the def itself for a
    // nested barrier and dropped every `return` (master hid it: a BeginRescue tail
    // typed untyped). `def g1(c); 5; ensure; return 3; end` is `(3 | 5)`.
    // Rows marked `match` are byte-identical to the reference's `sig-gen --print`
    // (oracle-probed); rows marked `decline` the reference emits (or skips) and the
    // port declines — a coverage loss, never a wrong signature.
    let rows = def_rows(
        "def_local_env_def_level_rescue_and_ensure_returns",
        r##"
def g1(c); 5; ensure; return 3; end
def g2(c); begin; return; 7; rescue; 8; end; end
def g3(c); 5; rescue; return 4 if c; 6; end
def g4(c); begin; 5; ensure; return 3; end; end
def g5(c); return 1 if c; 5; ensure; 7; end
def g6(c); 5; rescue; return; end
def g7(c); begin; return 2; ensure; 3; end; end
def g8(c); a = 1; a = 2 if c; a; rescue; a; end
def g9(c); a = 1; a + 1; ensure; return (a || 3); end
"##,
    );
    assert_eq!(rows["g1"], r##"def g1: (untyped) -> (3 | 5)"##);
    assert!(!rows.contains_key("g2"), "g2 must decline: {rows:?}");
    assert_eq!(rows["g3"], r##"def g3: (untyped) -> (4 | 5 | 6)"##);
    assert_eq!(rows["g4"], r##"def g4: (untyped) -> (3 | 5)"##);
    assert_eq!(rows["g5"], r##"def g5: (untyped) -> (1 | 5)"##);
    assert_eq!(rows["g6"], r##"def g6: (untyped) -> (5 | nil)"##);
    assert_eq!(rows["g7"], r##"def g7: (untyped) -> 2"##);
    assert!(!rows.contains_key("g8"), "g8 must decline: {rows:?}");
    assert!(!rows.contains_key("g9"), "g9 must decline: {rows:?}");
}

#[test]
fn def_local_env_block_returns_tail_env_and_loop_carried() {
    // A `return` in an ORDINARY block exits the method and the reference collects
    // it (ReturnBarrier, #1382: only a nested `def`, a lambda and `define_method`
    // / `Kernel#lambda` bodies are barriers) — the port treated every block as a
    // barrier and took a lambda's `return` for the method's. The tail is typed in
    // the env BEFORE the tail statement runs (the whole-body env already carried
    // the tail construct's own writes). A name a loop both reads and writes is
    // carried to the next iteration.
    // Rows marked `match` are byte-identical to the reference's `sig-gen --print`
    // (oracle-probed); rows marked `decline` the reference emits (or skips) and the
    // port declines — a coverage loss, never a wrong signature.
    let rows = def_rows(
        "def_local_env_block_returns_tail_env_and_loop_carried",
        r##"
def rb1(c); c.each { |i| return "s" }; 1; end
def rb3(c); l = -> { return "s" }; 1; end
def rb7(c); a = nil; c.each { |i| return "s" }; a; end
def rb10(c); x = c.map { |i| return 7 }; 2; end
def rb11(c); lambda { return 1 }; 2; end
def rb12(c); proc { return 1 }; 2; end
def rb13(c); c.send(:define_method, :x) { return 1 }; 2; end
def rb15(c); foo.lambda { return 1 }; 2; end
def rb8(c); i = 1; c.each { |i| return i }; 2; end
def tt1(c); a = 1; if c; a = "s"; 5; else; a; end; end
def tt2(c); a = 1; case c; when 1; a = "s"; 5; else; a; end; end
def lc1(c); a = :s; until c.x; a = [a]; end; a; end
def lc2(c); a = 1; while c; a = "s"; end; a; end
def dd1(c); begin; a = []; return []; @i = a; ensure; ; end; end
"##,
    );
    assert_eq!(rows["rb1"], r##"def rb1: (untyped) -> ("s" | 1)"##);
    assert_eq!(rows["rb3"], r##"def rb3: (untyped) -> 1"##);
    assert_eq!(rows["rb7"], r##"def rb7: (untyped) -> ("s" | nil)"##);
    assert_eq!(rows["rb10"], r##"def rb10: (untyped) -> (2 | 7)"##);
    assert_eq!(rows["rb11"], r##"def rb11: (untyped) -> 2"##);
    assert_eq!(rows["rb12"], r##"def rb12: (untyped) -> (1 | 2)"##);
    assert_eq!(rows["rb13"], r##"def rb13: (untyped) -> 2"##);
    assert_eq!(rows["rb15"], r##"def rb15: (untyped) -> (1 | 2)"##);
    assert!(!rows.contains_key("rb8"), "rb8 must decline: {rows:?}");
    assert_eq!(rows["tt1"], r##"def tt1: (untyped) -> (1 | 5)"##);
    assert_eq!(rows["tt2"], r##"def tt2: (untyped) -> (1 | 5)"##);
    assert!(!rows.contains_key("lc1"), "lc1 must decline: {rows:?}");
    assert_eq!(rows["lc2"], r##"def lc2: (untyped) -> ("s" | 1)"##);
    assert!(!rows.contains_key("dd1"), "dd1 must decline: {rows:?}");
}

#[test]
fn def_local_env_declines_a_call_the_port_does_not_fold() {
    // The reference folds a pure call on a constant receiver to a constant (`a =
    // 1.5; a.to_s` is the string 1.5); the port folds only some and types the rest
    // as the RBS nominal (`String`) — a shared-method byte mismatch the def-local
    // env exposed through locals (a literal receiver, `1.5.to_s`, is the pre-
    // existing gap master shares). A call on a def-local constant carrier whose
    // result is a scalar / array / hash nominal declines; a folded `Constant`
    // result is kept.
    // Rows marked `match` are byte-identical to the reference's `sig-gen --print`
    // (oracle-probed); rows marked `decline` the reference emits (or skips) and the
    // port declines — a coverage loss, never a wrong signature.
    let rows = def_rows(
        "def_local_env_declines_a_call_the_port_does_not_fold",
        r##"
def ft1(c); a = 1.5; a.to_s; end
def ft2(c); a = [1]; a.to_s; end
def ft3(c); a = {a: 1}; a.size; end
def ft4(c); a = "ab"; a.strip; end
def ft5(c); a = :s; a.upcase; end
def ft6(c); a = 1; a.to_s; end
def ft7(c); a = "x"; a.upcase; end
def ft8(c); a = [1, 2]; a.size; end
def ft9(c); a = 5; a * 2; end
def ft10(c); a = "ab"; a.length; end
def ft11(c); a = {a: 1}; a[:a]; end
"##,
    );
    assert!(!rows.contains_key("ft1"), "ft1 must decline: {rows:?}");
    assert!(!rows.contains_key("ft2"), "ft2 must decline: {rows:?}");
    assert!(!rows.contains_key("ft3"), "ft3 must decline: {rows:?}");
    assert!(!rows.contains_key("ft4"), "ft4 must decline: {rows:?}");
    assert!(!rows.contains_key("ft5"), "ft5 must decline: {rows:?}");
    assert_eq!(rows["ft6"], r##"def ft6: (untyped) -> "1""##);
    assert_eq!(rows["ft7"], r##"def ft7: (untyped) -> "X""##);
    assert_eq!(rows["ft8"], r##"def ft8: (untyped) -> 2"##);
    assert_eq!(rows["ft9"], r##"def ft9: (untyped) -> 10"##);
    assert_eq!(rows["ft10"], r##"def ft10: (untyped) -> 2"##);
    assert_eq!(rows["ft11"], r##"def ft11: (untyped) -> 1"##);
}

#[test]
fn def_local_env_declines_a_hole_in_a_generic_slot() {
    // A def-local the replay declines reads `untyped`; inside a non-symbol-key
    // Hash literal that lands in a generic type-arg slot (`Hash[String,
    // untyped]`), which the buried-untyped guard exempts as an elaborated form —
    // the reference has the precise value union
    // (`Rack::Directory#check_bad_request`: `Hash[String, \"12\" | ...]`). A tail
    // that reads a def-written local and carries such a hole declines; a literal
    // tuple of typed locals is kept.
    // Rows marked `match` are byte-identical to the reference's `sig-gen --print`
    // (oracle-probed); rows marked `decline` the reference emits (or skips) and the
    // port declines — a coverage loss, never a wrong signature.
    let rows = def_rows(
        "def_local_env_declines_a_hole_in_a_generic_slot",
        r##"
def rk1(p); return if p.x; body = "Bad"; [400, { "ct" => "t", "cl" => body.bytesize.to_s }, [body]]; end
def rk2(c); a = 1; [a, { "k" => a }]; end
def rk3(c); a = "s"; [a, a]; end
"##,
    );
    assert!(!rows.contains_key("rk1"), "rk1 must decline: {rows:?}");
    assert_eq!(rows["rk2"], r##"def rk2: (untyped) -> [1, Hash[String, 1]]"##);
    assert_eq!(rows["rk3"], r##"def rk3: (untyped) -> ["s", "s"]"##);
}

#[test]
fn def_local_env_folds_a_literal_decision() {
    // The reference constant-folds a decision built from literals (`if true`, `if
    // 1 > 2`, `x = 5 if (y = 3)`); `type_of(If)` elides the dead arm, so the tail
    // replay and the statement replay must too — master emitted `1` for `if true;
    // 1; else; 2; end`, the replay joined both arms.
    // Rows marked `match` are byte-identical to the reference's `sig-gen --print`
    // (oracle-probed); rows marked `decline` the reference emits (or skips) and the
    // port declines — a coverage loss, never a wrong signature.
    let rows = def_rows(
        "def_local_env_folds_a_literal_decision",
        r##"
def k1(c); if true; 1; else; 2; end; end
def k2(c); if nil; 1; else; 2; end; end
def k3(c); if 1; 1; else; 2; end; end
def k4(c); if (a = 2); 1; else; 2; end; end
def k6(c); 1 if true; end
def k7(c); x = nil; x = 5 if true; x; end
def k8(c); x = nil; x = 5 if nil; x; end
def k9(c); x = nil; x = 5 if (y = 3); x; end
def k12(c); x = 1; x = 5 if 1 == 1; x; end
def k16(c); x = 1; x = 5 if 1 > 2; x; end
def k17(c); x = 1; if "a" == "b"; x = 2; end; x; end
def k18(c); true ? 1 : 2; end
def k19(c); (c; 1) ? 3 : 4; end
def k20(c); x = 1; x = 2 if (c; true); x; end
def k22(c); x = 1; x = 2 if true && false; x; end
def k23(c); x = 1; x = 2 if [1]; x; end
def k24(c); x = 1; x = 2 if "s"; x; end
def k31(c); x = 1; x = 2 if c; x; end
"##,
    );
    assert_eq!(rows["k1"], r##"def k1: (untyped) -> 1"##);
    assert_eq!(rows["k2"], r##"def k2: (untyped) -> 2"##);
    assert_eq!(rows["k3"], r##"def k3: (untyped) -> 1"##);
    assert_eq!(rows["k4"], r##"def k4: (untyped) -> 1"##);
    assert_eq!(rows["k6"], r##"def k6: (untyped) -> 1"##);
    assert_eq!(rows["k7"], r##"def k7: (untyped) -> 5"##);
    assert_eq!(rows["k8"], r##"def k8: (untyped) -> nil"##);
    assert_eq!(rows["k9"], r##"def k9: (untyped) -> 5"##);
    assert_eq!(rows["k12"], r##"def k12: (untyped) -> 5"##);
    assert_eq!(rows["k16"], r##"def k16: (untyped) -> 1"##);
    assert_eq!(rows["k17"], r##"def k17: (untyped) -> 1"##);
    assert_eq!(rows["k18"], r##"def k18: (untyped) -> 1"##);
    assert!(!rows.contains_key("k19"), "k19 must decline: {rows:?}");
    assert!(!rows.contains_key("k20"), "k20 must decline: {rows:?}");
    assert!(!rows.contains_key("k22"), "k22 must decline: {rows:?}");
    assert_eq!(rows["k23"], r##"def k23: (untyped) -> 2"##);
    assert_eq!(rows["k24"], r##"def k24: (untyped) -> 2"##);
    assert_eq!(rows["k31"], r##"def k31: (untyped) -> (1 | 2)"##);
}

#[test]
fn def_local_env_declines_writes_the_replay_does_not_walk() {
    // A local written in a predicate, argument, interpolation, value operand,
    // `for` index, `rescue => e` or pattern is stale after the statement; the
    // reference re-types it (`a = 1; if (a = "s"); end; a` → `"s"`).
    // Rows marked `match` are byte-identical to the reference's `sig-gen --print`
    // (oracle-probed); rows marked `decline` the reference emits (or skips) and the
    // port declines — a coverage loss, never a wrong signature.
    let rows = def_rows(
        "def_local_env_declines_writes_the_replay_does_not_walk",
        r##"
def s1(c); a = 1; if (a = "s"); end; a; end
def s2(c); a = 1; case (a = "s"); when 1; end; a; end
def s3(c); a = 1; while (a = "s"); break; end; a; end
def s4(c); a = 1; foo(a = "s"); a; end
def s5(c); a = 1; c.foo(a = "s"); a; end
def s6(c); a = 1; [a = "s"]; a; end
def s7(c); a = 1; "#{a = "s"}"; a; end
def s8(c); a = 1; x = (a = "s"); a; end
def s9(c); a = 1; @i = (a = "s"); a; end
def s10(c); a = 1; x, y = (a = "s"), 1; a; end
def s11(c); a = 1; c[a = "s"] = 1; a; end
def s12(c); a = 1; ((a = "s") && c); a; end
def s13(c); a = 1; !(a = "s"); a; end
def s14(c); a = 1; p(a = "s"); a; end
def s15(c); a = 1; (a = "s") rescue 1; a; end
def s16(c); a = 1; c.foo rescue (a = "s"); a; end
def s17(c); a = 1; for a in c; end; a; end
def s18(c); a = 1; for i, a in c; end; a; end
def s19(c); a = 1; begin; c.x; rescue => a; end; a; end
def s20(c); a = 1; case c; in Integer => a; end; a; end
def s21(c); a = 1; case [c]; in [a]; end; a; end
def s22(c); a = 1; c => a; a; end
def s23(c); a = 1; c in [a]; a; end
def s24(c); a = 1; /(?<a>x)/ =~ c; a; end
def s25(c); a = 1; x = (a = 2) if c; a; end
def s26(c); a = 1; a = 2 and c; a; end
def s27(c); a = 1; case c; in Integer; a = "s"; end; a; end
def s28(c); a = 1; c ? (a = 2) : 3; a; end
def s29(c); a = 1; c.each { |a| a }; a; end
def s30(c); a = 1; def foo; a = 2; end; a; end
def s31(c); a = 1; b = a; b = 2 if c; b; end
"##,
    );
    assert!(!rows.contains_key("s1"), "s1 must decline: {rows:?}");
    assert!(!rows.contains_key("s2"), "s2 must decline: {rows:?}");
    assert!(!rows.contains_key("s3"), "s3 must decline: {rows:?}");
    assert!(!rows.contains_key("s4"), "s4 must decline: {rows:?}");
    assert!(!rows.contains_key("s5"), "s5 must decline: {rows:?}");
    assert!(!rows.contains_key("s6"), "s6 must decline: {rows:?}");
    assert!(!rows.contains_key("s7"), "s7 must decline: {rows:?}");
    assert!(!rows.contains_key("s8"), "s8 must decline: {rows:?}");
    assert!(!rows.contains_key("s9"), "s9 must decline: {rows:?}");
    assert!(!rows.contains_key("s10"), "s10 must decline: {rows:?}");
    assert!(!rows.contains_key("s11"), "s11 must decline: {rows:?}");
    assert!(!rows.contains_key("s12"), "s12 must decline: {rows:?}");
    assert!(!rows.contains_key("s13"), "s13 must decline: {rows:?}");
    assert!(!rows.contains_key("s14"), "s14 must decline: {rows:?}");
    assert!(!rows.contains_key("s15"), "s15 must decline: {rows:?}");
    assert!(!rows.contains_key("s16"), "s16 must decline: {rows:?}");
    assert!(!rows.contains_key("s17"), "s17 must decline: {rows:?}");
    assert!(!rows.contains_key("s18"), "s18 must decline: {rows:?}");
    assert!(!rows.contains_key("s19"), "s19 must decline: {rows:?}");
    assert!(!rows.contains_key("s20"), "s20 must decline: {rows:?}");
    assert!(!rows.contains_key("s21"), "s21 must decline: {rows:?}");
    assert!(!rows.contains_key("s22"), "s22 must decline: {rows:?}");
    assert!(!rows.contains_key("s23"), "s23 must decline: {rows:?}");
    assert!(!rows.contains_key("s24"), "s24 must decline: {rows:?}");
    assert!(!rows.contains_key("s25"), "s25 must decline: {rows:?}");
    assert!(!rows.contains_key("s26"), "s26 must decline: {rows:?}");
    assert!(!rows.contains_key("s27"), "s27 must decline: {rows:?}");
    assert_eq!(rows["s28"], r##"def s28: (untyped) -> (1 | 2)"##);
    assert_eq!(rows["s29"], r##"def s29: (untyped) -> 1"##);
    assert_eq!(rows["s30"], r##"def s30: (untyped) -> 1"##);
    assert_eq!(rows["s31"], r##"def s31: (untyped) -> (1 | 2)"##);
}

#[test]
fn def_local_env_declines_a_decision_on_a_typed_local() {
    // The reference narrows (and constant-folds) on a decision that reads a local
    // it has typed (`a.nil?`, `a == 1`, `a ? a : 0`, `a&.succ`); the replay does
    // not narrow, so it declined-nothing and emitted the looser `(1 | 5 | nil)` /
    // a wrong `2`. Any statement or tail holding such a decision declines what it
    // writes.
    // Rows marked `match` are byte-identical to the reference's `sig-gen --print`
    // (oracle-probed); rows marked `decline` the reference emits (or skips) and the
    // port declines — a coverage loss, never a wrong signature.
    let rows = def_rows(
        "def_local_env_declines_a_decision_on_a_typed_local",
        r##"
def n1(c); a = nil; a = 1 if c; return 5 if a.nil?; a; end
def n2(c); a = nil; a = 1 if c; return 5 unless a; a; end
def n3(c); a = nil; a = 1 if c; if a; a; else; 0; end; end
def n4(c); a = nil; a = 1 if c; a.nil? ? 0 : a; end
def n5(c); a = nil; a = "s" if c; return 0 unless a.is_a?(String); a; end
def n6(c); a = nil; a = 1 if c; a&.succ; end
def n7(c); a = nil; a = 1 if c; raise "x" if a.nil?; a; end
def n8(c); a = 1; if a == 1; a; else; "s"; end; end
def n9(c); a = nil; a = 1 if c; x = (a.nil? ? 0 : a); x; end
def n10(c); a = nil; a = 1 if c; case a; when nil; 0; else; a; end; end
def n11(c); a = nil; a = 1 if c; while a.nil?; a = 2; end; a; end
def n12(c); a = 1; case c; in Integer; 1; end; a = 2 if c; a; end
def n13(c); a = 1; raise unless c.is_a?(Integer); a = 2 if c; a; end
def n14(c); a = nil; a = 1 if c; a; end
def n15(c); r = nil; r = 1 if c.a; r = 2 if c.b; r; end
def n16(c); a = nil; a = 1 if c; x = a; x; end
"##,
    );
    assert!(!rows.contains_key("n1"), "n1 must decline: {rows:?}");
    assert!(!rows.contains_key("n2"), "n2 must decline: {rows:?}");
    assert!(!rows.contains_key("n3"), "n3 must decline: {rows:?}");
    assert!(!rows.contains_key("n4"), "n4 must decline: {rows:?}");
    assert!(!rows.contains_key("n5"), "n5 must decline: {rows:?}");
    assert!(!rows.contains_key("n6"), "n6 must decline: {rows:?}");
    assert!(!rows.contains_key("n7"), "n7 must decline: {rows:?}");
    assert!(!rows.contains_key("n8"), "n8 must decline: {rows:?}");
    assert!(!rows.contains_key("n9"), "n9 must decline: {rows:?}");
    assert!(!rows.contains_key("n10"), "n10 must decline: {rows:?}");
    assert!(!rows.contains_key("n11"), "n11 must decline: {rows:?}");
    assert!(!rows.contains_key("n12"), "n12 must decline: {rows:?}");
    assert!(!rows.contains_key("n13"), "n13 must decline: {rows:?}");
    assert_eq!(rows["n14"], r##"def n14: (untyped) -> (1 | nil)"##);
    assert_eq!(rows["n15"], r##"def n15: (untyped) -> (1 | 2 | nil)"##);
    assert_eq!(rows["n16"], r##"def n16: (untyped) -> (1 | nil)"##);
}

#[test]
fn def_local_env_declines_a_terminating_arm() {
    // The reference drops an arm that exits (`return`, `raise`, `exit`, a
    // `bot`-typed helper) from the join; the port joined it (`if c; a = 2; raise;
    // end; a` → `1`).
    // Rows marked `match` are byte-identical to the reference's `sig-gen --print`
    // (oracle-probed); rows marked `decline` the reference emits (or skips) and the
    // port declines — a coverage loss, never a wrong signature.
    let rows = def_rows(
        "def_local_env_declines_a_terminating_arm",
        r##"
def t1(c); a = 1; if c; a = 2; raise "x"; end; a; end
def t2(c); a = 1; if c; a = 2; return; end; a; end
def t3(c); a = 1; if c; a = 2; return 7; end; a; end
def t4(c); a = 1; unless c; a = 2; raise "x"; end; a; end
def t5(c); a = 1; if c; a = 2; else; raise "x"; end; a; end
def t6(c); a = 1; case c; when 1; a = 2; raise "x"; else; a = 3; end; a; end
def t7(c); a = 1; if c; a = 2; exit; end; a; end
def t8(c); a = 1; if c; a = 2; abort; end; a; end
def t9(c); a = 1; if c; a = 2; throw :x; end; a; end
def t10(c); a = 1; if c; a = 2; fail "x"; end; a; end
def t11(c); a = 1; if c; a = 2; die; end; a; end
def t12(c); a = 1; if c; a = 2; self.die; end; a; end
def t13(c); a = 1; if c; a = 2; Kernel.exit; end; a; end
def t14(c); a = 1; c && (a = 2; raise "x"); a; end
def t15(c); a = 1; begin; c.x; rescue; a = 2; raise "x"; end; a; end
def t16(c); a = 1; while c; a = 2; break; end; a; end
def t17(c); a = 1; if c; a = 2; puts a; end; a; end
def t18(c); a = 1; if c; return a; end; a = 2; a; end
def t19(c); a = 1; if c; a = 2; else; a = 3; end; a; end
"##,
    );
    assert!(!rows.contains_key("t1"), "t1 must decline: {rows:?}");
    assert!(!rows.contains_key("t2"), "t2 must decline: {rows:?}");
    assert!(!rows.contains_key("t3"), "t3 must decline: {rows:?}");
    assert!(!rows.contains_key("t4"), "t4 must decline: {rows:?}");
    assert!(!rows.contains_key("t5"), "t5 must decline: {rows:?}");
    assert!(!rows.contains_key("t6"), "t6 must decline: {rows:?}");
    assert!(!rows.contains_key("t7"), "t7 must decline: {rows:?}");
    assert!(!rows.contains_key("t8"), "t8 must decline: {rows:?}");
    assert!(!rows.contains_key("t9"), "t9 must decline: {rows:?}");
    assert!(!rows.contains_key("t10"), "t10 must decline: {rows:?}");
    assert!(!rows.contains_key("t11"), "t11 must decline: {rows:?}");
    assert!(!rows.contains_key("t12"), "t12 must decline: {rows:?}");
    assert!(!rows.contains_key("t13"), "t13 must decline: {rows:?}");
    assert!(!rows.contains_key("t14"), "t14 must decline: {rows:?}");
    assert!(!rows.contains_key("t15"), "t15 must decline: {rows:?}");
    assert!(!rows.contains_key("t16"), "t16 must decline: {rows:?}");
    assert_eq!(rows["t17"], r##"def t17: (untyped) -> (1 | 2)"##);
    assert_eq!(rows["t18"], r##"def t18: (untyped) -> (1 | 2)"##);
    assert_eq!(rows["t19"], r##"def t19: (untyped) -> (2 | 3)"##);
}

/// `Class: rbs` per emitted candidate, in emission order.
fn emitted_rows(tag: &str, src: &str) -> Vec<String> {
    candidates_tagged(tag, src, false)
        .iter()
        .map(|c| format!("{}: {}", c.class_name, c.rbs))
        .collect()
}

#[test]
fn conditional_namespace_defs_emit_candidates() {
    // rigor-rs#535, oracle (`rigor sig-gen --print`, pinned reference): the
    // reference's `walk_defs` descends if/case/begin/loop bodies, so a
    // namespace or def under a conditional emits as a direct one does.
    let rows: &[(&str, &[&str])] = &[
        ("class K; if c; class N; def z; 1; end; end; end; end", &["K::N: def z: () -> 1"]),
        ("class K; if c; module N; def z; 1; end; end; end; end", &["K::N: def z: () -> 1"]),
        ("class K; unless c; class N; def z; 1; end; end; end; end", &["K::N: def z: () -> 1"]),
        ("class K; if c; def a; 1; end; else; def b; 2; end; end; end", &["K: def a: () -> 1", "K: def b: () -> 2"]),
        ("class K; if c; class << self; def s; 1; end; end; end; end", &["K: def self.s: () -> 1"]),
        ("if c; class N; def z; 1; end; end; end", &["N: def z: () -> 1"]),
    ];
    for (i, (src, want)) in rows.iter().enumerate() {
        assert_eq!(emitted_rows(&format!("cond_ns_{i}"), src), *want, "{src}");
    }
}

#[test]
fn visibility_call_that_last_writes_a_conditional_defs_slot_decides_it() {
    // rigor-rs#535 review item (a), oracle-verified rows. The reference's table
    // is keyed `(class, name)` per file and written by a full-depth walk, so a
    // literal-name visibility call anywhere (after the def, inside another
    // conditional, a block, a later reopening) re-marks the slot; a `def`
    // after it writes the slot again; `module_function :b` reads as `private`.
    let rows: &[(&str, &[&str])] = &[
        ("class K; if c; def b; 2; end; end; private :b; end", &[]),
        (
            "class K; if c; def a; 1; end; def b; 2; end; end; private :a, :b; def z; 1; end; end",
            &["K: def z: () -> 1"],
        ),
        ("module M; if c; def b; 2; end; end; module_function :b; end", &[]),
        ("module M; if c; def b; 2; end; end; private :b; end", &[]),
        ("class K; if c; def b; 2; end; private :b; end; end", &[]),
        ("class K; if c; def b; 2; end; private :b; end; def z; 1; end; end", &["K: def z: () -> 1"]),
        ("class K; if c; def a; 1; end; private :a; else; def b; 1; end; end; end", &["K: def b: () -> 1"]),
        ("class K; if c; def b; 2; end; end; private \"b\"; end", &[]),
        ("class K; if c; def b; 2; end; end; protected :b; end", &[]),
        ("class K; if c; def b; 2; end; end; private %i[b zz]; end", &[]),
        ("class K; if c; def b; 2; end; end; private :b, :zz; end", &[]),
        ("class K; if c; def b; 2; end; end; private(:b); end", &[]),
        ("class K; if c; def b; 2; end; end; self.private :b; end", &[]),
        ("class K; if c; def b; 2; end; end; send(:private, :b); end", &[]),
        ("class K; if c; def b; 2; end; end; __send__(:private, :b); end", &[]),
        ("class K; if c; def b; 2; end; end; private :b if d; end", &[]),
        ("class K; if c; def b; 2; end; end; if d; private :b; end; end", &[]),
        ("class K; if c; def b; 2; end; end; foo { private :b }; end", &[]),
        ("class K; unless c; def b; 2; end; end; private :b; end", &[]),
        ("class K; def b; 2; end if c; private :b; end", &[]),
        ("class K; case x; when 1; def b; 2; end; end; private :b; end", &[]),
        ("class K; begin; def b; 2; end; rescue; end; private :b; end", &[]),
        ("class K; while c; def b; 2; end; end; private :b; end", &[]),
        ("class K; 3.times { def b; 2; end }; private :b; end", &[]),
        // A reopening shares the slot; a nested one qualifies by path.
        ("class K; if c; def b; 2; end; end; end; class K; private :b; end", &[]),
        (
            "class K; if c; def b; 2; end; end; end; class K; def z; 1; end; private :b; end",
            &["K: def z: () -> 1"],
        ),
        ("module N; class K; if c; def b; 2; end; end; end; end; module N; class K; private :b; end; end", &[]),
        // The LAST write wins: public after private re-opens the slot, and a
        // `def` after a call writes it at its running default again.
        ("class K; if c; def b; 2; end; end; private :b; public :b; end", &["K: def b: () -> 2"]),
        ("class K; if c; def b; 2; end; end; public :b; private :b; end", &[]),
        ("class K; if c; def b; 2; end; end; public :b; end", &["K: def b: () -> 2"]),
        // Review item (b): a re-mark to public overrides the running `private`.
        ("class K; private; if c; def b; 2; end; end; public :b; end", &["K: def b: () -> 2"]),
        ("class K; if c; def b; 2; end; end; protected; def z; 1; end; public :b; end", &["K: def b: () -> 2"]),
        ("class K; private :b; if c; def b; 2; end; end; end", &["K: def b: () -> 2"]),
        ("class K; if c; private :b; def b; 2; end; end; end", &["K: def b: () -> 2"]),
        // Must-still-fire controls: calls the reference does not apply to this
        // slot (a splat, a def body, a singleton class, another class's block,
        // `private_class_method`, `private_constant`) leave the def emitted.
        ("class K; if c; def b; 2; end; end; private(*names); end", &["K: def b: () -> 2"]),
        ("class K; if c; def b; 2; end; end; def d; private :b; end; end", &["K: def b: () -> 2"]),
        ("class K; if c; def b; 2; end; end; class << self; private :b; end; end", &["K: def b: () -> 2"]),
        ("class K; if c; def b; 2; end; end; Struct.new(:q) do; private :b; end; end", &["K: def b: () -> 2"]),
        ("class K; if c; def b; 2; end; end; Class.new { private :b }; end", &["K: def b: () -> 2"]),
        ("class K; if c; def b; 2; end; end; class L; private :b; end; end", &["K: def b: () -> 2"]),
        ("class K; if c; def b; 2; end; end; private_class_method :b; end", &["K: def b: () -> 2"]),
        ("class K; if c; def b; 2; end; end; private_constant :B; end", &["K: def b: () -> 2"]),
        ("class K; if c; def self.b; 2; end; end; private_class_method :b; end", &["K: def self.b: () -> 2"]),
        ("class K; if c; def b; 2; end; end; private :zz; def z; 1; end; end", &["K: def b: () -> 2", "K: def z: () -> 1"]),
    ];
    for (i, (src, want)) in rows.iter().enumerate() {
        assert_eq!(emitted_rows(&format!("cond_remark_{i}"), src), *want, "{src}");
    }
}

#[test]
fn defs_and_namespaces_in_call_blocks_emit_in_source_order() {
    // rigor-rs#535 review item (c): the reference's single top-to-bottom walk
    // emits a call block's defs and namespaces where the call sits, so its
    // class groups come out in first-seen order (`K` before `K::N`). Printed
    // order (oracle): `class K` {c, b} then `class K::N`.
    let src = "class K; def c; 3; end; foo { class N; def z; 1; end; end; def b; 2; end }; end";
    assert_eq!(
        emitted_rows("call_block_order", src),
        ["K: def c: () -> 3", "K::N: def z: () -> 1", "K: def b: () -> 2"]
    );
    let src = "class K; foo { def a; 1; end; class N; def z; 1; end; end; def b; 2; end }; def c; 3; end; end";
    assert_eq!(
        emitted_rows("call_block_order2", src),
        ["K: def a: () -> 1", "K::N: def z: () -> 1", "K: def b: () -> 2", "K: def c: () -> 3"]
    );
}

/// Check one `def_local_*` row table: each row is the body of `def rN(c, xs)`
/// under `class K`. `Some(t)` asserts the port emits `-> t` (byte-identical to
/// the oracle's `sig-gen --print`, probed when the row was written); `None`
/// asserts the port declines (a coverage loss — never a wrong signature).
fn check_def_local_rows(tag: &str, rows: &[(&str, Option<&str>)]) {
    let body: String = rows
        .iter()
        .enumerate()
        .map(|(i, (src, _))| format!("def r{i}(c, xs); {src}; end\n"))
        .collect();
    let got = def_rows(tag, &body);
    for (i, (src, want)) in rows.iter().enumerate() {
        let key = format!("r{i}");
        match want {
            Some(ty) => assert_eq!(
                got.get(&key).map(String::as_str),
                Some(format!("def {key}: (untyped, untyped) -> {ty}").as_str()),
                "row `{src}`"
            ),
            None => assert!(!got.contains_key(&key), "row `{src}` must decline: {:?}", got.get(&key)),
        }
    }
}

#[test]
fn def_local_env_folds_operator_writes() {
    // rigor-rs#548: `a <op>= v` types as the literal-by-literal fold of its entry binding and `v`;
    // every dispatch the port cannot fold exactly declines the local.
    // `Some(t)`: the port emits `-> t`, byte-identical to the reference (oracle-probed);
    // `None`: the port declines (the reference's answer is noted) — a coverage loss, never a wrong signature.
    let rows: &[(&str, Option<&str>)] = &[
        ("a = 1; a += 2; a", Some("3")),
        ("a = 1; a -= 2; a", Some("-1")),
        ("a = 3; a *= 2; a", Some("6")),
        ("a = 7; a /= 2; a", Some("3")),
        ("a = 7; a %= 4; a", Some("3")),
        ("a = 2; a **= 10; a", Some("1024")),
        ("a = 1; a <<= 3; a", Some("8")),
        ("a = 5; a >>= 1; a", Some("2")),
        ("a = 6; a &= 3; a", Some("2")),
        ("a = 6; a |= 3; a", Some("7")),
        ("a = 6; a ^= 3; a", Some("5")),
        ("a = \"x\"; a += \"y\"; a", Some("\"xy\"")),
        ("a = \"x\"; a *= 3; a", Some("\"xxx\"")),
        ("s = \"\"; s += \"a\"; s", Some("\"a\"")),
        ("a = 1; a += 1; a += 1; a", Some("3")),
        ("a = 1; a += 1; b = a; b", Some("2")),
        ("b = 2; a = 1; a += b; a", Some("3")),
        ("a = 1; if c; a += 1; end; a", Some("(1 | 2)")),
        ("a = 1; if c; a += 1; else; a += 2; end; a", Some("(2 | 3)")),
        ("a = 1; c ? (a += 1) : 3; a", Some("(1 | 2)")),
        ("a = 1; c && a += 1; a", Some("(1 | 2)")),
        ("a = 1; case c; when 1 then a += 1; else a += 2; end; a", Some("(2 | 3)")),
        ("a = 1; begin; foo; rescue; a += 5; end; a", None),  // ref: (1 | 6)
        ("a = 1; begin; a += 1; ensure; a += 5; end; a", None),  // ref: 7
        ("a = 7; a /= 0; a", None),  // ref: Integer
        ("a = 1; a += 1.5; a", None),  // ref: Float
        ("a = 1.5; a += 1; a", None),  // ref: Float
        ("a = 1; a += nil; a", None),  // ref: Integer
        ("a = 1; a += \"x\"; a", None),  // ref: Integer
        ("a = :s; a += 1; a", None),  // ref: None
        ("a = nil; a += 1; a", None),  // ref: None
        ("a = [1]; a += [2]; a", None),  // ref: [1, 2]
        ("a = 1; a += c; a", None),  // ref: None
        ("a = c; a += 1; a", None),  // ref: None
        ("a += 1; a", None),  // ref: None
        ("a = 1; a += 1", None),  // ref: 2
        ("a = 1; x = (a += 1); x", None),  // ref: 2
        ("a = 1; c ? (a += 1) : 0", None),  // ref: (0 | 2)
        ("a = 1; @n = c ? (a += 1) : 0; @n", None),  // ref: (0 | 2)
        ("a = 1; x = c ? (a += 1) : 0; x", None),  // ref: (0 | 2)
        ("a = 2; a **= 100; a", Some("1267650600228229401496703205376")),
    ];
    check_def_local_rows("def_local_env_folds_operator_writes", rows);
}

#[test]
fn def_local_env_folds_logical_writes() {
    // rigor-rs#548: `a ||= v` is the truthy part of the entry plus `v`; `a &&= v` the falsy part plus `v`.
    // `Some(t)`: the port emits `-> t`, byte-identical to the reference (oracle-probed);
    // `None`: the port declines (the reference's answer is noted) — a coverage loss, never a wrong signature.
    let rows: &[(&str, Option<&str>)] = &[
        ("a = nil; a ||= 1; a", Some("1")),
        ("a = 1; a ||= \"x\"; a", Some("(\"x\" | 1)")),
        ("a = false; a ||= \"x\"; a", Some("\"x\"")),
        ("a = nil; a ||= nil; a", Some("nil")),
        ("a = nil; a &&= 1; a", Some("(1 | nil)")),
        ("a = 1; a &&= \"x\"; a", Some("\"x\"")),
        ("a = false; a &&= 1; a", Some("(1 | false)")),
        ("a = nil; a ||= c; a", None),  // ref: None
        ("a = c; a ||= 1; a", None),  // ref: None
        ("a = 1; a ||= foo; a", None),  // ref: None
        ("a = 1; if c; a ||= 2; end; a", Some("(1 | 2)")),
        ("a ||= 1; a", None),  // ref: None
    ];
    check_def_local_rows("def_local_env_folds_logical_writes", rows);
}

#[test]
fn def_local_env_declines_loop_and_block_carried_op_writes() {
    // rigor-rs#548 review blocker 1: an operator write in a loop / block body feeds back on the variable
    // (`s += 1` runs 0..n times); the reference widens it to the class (`Integer`) and the port declines
    // instead of folding the body as if it ran once (`(0 | 1)`).
    // `Some(t)`: the port emits `-> t`, byte-identical to the reference (oracle-probed);
    // `None`: the port declines (the reference's answer is noted) — a coverage loss, never a wrong signature.
    let rows: &[(&str, Option<&str>)] = &[
        ("s = 0; 3.times { s += 1 }; s", None),  // ref: Integer
        ("s = 0; while true; s += 1; end; s", None),  // ref: Integer
        ("s = 0; until s > 3; s += 1; end; s", None),  // ref: Integer
        ("s = 1; [1,2].each { |n| s *= 2 }; s", None),  // ref: Integer
        ("s = \"\"; [1,2].each { s += \"x\" }; s", None),  // ref: String
        ("a = 1; [1].each { |e| a += e }; a", None),  // ref: Integer
        ("s = 0; while c; s += 1; end; s", None),  // ref: Integer
        ("s = 0; loop { s += 1; break if c }; s", None),  // ref: None
        ("s = 0; for i in xs; s += 1; end; s", None),  // ref: (0 | 1)
        ("s = 0; xs.each { |x| s += x }; s", None),  // ref: None
        ("s = 0; xs.map { s += 1 }; s", None),  // ref: None
        ("s = 0; 3.times { s ||= 1 }; s", None),  // ref: (0 | 1)
        ("s = nil; 3.times { s ||= 1 }; s", None),  // ref: (1 | nil)
        ("s = 0; xs.each { |x| if c; s += 1; end }; s", None),  // ref: None
        ("s = 0; while c; if xs; s += 1; end; end; s", None),  // ref: Integer
        ("s = 0; begin; s += 1; end while c; s", None),  // ref: Integer
        ("s = 0; s += 1 while c; s", None),  // ref: Integer
        ("s = 0; xs.each { while c; s += 1; end }; s", None),  // ref: None
        ("s = 0; 1.times { s += 1 }; s", None),  // ref: Integer
        ("s = 0; xs.each { s += 1; return s if c }; s", None),  // ref: None
        ("s = 0; while c; return s; s += 1; end; s", None),  // ref: (0 | 1 | 2 | Integer)
        ("a = 1; xs.each { a = 2 }; a", None),  // ref: None
        ("a = 1; while c; a = 2; end; a", Some("(1 | 2)")),
    ];
    check_def_local_rows("def_local_env_declines_loop_and_block_carried_op_writes", rows);
}

#[test]
fn def_local_env_binds_ivar_tails() {
    // rigor-rs#548: `@i = v` binds the sigilled name — a later read in the same def types as the literal it
    // was written with; the replay declines everything that may rewrite it unseen.
    // `Some(t)`: the port emits `-> t`, byte-identical to the reference (oracle-probed);
    // `None`: the port declines (the reference's answer is noted) — a coverage loss, never a wrong signature.
    let rows: &[(&str, Option<&str>)] = &[
        ("@i = 1; @i", Some("1")),
        ("@i = \"s\"; @i", Some("\"s\"")),
        ("@i = [1]; @i", Some("[1]")),
        ("@i = :a; @i", Some(":a")),
        ("@i = {a: 1}; @i", Some("{ a: 1 }")),
        ("@i = nil; @i", Some("nil")),
        ("@i = 1.5; @i", Some("Float")),
        ("@i = 1; @i = 2; @i", Some("2")),
        ("@i = 1; x = @i; x", Some("1")),
        ("@i = 1; [@i, @i]", Some("[1, 1]")),
        ("@i = 1; if c; @i = 2; end; @i", Some("(1 | 2)")),
        ("@i = 1; if c; @i = 2; else; @i = 3; end; @i", Some("(2 | 3)")),
        ("@i = 1; @i = 2 if c; @i", Some("(1 | 2)")),
        ("@i = 1; unless c; @i = :s; end; @i", Some("(1 | :s)")),
        ("@i = 1; case c; when 1 then @i = 2; else @i = 3; end; @i", Some("(2 | 3)")),
        ("@i = 1; begin; @i = 2; rescue; @i = 3; end; @i", Some("(2 | 3)")),
        ("@i = 1; begin; foo; rescue; @i = 3; end; @i", None),  // ref: None
        ("if c; @i = 1; end; @i", None),  // ref: None
        ("@i = 1 if c; @i", None),  // ref: None
        ("if c; @i = 1; else; @i = 2; end; @i", Some("(1 | 2)")),
        ("@i = nil; @i = 3 if c; @i", Some("(3 | nil)")),
        ("@i = 1; a = @i; a", Some("1")),
        ("a = 2; @i = a; @i", Some("2")),
        ("@i = c; @i", None),  // ref: None
        ("@i = foo; @i", None),  // ref: None
        ("@i", None),  // ref: None
        ("x = @i; x", None),  // ref: None
        ("@i = 1; @j", None),  // ref: None
        ("@i = 1; @i..2", Some("Range[Integer]")),
        ("@i = 1; @i.to_s", Some("\"1\"")),
        ("@i = [1]; @i.to_s", None),  // ref: "[1]"
        ("@i = 1; @i + 1", Some("2")),
        ("@i = \"s\"; @i.upcase", Some("\"S\"")),
    ];
    check_def_local_rows("def_local_env_binds_ivar_tails", rows);
}

#[test]
fn def_local_env_declines_ivar_writes_and_calls_the_replay_does_not_see() {
    // rigor-rs#548 review blocker 2 + non-blocking rows: an ivar operator write, a multi-assign to an ivar target,
    // and any call that may rewrite `self`'s ivars (`foo`, `self.foo`, `reset!`, `yield`, `super`, `send`,
    // `instance_variable_set`, a proc call, an attribute / index write on self) leave the old binding stale — the
    // reference widens it (`@n = 1; foo; @n` reads the class seed); the port declines it.
    // `Some(t)`: the port emits `-> t`, byte-identical to the reference (oracle-probed);
    // `None`: the port declines (the reference's answer is noted) — a coverage loss, never a wrong signature.
    let rows: &[(&str, Option<&str>)] = &[
        ("@n = 0; @n += 1; @n", None),  // ref: 1
        ("@n = nil; @n ||= 5; @n", None),  // ref: 5
        ("@n = 0; [1,2].each { @n += 1 }; @n", None),  // ref: Integer
        ("@n = 1; @n &&= 2; @n", None),  // ref: 2
        ("@n = 1; x = (@n += 1); @n", None),  // ref: 2
        ("@n = 0; while c; @n += 1; end; @n", None),  // ref: Integer
        ("@n = 1; if c; @n += 1; end; @n", None),  // ref: (1 | 2)
        ("@n, x = \"s\", 2; @n", None),  // ref: "s"
        ("@n = 1; x, @n = 2, 3; @n", None),  // ref: 3
        ("@n = 1; @n, = 2; @n", None),  // ref: 2
        ("@n = 1; (@n, x), y = [2, 3], 4; @n", None),  // ref: 2
        ("@n = 1; *@n, x = 2, 3; @n", None),  // ref: [2]
        ("@n = 1; self.a, x = 2, 3; @n", None),  // ref: 1
        ("@n = 1; foo; @n", None),  // ref: (0 | 1 | 2 | 3 | 5 | Array[untyped] | Hash[untyped, untyped] | Integer | String | nil)
        ("@n = 1; self.foo; @n", None),  // ref: (0 | 1 | 2 | 3 | 5 | Array[untyped] | Hash[untyped, untyped] | Integer | String | nil)
        ("@n = 1; reset!; @n", None),  // ref: (0 | 1 | 2 | 3 | 5 | Array[untyped] | Hash[untyped, untyped] | Integer | String | nil)
        ("@n = 1; yield; @n", None),  // ref: (0 | 1 | 2 | 3 | 5 | Array[untyped] | Hash[untyped, untyped] | Integer | String | nil)
        ("@n = 1; super; @n", None),  // ref: (0 | 1 | 2 | 3 | 5 | Array[untyped] | Hash[untyped, untyped] | Integer | String | nil)
        ("@n = 1; super(1); @n", None),  // ref: (0 | 1 | 2 | 3 | 5 | Array[untyped] | Hash[untyped, untyped] | Integer | String | nil)
        ("@n = 1; send(:m); @n", None),  // ref: (0 | 1 | 2 | 3 | 5 | Array[untyped] | Hash[untyped, untyped] | Integer | String | nil)
        ("@n = 1; instance_variable_set(:@n, \"s\"); @n", None),  // ref: "s"
        ("@n = 1; c.call; @n", None),  // ref: (0 | 1 | 2 | 3 | 5 | Array[untyped] | Hash[untyped, untyped] | Integer | String | nil)
        ("@n = 1; c.(); @n", None),  // ref: (0 | 1 | 2 | 3 | 5 | Array[untyped] | Hash[untyped, untyped] | Integer | String | nil)
        ("@n = 1; self.n = 5; @n", None),  // ref: (0 | 1 | 2 | 3 | 5 | Array[untyped] | Hash[untyped, untyped] | Integer | String | nil)
        ("@n = 1; self[:a] = 1; @n", None),  // ref: (0 | 1 | 2 | 3 | 5 | Array[untyped] | Hash[untyped, untyped] | Integer | String | nil)
        ("@n = 1; self.n += 1; @n", None),  // ref: 1
        ("@n = 1; puts 1; @n", None),  // ref: 1
        ("@n = 1; foo(@n); @n", None),  // ref: (0 | 1 | 2 | 3 | 5 | Array[untyped] | Hash[untyped, untyped] | Integer | String | nil)
        ("@n = 1; x = (foo; 2); @n", None),  // ref: (0 | 1 | 2 | 3 | 5 | Array[untyped] | Hash[untyped, untyped] | Integer | String | nil)
        ("@n = 1; [foo, @n]", None),  // ref: [untyped, 1]
        ("@n = 1; foo; x = @n; x", None),  // ref: (0 | 1 | 2 | 3 | 5 | Array[untyped] | Hash[untyped, untyped] | Integer | String | nil)
        ("@n = 1; if c; foo; end; @n", None),  // ref: (0 | 1 | 2 | 3 | 5 | Array[untyped] | Hash[untyped, untyped] | Integer | String | nil)
        ("@n = 1; xs.each { foo }; @n", None),  // ref: (0 | 1 | 2 | 3 | 5 | Array[untyped] | Hash[untyped, untyped] | Integer | String | nil)
        ("@n = 1; defined?(@n); @n", None),  // ref: 1
        ("@n = 1; (@n = 2) rescue nil; @n", None),  // ref: (1 | 2)
        ("@n = 1; begin; foo; rescue; end; @n", None),  // ref: (0 | 1 | 2 | 3 | 5 | Array[untyped] | Hash[untyped, untyped] | Integer | String | nil)
        ("@n = 1; while foo; return @n; end; @n = 2; @n", None),  // ref: (0 | 1 | 2 | 3 | 5 | Array[untyped] | Hash[untyped, untyped] | Integer | String | nil)
        ("@n = \"s\"; @n << \"t\"; @n", None),  // ref: String
        ("@n = [1]; @n << 2; @n", None),  // ref: Array[untyped]
        ("@n = {a: 1}; @n[:b] = 2; @n", None),  // ref: Hash[untyped, untyped]
        ("@n = \"s\"; @n.upcase!; @n", None),  // ref: String
        ("@n = \"s\"; foo(@n); @n", None),  // ref: ("s" | 0 | 1 | 2 | 3 | 5 | Array[untyped] | Hash[untyped, untyped] | Integer | String | nil)
        ("@n = \"s\"; xs.foo(@n); @n", None),  // ref: "s"
        ("@n = [1]; q = @n; q << 2; @n", Some("[1]")),
        ("@n = 1; xs.each { @n = 2 }; @n", None),  // ref: None
        ("@n = 1; 3.times { @n = 2 }; @n", None),  // ref: (1 | 2)
        ("@n = 1; l = -> { @n = 2 }; l.call; @n", None),  // ref: None
        ("@n = 1; xs.foo; @n", Some("1")),
        ("@n = 1; xs.foo(self); @n", Some("1")),
        ("@n = nil; @n = 1 if c; @n", Some("(1 | nil)")),
        ("if c; @n = 1; end; @n", None),  // ref: (0 | 1 | 2 | 3 | 5 | Array[untyped] | Hash[untyped, untyped] | Integer | String | nil)
        ("@n = 1; @n = 2 if c; @n", Some("(1 | 2)")),
        ("@@n = 1; @@n += 1; @@n", None),  // ref: 2
        ("$n = 1; $n += 1; $n", None),  // ref: 2
        ("@@n = 1; @@n", None),  // ref: 1
        ("$n = 1; $n", None),  // ref: 1
        ("@@n = 1; @@n = 2 if c; @@n", None),  // ref: (1 | 2)
    ];
    check_def_local_rows("def_local_env_declines_ivar_writes_and_calls_the_replay_does_not_see", rows);
}

#[test]
fn def_local_env_declines_a_decision_on_an_ivar_cvar_or_gvar() {
    // rigor-rs#548: the reference constant-folds a decision on an ivar / cvar / gvar bound earlier in the
    // method (`@n = 1; a = 1; if @n; a = 2; end` reads `2`); the port holds no such fold and declines the
    // names the decision guards.
    // `Some(t)`: the port emits `-> t`, byte-identical to the reference (oracle-probed);
    // `None`: the port declines (the reference's answer is noted) — a coverage loss, never a wrong signature.
    let rows: &[(&str, Option<&str>)] = &[
        ("@n = 1; a = 1; if @n == 1; a = 2; else; a = 3; end; a", None),  // ref: 2
        ("@n = nil; a = 1; if @n; a = 2; end; a", None),  // ref: 1
        ("@n = 1; @n ? 1 : 2", None),  // ref: 1
        ("@n = 1; @n.nil? ? 1 : 2", None),  // ref: 2
        ("@n = 1; a = 1; a = 2 unless @n; a", None),  // ref: 1
        ("@n = 1; a = 1; while @n < 0; a = 2; end; a", None),  // ref: (1 | 2)
        ("@n = 1; a = 1; case @n; when 1 then a = 2; else a = 3; end; a", None),  // ref: (2 | 3)
        ("@n = 1; @n && 3", None),  // ref: 3
        ("@n = nil; @n || 3", None),  // ref: 3
        ("@n = 1; @n&.to_s", None),  // ref: "1"
        ("@n = 1; a = 1; if (@n += 1) > 1; a = 2; end; a", None),  // ref: 2
        ("@n = 1; a = 1; if foo; a = 2; end; a", None),  // ref: (1 | 2)
        ("@@n = 1; a = 1; if @@n == 1; a = 2; else; a = 3; end; a", None),  // ref: 2
        ("$n = 1; a = 1; if $n == 1; a = 2; else; a = 3; end; a", None),  // ref: 2
        ("@@n = nil; a = 1; if @@n; a = 2; end; a", None),  // ref: 1
        ("$n = nil; a = 1; if $n; a = 2; end; a", None),  // ref: 1
        ("@@n = 1; @@n ? 1 : 2", None),  // ref: 1
        ("$n = 1; $n ? 1 : 2", None),  // ref: 1
        ("a = 1; if @@n; a = 2; end; a", Some("(1 | 2)")),
        ("a = 1; if $stdout; a = 2; end; a", Some("(1 | 2)")),
        ("a = 1; if @n; a = 2; end; a", Some("(1 | 2)")),
    ];
    check_def_local_rows("def_local_env_declines_a_decision_on_an_ivar_cvar_or_gvar", rows);
}

#[test]
fn def_local_env_binds_a_rescue_reference() {
    // rigor-rs#548: `rescue => e` binds `e` to StandardError inside its clause, `rescue T1, T2 => e` to the
    // union of the listed (non-generic) core classes; a generic class (`KeyError[K, R]`), a path, a splat, a
    // shadowed or unknown name declines. `e` itself past the construct reads `(… | nil)` on the reference: declined.
    // `Some(t)`: the port emits `-> t`, byte-identical to the reference (oracle-probed);
    // `None`: the port declines (the reference's answer is noted) — a coverage loss, never a wrong signature.
    let rows: &[(&str, Option<&str>)] = &[
        ("a = 1; begin; puts; rescue => e; a = e; end; a", Some("(1 | StandardError)")),
        ("a = 1; begin; puts; rescue StandardError => e; a = e; end; a", Some("(1 | StandardError)")),
        ("a = 1; begin; puts; rescue ArgumentError => e; a = e; end; a", Some("(1 | ArgumentError)")),
        ("a = 1; begin; puts; rescue TypeError, ArgumentError => e; a = e; end; a", Some("(1 | ArgumentError | TypeError)")),
        ("a = 1; begin; puts; rescue IOError => e; a = e; end; a", Some("(1 | IOError)")),
        ("a = 1; begin; puts; rescue ZeroDivisionError => e; a = e; end; a", Some("(1 | ZeroDivisionError)")),
        ("a = 1; begin; puts; rescue RuntimeError => e; a = e; end; a", Some("(1 | RuntimeError)")),
        ("a = 1; begin; puts; rescue IndexError => e; a = e; end; a", Some("(1 | IndexError)")),
        ("a = 1; begin; puts; rescue => e; a = e.message; end; a", Some("(1 | String)")),
        ("a = 1; begin; puts; rescue => e; a = [e]; end; a", Some("(1 | [StandardError])")),
        ("a = 1; begin; puts; rescue ArgumentError => e; a = e; rescue TypeError => f; a = f; end; a", Some("(1 | ArgumentError | TypeError)")),
        ("a = 1; begin; puts; rescue ArgumentError => e; a = e; ensure; a = 2; end; a", Some("2")),
        ("a = 1; begin; puts; rescue KeyError => e; a = e; end; a", None),  // ref: (1 | KeyError[untyped, untyped])
        ("a = 1; begin; puts; rescue NameError => e; a = e; end; a", None),  // ref: (1 | NameError[untyped])
        ("a = 1; begin; puts; rescue NoMethodError => e; a = e; end; a", None),  // ref: (1 | NoMethodError[untyped])
        ("a = 1; begin; puts; rescue FrozenError => e; a = e; end; a", None),  // ref: (1 | FrozenError[untyped])
        ("a = 1; begin; puts; rescue KeyError, NameError => e; a = e; end; a", None),  // ref: (1 | KeyError[untyped, untyped] | NameError[untyped])
        ("a = 1; begin; puts; rescue Foo::Bar => e; a = e; end; a", None),  // ref: None
        ("a = 1; begin; puts; rescue Errno::ENOENT => e; a = e; end; a", None),  // ref: (1 | Errno::ENOENT)
        ("a = 1; begin; puts; rescue ArgumentError, *c => e; a = e; end; a", None),  // ref: None
        ("a = 1; begin; puts; rescue (c) => e; a = e; end; a", None),  // ref: None
        ("a = 1; begin; puts; rescue => e; a = e; end; e", None),  // ref: (StandardError | nil)
        ("begin; puts; rescue => e; e; end", None),  // ref: None
        ("begin; puts; 1; rescue => e; e; end", None),  // ref: None
        ("a = 1; begin; puts; rescue ArgumentError => e; @n = e; end; @n", None),  // ref: None
        ("a = 1; begin; puts; rescue => e; a = e; retry if c; end; a", None),  // ref: (1 | StandardError)
        ("a = 1; begin; puts; rescue => e; a = e; else; a = 2; end; a", None),  // ref: (2 | StandardError)
        ("a = 1; begin; puts; rescue => e; a = e; end while c; a", None),  // ref: (1 | StandardError)
        ("a = 1; xs.each { begin; puts; rescue => e; a = e; end }; a", None),  // ref: None
        ("a = 1; while c; begin; puts; rescue => e; a = e; end; end; a", Some("(1 | StandardError)")),
        ("a = 1; begin; puts; rescue => e; return e; end; a", Some("(1 | StandardError)")),
        ("e = 5; begin; puts; rescue => e; a = 1; end; e", None),  // ref: (5 | StandardError)
        ("a = 1; (puts) rescue (a = 3); a", None),  // ref: (1 | 3)
        ("a = 1; begin; a = 2; rescue => e; a = e; end; a", Some("(2 | StandardError)")),
        ("a = 1; begin; puts; rescue; a = 2; end; a", Some("(1 | 2)")),
    ];
    check_def_local_rows("def_local_env_binds_a_rescue_reference", rows);
}
