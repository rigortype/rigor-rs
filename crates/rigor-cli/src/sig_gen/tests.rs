use super::*;

fn candidates_tagged(tag: &str, src: &str, include_private: bool) -> Vec<Candidate> {
    // Write to a UNIQUE temp file per test so parallel runs never race on a
    // shared path (write to `generate_file`'s read path is the point).
    let dir = std::env::temp_dir().join(format!("rigor_siggen_test_{tag}"));
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("t.rb");
    std::fs::write(&file, src).unwrap();
    // An EMPTY sig env: no project RBS ⇒ every candidate is `new_method`
    // (these tests exercise inference/rendering, not env classification —
    // that has its own `sig_env` unit tests + the oracle E2E gate).
    let env = SigEnv::build(&[]);
    let out = generate_file(file.to_str().unwrap(), include_private, &env);
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
    let out = generate_file(rb.to_str().unwrap(), false, &env);
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
    // Probe N: an EMPTY `class Foo` in sig ⇒ `hash` resolves through Object ⇒
    // tighter, was: Integer.
    let cs = candidates_with_sig("empty_decl", "class Foo\n  def hash\n    1\n  end\nend\n", "class Foo\nend\n");
    assert_eq!(cs[0].classification, "tighter_return");
    assert_eq!(cs[0].declared_return_rbs.as_deref(), Some("Integer"));
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
    assert_eq!(cs[0].class_name, "M::Foo");
    assert_eq!(cs[0].classification, "tighter_return");
    assert_eq!(cs[0].declared_return_rbs.as_deref(), Some("Integer"));
}

#[test]
fn env_project_superclass_resolves_tighter() {
    // Probe O: `class Foo < Base`, Base declares `greeting: () -> String`.
    let cs = candidates_with_sig(
        "super",
        "class Foo\n  def greeting\n    \"hi\"\n  end\nend\n",
        "class Base\n  def greeting: () -> String\nend\n\nclass Foo < Base\nend\n",
    );
    assert_eq!(cs[0].classification, "tighter_return");
    assert_eq!(cs[0].declared_return_rbs.as_deref(), Some("String"));
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
    // Probe I: `attr_reader name: String` ⇒ `def name` tighter, was: String.
    let cs = candidates_with_sig("attr", "class Foo\n  def name\n    \"n\"\n  end\nend\n", "class Foo\n  attr_reader name: String\nend\n");
    assert_eq!(cs[0].classification, "tighter_return");
    assert_eq!(cs[0].declared_return_rbs.as_deref(), Some("String"));
}

#[test]
fn env_singleton_own_and_inherited() {
    // Probe Q2: own `def self.build: () -> Integer` ⇒ tighter.
    let own = candidates_with_sig(
        "sing_own",
        "class Foo\n  def self.build\n    1\n  end\nend\n",
        "class Foo\n  def self.build: () -> Integer\nend\n",
    );
    assert_eq!(own[0].classification, "tighter_return");
    assert_eq!(own[0].kind, "singleton");
    assert_eq!(own[0].declared_return_rbs.as_deref(), Some("Integer"));

    // Probe Q3: `def self.hash` on an empty `class Foo` inherits `Object#hash`
    // through the class object's `Class`/`Module`/`Object` ancestry.
    let inh = candidates_with_sig(
        "sing_inh",
        "class Foo\n  def self.hash\n    1\n  end\nend\n",
        "class Foo\nend\n",
    );
    assert_eq!(inh[0].classification, "tighter_return");
    assert_eq!(inh[0].declared_return_rbs.as_deref(), Some("Integer"));
}

#[test]
fn env_literal_tightening_emits_but_computed_drops() {
    // A directly-authored literal tail tightens; a computed constant drops.
    let lit = candidates_with_sig("lit", "class Foo\n  def m\n    1\n  end\nend\n", "class Foo\n  def m: () -> Integer\nend\n");
    assert_eq!(lit[0].classification, "tighter_return");
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
    // A genuine tightening still emits (declared String, inferred "hi").
    let ok = candidates_with_sig("tight", "class Foo\n  def m\n    \"hi\"\n  end\nend\n", "class Foo\n  def m: () -> String\nend\n");
    assert_eq!(ok[0].classification, "tighter_return");
    assert_eq!(ok[0].declared_return_rbs.as_deref(), Some("String"));
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
fn block_return_is_barriered_and_multi_return_skips() {
    // A return inside a block is barriered (reference RETURN_BARRIER_NODES —
    // union is tail-only); a multi-value return skips the method (the
    // reference silently drops its type, an unsound emit we do not adopt).
    let src = "class A\n  def b(x)\n    [1].each { return 5 }\n    \"s\"\n  end\n  def m(x)\n    return 1, 2 if x\n    \"s\"\n  end\nend\n";
    let cs = candidates_tagged("blockret", src, false);
    assert_eq!(cs.len(), 1, "only the block-barriered method emits: {cs:?}");
    assert_eq!(cs[0].rbs, "def b: (untyped) -> \"s\"");
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
    let cs = candidates_tagged("hdrsuper", "class Foo < Bar\n  def m\n    \"m\"\n  end\nend\n", false);
    assert_eq!(cs[0].decl_header, "class Foo < Bar");
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
    };
    let mut info = NamespaceInfo::default();
    info.kinds.insert("Outer".into(), "module");
    info.kinds.insert("Outer::Inner".into(), "class");
    info.supers.insert("Outer::Inner".into(), "Base".into());
    let out = render_new_file(&[cand("Outer::Inner", "def m: () -> :s")], &[], &info);
    assert_eq!(out, "module Outer\n  class Inner < Base\n    def m: () -> :s\n  end\nend\n");
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
        ("class E\n  def initialize(*a, **o, &b)\n    @a = a\n  end\nend\n", "def initialize: (*untyped, **untyped, ?{ (?) -> void }) -> void"),
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
fn merge_inserts_new_method_nested_reproduces_indent_quirk() {
    // The token-start splice + fixed 2-space indent: the inserted line renders
    // 4-space and the inner `end` drops to column 0 (oracle-verified).
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
        "module Outer\n  class Inner\n    def existing: () -> String\n    def newm: () -> 5\nend\nend\n"
    );
}

#[test]
fn merge_equivalent_conflict_drops_silently() {
    // Same declared return → dropped: not applied, not skipped → noop.
    let src = "class Foo\n  def greeting: () -> \"hi\"\nend\n".to_string();
    let out = apply_merge(
        src.clone(),
        vec![mc("Foo", "greeting", "instance", "def greeting: () -> \"hi\"")],
        &HashMap::new(),
        false,
    );
    assert_eq!(out.action, "noop");
    assert!(out.applied.is_empty() && out.skipped.is_empty());
    assert_eq!(out.source, src, "byte-untouched when everything drops");
}

#[test]
fn merge_different_conflict_skips_user_authored_with_declared_return() {
    let src = "class Foo\n  def greeting: () -> String\nend\n".to_string();
    let out = apply_merge(
        src.clone(),
        vec![mc("Foo", "greeting", "instance", "def greeting: () -> \"hi\"")],
        &HashMap::new(),
        false,
    );
    assert_eq!(out.action, "noop"); // nothing applied
    assert_eq!(out.skipped.len(), 1);
    assert_eq!(out.skipped[0].classification, "tighter_return");
    assert_eq!(out.skipped[0].declared_return_rbs.as_deref(), Some("String"));
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
    // attr_reader name: String vs candidate "n" → different → skipped.
    assert_eq!(out.skipped.len(), 1);
    assert_eq!(out.skipped[0].declared_return_rbs.as_deref(), Some("String"));
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
        "class Foo\n  def existing: () -> String\nend\n\nclass Bar < Base\n  def added: () -> 7\nend\n"
    );
}

#[test]
fn append_new_class_qualified_name_stays_compact() {
    // A class not in the file with a qualified name uses `class A::B`, NOT
    // nested modules.
    let src = "class Foo\nend\n".to_string();
    let out = apply_merge(
        src,
        vec![mc("A::B", "m", "instance", "def m: () -> 1")],
        &HashMap::new(),
        false,
    );
    assert_eq!(out.source, "class Foo\nend\n\nclass A::B\n  def m: () -> 1\nend\n");
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
