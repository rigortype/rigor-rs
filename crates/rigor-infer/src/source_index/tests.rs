use super::*;
use rigor_parse::{lower, parse, LoweredAst};
use rigor_types::Interner;
use rigor_index::CoreIndex;

fn lower_src(src: &[u8]) -> LoweredAst {
    lower(&parse(src))
}

/// Build a PROJECT index over one source string.
fn build_one(src: &[u8], core: &CoreIndex) -> (LoweredAst, SourceIndex) {
    let ast = lower_src(src);
    let idx = SourceIndex::build(&ast, core);
    (ast, idx)
}

// --- tier-4b positive: tail types to a concrete core class ---------------

#[test]
fn infers_interpolation_return_as_string() {
    // `def full_name; "#{first} #{last}"; end` — the tail is an interpolated
    // String, which always types String ⇒ ("User","full_name") -> "String".
    let core = CoreIndex::new();
    let (_ast, idx) = build_one(
        b"class User\n  def full_name\n    \"#{first} #{last}\"\n  end\nend\n",
        &core,
    );
    assert_eq!(idx.method_return("User", "full_name"), Some("String"));
}

#[test]
fn infers_integer_and_array_literal_returns() {
    let core = CoreIndex::new();
    let (_a, idx) = build_one(b"class C\n  def n\n    42\n  end\n  def a\n    [1, 2]\n  end\nend\n", &core);
    assert_eq!(idx.method_return("C", "n"), Some("Integer"));
    assert_eq!(idx.method_return("C", "a"), Some("Array"));
}

#[test]
fn infers_core_call_tail_return() {
    // `def shout; "x".upcase; end` — `"x".upcase` folds to a String constant,
    // whose class is String ⇒ "String".
    let core = CoreIndex::new();
    let (_a, idx) = build_one(b"class C\n  def shout\n    \"x\".upcase\n  end\nend\n", &core);
    assert_eq!(idx.method_return("C", "shout"), Some("String"));
}

#[test]
fn infers_cross_file_return() {
    // A class defined in ast[0] is inferred even though it is `.new`'d in
    // ast[1]; the return map is keyed by NAME, so it is cross-file safe.
    let core = CoreIndex::new();
    let a0 = lower_src(b"class User\n  def full_name\n    \"#{a} #{b}\"\n  end\nend\n");
    let a1 = lower_src(b"u = User.new\nu.full_name.lenght\n");
    let idx = SourceIndex::build_project(&[&a0, &a1], &core);
    assert_eq!(idx.method_return("User", "full_name"), Some("String"));
}

// --- tier-4b negative: no entry under the gates --------------------------

#[test]
fn param_dependent_body_declines() {
    // `def n(x); x; end` — `x` is an unbound param ⇒ Dynamic ⇒ no entry.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(b"class C\n  def n(x)\n    x\n  end\nend\n", &core);
    assert_eq!(idx.method_return("C", "n"), None);
}

#[test]
fn ivar_body_declines() {
    // `def name; @name; end` — an ivar read types Dynamic ⇒ no entry.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(b"class C\n  def name\n    @name\n  end\nend\n", &core);
    assert_eq!(idx.method_return("C", "name"), None);
}

#[test]
fn explicit_return_declines() {
    // Any explicit `return` ⇒ decline even if the tail would type.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"class C\n  def m\n    return \"e\" if x\n    \"ok\"\n  end\nend\n",
        &core,
    );
    assert_eq!(idx.method_return("C", "m"), None);
}

#[test]
fn conditional_tail_declines() {
    // The tail is an `if` expression (branch carrier) ⇒ decline.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"class C\n  def m\n    if x\n      \"a\"\n    else\n      \"b\"\n    end\n  end\nend\n",
        &core,
    );
    assert_eq!(idx.method_return("C", "m"), None);
}

#[test]
fn in_source_method_call_tail_declines() {
    // `def wrapper; other; end` calling another in-source (implicit-self)
    // method ⇒ Dynamic under the empty env ⇒ decline.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"class C\n  def other\n    \"x\"\n  end\n  def wrapper\n    other\n  end\nend\n",
        &core,
    );
    assert_eq!(idx.method_return("C", "wrapper"), None);
}

#[test]
fn disagreeing_reopened_defs_decline() {
    // `class C; def m; "s"; end; end` reopened with `def m; 1; end` —
    // String vs Integer disagree ⇒ the entry is removed (decline).
    let core = CoreIndex::new();
    let a0 = lower_src(b"class C\n  def m\n    \"s\"\n  end\nend\n");
    let a1 = lower_src(b"class C\n  def m\n    1\n  end\nend\n");
    let idx = SourceIndex::build_project(&[&a0, &a1], &core);
    assert_eq!(idx.method_return("C", "m"), None);
}

#[test]
fn agreeing_reopened_defs_keep() {
    // Same return twice ⇒ keep.
    let core = CoreIndex::new();
    let a0 = lower_src(b"class C\n  def m\n    \"s\"\n  end\nend\n");
    let a1 = lower_src(b"class C\n  def m\n    \"t\"\n  end\nend\n");
    let idx = SourceIndex::build_project(&[&a0, &a1], &core);
    assert_eq!(idx.method_return("C", "m"), Some("String"));
}

// --- tier-4b call-site PARAMETER BINDING descriptors ---------------------

#[test]
fn passthrough_param_records_bound_return() {
    // `def full(x); x; end` — the tail is a bare read of positional param 0,
    // so it records a param-bound descriptor (index 0, empty chain) and NO
    // param-independent return (the param is Dynamic under the empty env).
    let core = CoreIndex::new();
    let (_a, idx) = build_one(b"class C\n  def full(x)\n    x\n  end\nend\n", &core);
    assert_eq!(idx.method_return("C", "full"), None);
    assert_eq!(
        idx.param_bound_return("C", "full"),
        Some(&ParamBoundReturn { param_index: 0, chain: vec![] })
    );
}

#[test]
fn second_param_records_correct_index() {
    // `def pick(a, b); b; end` — the tail reads the SECOND positional param,
    // so the descriptor binds index 1.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(b"class C\n  def pick(a, b)\n    b\n  end\nend\n", &core);
    assert_eq!(
        idx.param_bound_return("C", "pick"),
        Some(&ParamBoundReturn { param_index: 1, chain: vec![] })
    );
}

#[test]
fn core_transform_param_records_chain() {
    // `def up(x); x.upcase.strip; end` — a no-arg core chain rooted at param
    // 0 records `{ index: 0, chain: ["upcase", "strip"] }` (apply order).
    let core = CoreIndex::new();
    let (_a, idx) =
        build_one(b"class C\n  def up(x)\n    x.upcase.strip\n  end\nend\n", &core);
    assert_eq!(
        idx.param_bound_return("C", "up"),
        Some(&ParamBoundReturn {
            param_index: 0,
            chain: vec!["upcase".into(), "strip".into()]
        })
    );
}

#[test]
fn splat_param_declines_binding() {
    // `def f(*xs); xs; end` — a splat breaks the positional index map ⇒ no
    // param-bound entry (and `xs` is param-rooted, so no independent entry).
    let core = CoreIndex::new();
    let (_a, idx) = build_one(b"class C\n  def f(*xs)\n    xs\n  end\nend\n", &core);
    assert_eq!(idx.param_bound_return("C", "f"), None);
    assert_eq!(idx.method_return("C", "f"), None);
}

#[test]
fn kwarg_param_declines_binding() {
    // `def f(x, k:); x; end` — a keyword param ⇒ decline (params == None).
    let core = CoreIndex::new();
    let (_a, idx) = build_one(b"class C\n  def f(x, k:)\n    x\n  end\nend\n", &core);
    assert_eq!(idx.param_bound_return("C", "f"), None);
}

#[test]
fn default_param_declines_binding() {
    // `def f(x = 1); x; end` — an optional (defaulted) param ⇒ decline.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(b"class C\n  def f(x = 1)\n    x\n  end\nend\n", &core);
    assert_eq!(idx.param_bound_return("C", "f"), None);
}

#[test]
fn block_param_declines_binding() {
    // `def f(x, &blk); x; end` — a block param ⇒ decline.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(b"class C\n  def f(x, &blk)\n    x\n  end\nend\n", &core);
    assert_eq!(idx.param_bound_return("C", "f"), None);
}

#[test]
fn chain_with_args_declines_binding() {
    // `def f(x); x.fetch(0); end` — a chain step that carries an argument is
    // not a no-arg core call ⇒ decline (we bind only the root param).
    let core = CoreIndex::new();
    let (_a, idx) =
        build_one(b"class C\n  def f(x)\n    x.fetch(0)\n  end\nend\n", &core);
    assert_eq!(idx.param_bound_return("C", "f"), None);
}

#[test]
fn non_param_root_tail_declines_binding() {
    // `def f(x); @y.upcase; end` — the chain root is an ivar, not a param ⇒
    // no param-bound entry.
    let core = CoreIndex::new();
    let (_a, idx) =
        build_one(b"class C\n  def f(x)\n    @y.upcase\n  end\nend\n", &core);
    assert_eq!(idx.param_bound_return("C", "f"), None);
}

#[test]
fn explicit_return_declines_param_binding() {
    // An explicit `return` ⇒ decline even for a param-rooted tail.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"class C\n  def f(x)\n    return x if x\n    x\n  end\nend\n",
        &core,
    );
    assert_eq!(idx.param_bound_return("C", "f"), None);
}

#[test]
fn disagreeing_reopened_param_bound_declines() {
    // `def m(x); x; end` reopened with `def m(a, b); b; end` — index 0 vs 1
    // disagree ⇒ the param-bound entry is removed.
    let core = CoreIndex::new();
    let a0 = lower_src(b"class C\n  def m(x)\n    x\n  end\nend\n");
    let a1 = lower_src(b"class C\n  def m(a, b)\n    b\n  end\nend\n");
    let idx = SourceIndex::build_project(&[&a0, &a1], &core);
    assert_eq!(idx.param_bound_return("C", "m"), None);
}

// --- ADR-35 slice 1: override-visibility ancestor walk -------------------

#[test]
fn method_visibility_reads_own_table() {
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"class C\n  def a\n  end\n  private\n  def b\n  end\nend\n",
        &core,
    );
    assert_eq!(idx.method_visibility("C", "a"), Some(Visibility::Public));
    assert_eq!(idx.method_visibility("C", "b"), Some(Visibility::Private));
    assert_eq!(idx.method_visibility("C", "missing"), None);
}

#[test]
fn nearest_ancestor_walks_superclass() {
    // B < A; A defines `foo` (public). The nearest ancestor of B defining
    // `foo` is A with Public.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"class A\n  def foo\n  end\nend\nclass B < A\n  private\n  def foo\n  end\nend\n",
        &core,
    );
    assert_eq!(
        idx.nearest_ancestor_defining("B", "foo"),
        Some(("A".to_string(), Some(Visibility::Public)))
    );
}

#[test]
fn nearest_ancestor_prefers_included_module_over_superclass() {
    // B includes M and is < A; both define `foo`. MRO ⇒ the included module
    // M is the nearest ancestor.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"module M\n  def foo\n  end\nend\nclass A\n  def foo\n  end\nend\nclass B < A\n  include M\n  def bar\n  end\nend\n",
        &core,
    );
    assert_eq!(
        idx.nearest_ancestor_defining("B", "foo"),
        Some(("M".to_string(), Some(Visibility::Public)))
    );
}

#[test]
fn nearest_ancestor_none_when_no_project_ancestor_defines() {
    // B < A but A does not define `foo` ⇒ no defining ancestor.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"class A\n  def other\n  end\nend\nclass B < A\n  def foo\n  end\nend\n",
        &core,
    );
    assert_eq!(idx.nearest_ancestor_defining("B", "foo"), None);
}

#[test]
fn nearest_ancestor_skips_rbs_third_party_super() {
    // `class B < ApplicationRecord` — the super is not a project source class
    // ⇒ dropped ⇒ no defining ancestor (RBS-ancestor carve-out).
    let core = CoreIndex::new();
    let (_a, idx) =
        build_one(b"class B < ApplicationRecord\n  private\n  def foo\n  end\nend\n", &core);
    assert_eq!(idx.nearest_ancestor_defining("B", "foo"), None);
}

#[test]
fn nearest_ancestor_returns_unknown_visibility_for_methods_only_entry() {
    // The keystone path: an ancestor that DEFINES the method (in `methods`)
    // but has NO visibility-table entry returns `(ancestor, None)` — the rule
    // layer must NOT synthesize Public from this. We construct a methods-only
    // entry directly (the public lowering keeps the two tables in lockstep, so
    // this exercises the data path that the "never synthesize Public" gate
    // guards against).
    let core = CoreIndex::new();
    let mut idx = SourceIndex::build(&lower_src(b"class B < A\n  def foo\n  end\nend\n"), &core);
    // Seed override class `A` with `foo` in `methods` only (no vis entry).
    idx.override_classes.insert(
        "A".to_string(),
        OverrideClass {
            superclass: None,
            includes: Vec::new(),
            extends: Vec::new(),
            method_visibilities: HashMap::new(),
            methods: ["foo".to_string()].into_iter().collect(),
            is_module: false,
        },
    );
    assert_eq!(
        idx.nearest_ancestor_defining("B", "foo"),
        Some(("A".to_string(), None))
    );
}

#[test]
fn nearest_ancestor_does_not_merge_namespace_collisions() {
    // The gitlab-foss FP root cause: a controller includes `Groups::Params`
    // (which defines `group_params`, not `group`), while a DIFFERENT
    // `IssuableFinder::Params` defines a private `group`. With lexical
    // qualification the include resolves to `Groups::Params` ONLY, so `group`
    // has no project ancestor here ⇒ None (no phantom override).
    let core = CoreIndex::new();
    let groups_params = lower_src(
        b"module Groups\n  module Params\n    def group_params\n    end\n  end\nend\n",
    );
    let finder_params = lower_src(
        b"module IssuableFinder\n  module Params\n    private\n    def group\n    end\n  end\nend\n",
    );
    let controller = lower_src(
        b"module Organizations\n  class GroupsController\n    include Groups::Params\n    private\n    def group\n    end\n  end\nend\n",
    );
    let idx = SourceIndex::build_project(
        &[&groups_params, &finder_params, &controller],
        &core,
    );
    // The controller's `group` has NO project ancestor defining it (the
    // included `Groups::Params` lacks `group`; `IssuableFinder::Params` is not
    // an ancestor) ⇒ silent. This is the precise zero-FP guarantee.
    assert_eq!(
        idx.nearest_ancestor_defining("Organizations::GroupsController", "group"),
        None
    );
}

#[test]
fn nearest_ancestor_resolves_namespaced_include_path() {
    // `include Groups::Params` from a class in a different namespace resolves
    // to the fully-qualified `Groups::Params` (which DOES define the method).
    let core = CoreIndex::new();
    let m = lower_src(b"module Groups\n  module Params\n    def gp\n    end\n  end\nend\n");
    let c = lower_src(
        b"module Organizations\n  class Ctrl\n    include Groups::Params\n    private\n    def gp\n    end\n  end\nend\n",
    );
    let idx = SourceIndex::build_project(&[&m, &c], &core);
    assert_eq!(
        idx.nearest_ancestor_defining("Organizations::Ctrl", "gp"),
        Some(("Groups::Params".to_string(), Some(Visibility::Public)))
    );
}

#[test]
fn nearest_ancestor_cross_file_via_build_project() {
    // Parent A in file 0, subclass B in file 1 — the project build seeds both,
    // so the walk resolves A across files.
    let core = CoreIndex::new();
    let a0 = lower_src(b"class A\n  def foo\n  end\nend\n");
    let a1 = lower_src(b"class B < A\n  private\n  def foo\n  end\nend\n");
    let idx = SourceIndex::build_project(&[&a0, &a1], &core);
    assert_eq!(
        idx.nearest_ancestor_defining("B", "foo"),
        Some(("A".to_string(), Some(Visibility::Public)))
    );
}

#[test]
fn nearest_ancestor_cycle_guarded() {
    // A < B and B < A (pathological cycle) — the walk terminates (None, no
    // panic/loop) when neither defines the method.
    let core = CoreIndex::new();
    let a0 = lower_src(b"class A < B\n  def x\n  end\nend\n");
    let a1 = lower_src(b"class B < A\n  def y\n  end\nend\n");
    let idx = SourceIndex::build_project(&[&a0, &a1], &core);
    assert_eq!(idx.nearest_ancestor_defining("A", "foo"), None);
}

#[test]
fn class_name_for_id_of_recovers_source_name() {
    // A `Nominal` over a source-range id resolves to its class NAME (the
    // companion to the core `class_name_of`, which returns None for it).
    let core = CoreIndex::new();
    let (_a, idx) = build_one(b"class Point\n  def x\n    1\n  end\nend\n", &core);
    let mut i = Interner::new();
    let class = idx.class_id("Point").expect("Point registered");
    let ty = i.intern(rigor_types::Type::Nominal { class, args: vec![] });
    assert_eq!(idx.class_name_for_id_of(&i, ty), Some("Point"));
    // A Dynamic carrier ⇒ None.
    let u = i.untyped();
    assert_eq!(idx.class_name_for_id_of(&i, u), None);
}

// --- ADR-0038 interprocedural literal-tail fold ---------------------------

/// Build a PROJECT index over N source strings.
fn build_many(srcs: &[&[u8]], core: &CoreIndex) -> SourceIndex {
    let asts: Vec<LoweredAst> = srcs.iter().map(|s| lower_src(s)).collect();
    let refs: Vec<&LoweredAst> = asts.iter().collect();
    SourceIndex::build_project(&refs, core)
}

#[test]
fn const_singleton_bare_literal_folds() {
    // `module M; def self.ro?; false; end; end` ⇒ `M.ro?` folds to false.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(b"module M\n  def self.ro?\n    false\n  end\nend\n", &core);
    assert_eq!(idx.const_singleton_literal("M", "ro?"), Some(Scalar::Bool(false)));
}

#[test]
fn const_singleton_class_receiver_folds() {
    // A CLASS (not just a module) singleton call folds too.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(b"class K\n  def self.on?\n    true\n  end\nend\n", &core);
    assert_eq!(idx.const_singleton_literal("K", "on?"), Some(Scalar::Bool(true)));
}

#[test]
fn qualified_const_receiver_folds_stripping_leading_colons() {
    // `module Gitlab; module Database; def self.read_only?; false` keys the
    // fold at the QUALIFIED owner `Gitlab::Database`, matched by the dotted
    // receiver (with or without a leading `::`).
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"module Gitlab\n  module Database\n    def self.read_only?\n      false\n    end\n  end\nend\n",
        &core,
    );
    assert_eq!(
        idx.const_singleton_literal("Gitlab::Database", "read_only?"),
        Some(Scalar::Bool(false))
    );
    assert_eq!(
        idx.const_singleton_literal("::Gitlab::Database", "read_only?"),
        Some(Scalar::Bool(false))
    );
}

#[test]
fn depth_two_bang_of_singleton_call_folds() {
    // `read_write? = !read_only?` — the tail `!read_only?` resolves the
    // OWN-CLASS singleton `read_only?` (false) and inverts it ⇒ true.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"module Gitlab\n  module Database\n    def self.read_only?\n      false\n    end\n    def self.read_write?\n      !read_only?\n    end\n  end\nend\n",
        &core,
    );
    assert_eq!(
        idx.const_singleton_literal("Gitlab::Database", "read_write?"),
        Some(Scalar::Bool(true))
    );
}

#[test]
fn cross_owner_const_call_declines() {
    // `Bar` defines `read_only?`; `Foo` does not. A `Foo.read_only?` fold must
    // DECLINE (own-class resolution — a same-name method elsewhere is never
    // adopted), even though `read_only?` has exactly one project definer.
    let core = CoreIndex::new();
    let idx = build_many(
        &[
            b"class Foo\nend\n",
            b"module Bar\n  def self.read_only?\n    false\n  end\nend\n",
        ],
        &core,
    );
    assert_eq!(idx.const_singleton_literal("Foo", "read_only?"), None);
    assert_eq!(idx.const_singleton_literal("Bar", "read_only?"), Some(Scalar::Bool(false)));
}

#[test]
fn implicit_self_same_class_instance_folds() {
    // `def flag; false; end` resolves an implicit-self `flag` in the SAME
    // class to false.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(b"class Widget\n  def flag\n    false\n  end\nend\n", &core);
    assert_eq!(
        idx.implicit_self_literal("Widget", DefKind::Instance, "flag"),
        Some(Scalar::Bool(false))
    );
}

#[test]
fn implicit_self_inherited_instance_folds() {
    // `class User < Base; Base defines flag` — an implicit-self `flag` in User
    // resolves through the ancestry to Base#flag.
    let core = CoreIndex::new();
    let idx = build_many(
        &[
            b"class Base\n  def flag\n    false\n  end\nend\n",
            b"class User < Base\nend\n",
        ],
        &core,
    );
    assert_eq!(
        idx.implicit_self_literal("User", DefKind::Instance, "flag"),
        Some(Scalar::Bool(false))
    );
}

#[test]
fn implicit_self_included_module_folds() {
    // `class User; include Flaggable; Flaggable defines flag` — resolves
    // through the included module.
    let core = CoreIndex::new();
    let idx = build_many(
        &[
            b"module Flaggable\n  def flag\n    false\n  end\nend\n",
            b"class User\n  include Flaggable\nend\n",
        ],
        &core,
    );
    assert_eq!(
        idx.implicit_self_literal("User", DefKind::Instance, "flag"),
        Some(Scalar::Bool(false))
    );
}

#[test]
fn implicit_self_cross_class_declines() {
    // `Widget` defines `flag`; `User` (unrelated) calls it implicitly. Even
    // with a single project definer, the fold DECLINES — `flag` is not in
    // User's ancestry (the cross-class zero-FP keystone).
    let core = CoreIndex::new();
    let idx = build_many(
        &[
            b"class Widget\n  def flag\n    false\n  end\nend\n",
            b"class User\nend\n",
        ],
        &core,
    );
    assert_eq!(idx.implicit_self_literal("User", DefKind::Instance, "flag"), None);
}

#[test]
fn implicit_self_singleton_kind_folds_own_class() {
    // Inside a `def self.check`, an implicit `read_only?` resolves the OWN
    // singleton table.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"module Gitlab\n  module Database\n    def self.read_only?\n      false\n    end\n  end\nend\n",
        &core,
    );
    assert_eq!(
        idx.implicit_self_literal("Gitlab::Database", DefKind::Singleton, "read_only?"),
        Some(Scalar::Bool(false))
    );
    // The instance table is SEPARATE — no instance `read_only?` exists.
    assert_eq!(
        idx.implicit_self_literal("Gitlab::Database", DefKind::Instance, "read_only?"),
        None
    );
}

#[test]
fn related_subclass_override_degrades_even_when_values_match() {
    // Base#flag = false, Sub < Base overrides flag = false (MATCHING value).
    // The base's literal is the DEFAULT, not what every receiver sees, so it
    // degrades to no-fold (reference `degrade_if_overridable`).
    let core = CoreIndex::new();
    let idx = build_many(
        &[
            b"class Base\n  def flag\n    false\n  end\nend\n",
            b"class Sub < Base\n  def flag\n    false\n  end\nend\n",
        ],
        &core,
    );
    assert_eq!(idx.implicit_self_literal("Base", DefKind::Instance, "flag"), None);
}

#[test]
fn two_unrelated_definers_each_fold() {
    // A and B are UNRELATED modules that each define a singleton `ro? = false`.
    // Neither is an override of the other, so each still folds (the recall the
    // single-definer guard would have lost — the `force_pipeline_creation_to_
    // continue?` pair).
    let core = CoreIndex::new();
    let idx = build_many(
        &[
            b"module A\n  def self.ro?\n    false\n  end\nend\n",
            b"module B\n  def self.ro?\n    false\n  end\nend\n",
        ],
        &core,
    );
    assert_eq!(idx.const_singleton_literal("A", "ro?"), Some(Scalar::Bool(false)));
    assert_eq!(idx.const_singleton_literal("B", "ro?"), Some(Scalar::Bool(false)));
}

#[test]
fn subclass_constant_singleton_declines() {
    // `Sub < Base`, only Base defines singleton `ro?`. A `Sub.ro?` call is an
    // INHERITED singleton — resolution is own-class only, so it declines
    // (reference probe 9: inherited singleton via subclass constant declines).
    let core = CoreIndex::new();
    let idx = build_many(
        &[
            b"class Base\n  def self.ro?\n    false\n  end\nend\n",
            b"class Sub < Base\nend\n",
        ],
        &core,
    );
    assert_eq!(idx.const_singleton_literal("Sub", "ro?"), None);
    assert_eq!(idx.const_singleton_literal("Base", "ro?"), Some(Scalar::Bool(false)));
}

#[test]
fn union_branch_tail_declines() {
    // A method whose tail is an `if`/ternary carrier never folds (a branch
    // carrier has no single scalar leaf in this slice).
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"module M\n  def self.ro?\n    cond ? true : nil\n  end\nend\n",
        &core,
    );
    assert_eq!(idx.const_singleton_literal("M", "ro?"), None);
}

#[test]
fn dynamic_leaf_declines() {
    // A non-literal tail (an unresolved call) declines.
    let core = CoreIndex::new();
    let (_a, idx) =
        build_one(b"module M\n  def self.ro?\n    some_dynamic_thing\n  end\nend\n", &core);
    assert_eq!(idx.const_singleton_literal("M", "ro?"), None);
}

#[test]
fn shape_return_declines() {
    // An array/hash literal tail is not a scalar ⇒ decline.
    let core = CoreIndex::new();
    let (_a, idx) =
        build_one(b"module M\n  def self.ro?\n    [1, 2]\n  end\nend\n", &core);
    assert_eq!(idx.const_singleton_literal("M", "ro?"), None);
}

#[test]
fn explicit_return_declines_fold() {
    // Any explicit `return` in the body declines (we read only the tail).
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"module M\n  def self.ro?\n    return true if x\n    false\n  end\nend\n",
        &core,
    );
    assert_eq!(idx.const_singleton_literal("M", "ro?"), None);
}

#[test]
fn disagreeing_reopen_declines_fold() {
    // The same singleton method reopened with a DIFFERENT literal declines.
    let core = CoreIndex::new();
    let idx = build_many(
        &[
            b"module M\n  def self.ro?\n    false\n  end\nend\n",
            b"module M\n  def self.ro?\n    true\n  end\nend\n",
        ],
        &core,
    );
    assert_eq!(idx.const_singleton_literal("M", "ro?"), None);
}

#[test]
fn recursive_method_declines_fold() {
    // A self-recursive body (`def loopy; loopy; end`) declines via the cycle
    // guard rather than spinning.
    let core = CoreIndex::new();
    let (_a, idx) =
        build_one(b"module M\n  def self.loopy\n    loopy\n  end\nend\n", &core);
    assert_eq!(idx.const_singleton_literal("M", "loopy"), None);
}

#[test]
fn raise_guarded_tail_folds() {
    // A raise-guarded earlier statement leaves the tail literal foldable.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"module M\n  def self.ro?\n    raise \"boom\" if never\n    false\n  end\nend\n",
        &core,
    );
    assert_eq!(idx.const_singleton_literal("M", "ro?"), Some(Scalar::Bool(false)));
}

// --- C1: constant-shadow gate --------------------------------------------

fn seg(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn toplevel_definition_shadows_everywhere() {
    // A toplevel `class Report` suppresses a bare `Report` read at ANY use
    // site (Ruby: a toplevel constant is always reachable).
    let core = CoreIndex::new();
    let (_a, idx) = build_one(b"class Report\nend\n", &core);
    assert!(idx.constant_shadowed("Report", &[]));
    assert!(idx.constant_shadowed("Report", &seg(&["Foo", "Bar"])));
}

#[test]
fn nested_definition_shadows_only_where_lexically_visible() {
    // `module A; module B; module Time; end; end; end` — a bare `Time` read
    // is shadowed inside `A::B::*` but RELAXES (fires) elsewhere.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"module A\n  module B\n    module Time\n    end\n    class C\n    end\n  end\nend\n",
        &core,
    );
    // Visible: the defining namespace and any scope nested within it.
    assert!(idx.constant_shadowed("Time", &seg(&["A", "B"])));
    assert!(idx.constant_shadowed("Time", &seg(&["A", "B", "C"])));
    // NOT visible: a sibling namespace, an outer scope, or the toplevel.
    assert!(!idx.constant_shadowed("Time", &seg(&["A"])));
    assert!(!idx.constant_shadowed("Time", &seg(&["A", "Z"])));
    assert!(!idx.constant_shadowed("Time", &[]));
    // A different bare name the project never defines is never shadowed.
    assert!(!idx.constant_shadowed("Time", &seg(&["Other"])));
}

#[test]
fn harvests_single_literal_constant_lexically() {
    // `class K; R = 1..1024; A = [:a]; N = 42; end` — each is harvested and
    // visible from within `K`, not from an unrelated scope.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"class K\n  A = [1, 2]\n  N = 42\n  S = \"hi\"\nend\n",
        &core,
    );
    // Visible inside `K`.
    assert_eq!(
        idx.literal_constant("N", &seg(&["K"]), _a.file_key()),
        Some(&ConstLit::Scalar(Scalar::Int(42)))
    );
    assert!(matches!(idx.literal_constant("A", &seg(&["K"]), _a.file_key()), Some(ConstLit::Tuple(_))));
    assert_eq!(
        idx.literal_constant("S", &seg(&["K"]), _a.file_key()),
        Some(&ConstLit::Scalar(Scalar::Str("hi".into())))
    );
    // NOT visible from an unrelated namespace or the toplevel.
    assert_eq!(idx.literal_constant("N", &[], _a.file_key()), None);
    assert_eq!(idx.literal_constant("N", &seg(&["Other"]), _a.file_key()), None);
}

/// Upstream #540 (`fc3b8b42`) — a literal-shape constant the FILE mutates is
/// harvested `Dynamic`-wrapped, so a read stops folding through the shape.
#[test]
fn mutated_literal_constants_are_widened() {
    let core = CoreIndex::new();
    let (a, idx) = build_one(
        b"LN = [true]\nUNTOUCHED = [true]\nFROZEN = [true].freeze\n\
          def m\n  LN[0] = false\nend\ndef n\n  UNTOUCHED.each { |x| x }\nend\n",
        &core,
    );
    // The mutated one is wrapped exactly once, around the shape it had.
    assert_eq!(
        idx.literal_constant("LN", &[], a.file_key()),
        Some(&ConstLit::Widened(Box::new(ConstLit::Tuple(vec![ConstLit::Scalar(
            Scalar::Bool(true)
        )]))))
    );
    // A non-mutating call (`each`) and `.freeze` leave the fold alone.
    assert!(matches!(
        idx.literal_constant("UNTOUCHED", &[], a.file_key()),
        Some(ConstLit::Tuple(_))
    ));
    assert!(matches!(
        idx.literal_constant("FROZEN", &[], a.file_key()),
        Some(ConstLit::Tuple(_))
    ));
}

/// The census resolves a BARE receiver through every lexical candidate, and
/// a `A::B` PATH receiver through the name as written. Same-file only.
#[test]
fn mutation_census_candidates_and_file_scope() {
    let core = CoreIndex::new();
    // A bare `T[0] = 1` inside `module Outer` widens `Outer::T` AND a
    // toplevel `T`; the path spelling widens only `Outer::T`.
    let (a, idx) = build_one(
        b"module Outer\n  T = [true]\n  def self.m\n    T[0] = false\n  end\nend\nT = [1]\n",
        &core,
    );
    assert!(matches!(
        idx.literal_constant("T", &seg(&["Outer"]), a.file_key()),
        Some(ConstLit::Widened(_))
    ));
    // A mutation in ANOTHER file must not widen this file's fold.
    let mutator = lower_src(b"def m\n  XF[0] = false\nend\n");
    let holder = lower_src(b"XF = [true]\n");
    let idx = SourceIndex::build_project(&[&holder, &mutator], &core);
    assert!(matches!(
        idx.literal_constant("XF", &[], holder.file_key()),
        Some(ConstLit::Tuple(_))
    ));
}

#[test]
fn cross_namespace_constant_not_folded() {
    // `module Expirable; DAYS = 7; end` — `DAYS` is NOT visible from an
    // unrelated `class Consumer` (the app/models concern-constant FP shape).
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"module Expirable\n  DAYS = 7\nend\nclass Consumer\n  D2 = 9\nend\n",
        &core,
    );
    // Visible only within its own namespace.
    assert_eq!(
        idx.literal_constant("DAYS", &seg(&["Expirable"]), _a.file_key()),
        Some(&ConstLit::Scalar(Scalar::Int(7)))
    );
    assert_eq!(idx.literal_constant("DAYS", &seg(&["Consumer"]), _a.file_key()), None);
    assert_eq!(idx.literal_constant("DAYS", &[], _a.file_key()), None);
}

#[test]
fn multiple_assignment_declines_harvest() {
    // A constant written twice (same qualified name) is ambiguous ⇒ declined.
    let core = CoreIndex::new();
    let (_a, idx) =
        build_one(b"class K\n  M = 1\n  M = 2\nend\n", &core);
    assert_eq!(idx.literal_constant("M", &seg(&["K"]), _a.file_key()), None);
}

#[test]
fn class_name_collision_declines_harvest() {
    // `Widget = [1]` where `class Widget` also exists ⇒ declined (a constant
    // is never a class; the class/source path owns that name).
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"class Widget\nend\nWidget = [1]\n",
        &core,
    );
    assert_eq!(idx.literal_constant("Widget", &[], _a.file_key()), None);
}

#[test]
fn range_constant_harvests_as_range() {
    let core = CoreIndex::new();
    let (_a, idx) = build_one(b"class K\n  R = 1..1024\nend\n", &core);
    assert_eq!(idx.literal_constant("R", &seg(&["K"]), _a.file_key()), Some(&ConstLit::Range));
}

// --- slice B: partially-literal containers ⇒ INERT bare nominals ----------

#[test]
fn partially_literal_containers_harvest_as_bare_nominals() {
    let core = CoreIndex::new();
    let (a, idx) = build_one(
        b"class K\n  LAM = { c: ->(_x) { 1 } }.freeze\n  DYN = [1, unknown_zzz, 2].freeze\n  \
          SPLAT_H = { a: 1, **unknown_zzz }.freeze\n  DYNKEY = { a: 1, unknown_zzz => 2 }.freeze\n  \
          SPLAT_A = [*unknown_zzz].freeze\n  INTERP = [\"a\", \"b#{1}\"].freeze\n  \
          CHAIN = [1, \"x\".upcase].freeze\nend\n",
        &core,
    );
    let k = seg(&["K"]);
    for name in ["LAM", "SPLAT_H", "DYNKEY"] {
        assert_eq!(
            idx.literal_constant(name, &k, a.file_key()),
            Some(&ConstLit::BareHash),
            "{name} should harvest as a bare Hash nominal"
        );
    }
    for name in ["DYN", "SPLAT_A", "INTERP", "CHAIN"] {
        assert_eq!(
            idx.literal_constant(name, &k, a.file_key()),
            Some(&ConstLit::BareArray),
            "{name} should harvest as a bare Array nominal"
        );
    }
}

#[test]
fn fully_literal_harvest_is_unchanged_by_the_widening() {
    // The value-pinned rendering must not regress: a fully-literal container
    // still harvests as Tuple/Hash, and the EMPTY literals keep their
    // zero-size shapes (`[]` / `{}` fold projections off them).
    let core = CoreIndex::new();
    let (a, idx) = build_one(
        b"class K\n  T = [1, 2].freeze\n  H = { a: 1 }.freeze\n  E = [].freeze\n  \
          EH = {}.freeze\n  R = 1..9\nend\n",
        &core,
    );
    let k = seg(&["K"]);
    assert!(matches!(idx.literal_constant("T", &k, a.file_key()), Some(ConstLit::Tuple(v)) if v.len() == 2));
    assert!(matches!(idx.literal_constant("H", &k, a.file_key()), Some(ConstLit::Hash(m)) if m.len() == 1));
    assert_eq!(idx.literal_constant("E", &k, a.file_key()), Some(&ConstLit::Tuple(vec![])));
    assert_eq!(idx.literal_constant("EH", &k, a.file_key()), Some(&ConstLit::Hash(vec![])));
    assert_eq!(idx.literal_constant("R", &k, a.file_key()), Some(&ConstLit::Range));
}

#[test]
fn a_nested_partial_container_degrades_only_the_inner_level() {
    // `{ a: [->(){}] }` keeps its OUTER shape (the key is static) and puts a
    // bare `Array` in the slot — the reference has `{ a: [Proc] }` there, so
    // `H[:a].zzz` fires in both engines (same class, sharper rendering in
    // the oracle). Verified against the oracle in fixture 92.
    let core = CoreIndex::new();
    let (a, idx) = build_one(b"class K\n  N = { a: [->() { 1 }] }.freeze\nend\n", &core);
    let Some(ConstLit::Hash(members)) = idx.literal_constant("N", &seg(&["K"]), a.file_key())
    else {
        panic!("expected an outer Hash shape");
    };
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].1, ConstLit::BareArray);
}

#[test]
fn non_container_rhs_still_declines() {
    // Slice B widens the CONTAINER arms only. A call chain, a bare constant
    // read, a lambda and a `Class.new` still decline (a chain-valued
    // constant is slice C's question, assessed separately).
    let core = CoreIndex::new();
    let (a, idx) = build_one(
        b"class K\n  CH = %w[a b].map.with_index.to_h.freeze\n  CR = OTHER_ZZZ\n  \
          L = ->(_x) { 1 }\n  C = Class.new(StandardError)\nend\n",
        &core,
    );
    let k = seg(&["K"]);
    for name in ["CH", "CR", "L", "C"] {
        assert_eq!(idx.literal_constant(name, &k, a.file_key()), None, "{name}");
    }
}

#[test]
fn multiple_assignment_still_declines_a_partial_container() {
    // The reference UNIONS duplicate assignments; C5's single-assignment
    // gate is a strict under-emit and slice B keeps it unchanged.
    let core = CoreIndex::new();
    let (a, idx) = build_one(
        b"class K\n  M = { c: ->(_x) { 1 } }\n  M = [1, 2]\nend\n",
        &core,
    );
    assert_eq!(idx.literal_constant("M", &seg(&["K"]), a.file_key()), None);
}

// --- slice A: PER-FILE constant-value consumption -------------------------

#[test]
fn constant_value_is_consumed_only_in_the_assigning_file() {
    // The reference rebuilds its in-source constant-value table per file
    // (`ScopeIndexer#build_in_source_constants` walks ONE file's root), so a
    // same-namespace read in another file resolves nothing there. Probed:
    // `module M; class C; L = [1, 2].freeze` in `a.rb`, `L.frobnicate_zzz`
    // inside the same `M::C` in `b.rb` — reference silent, rigor-rs fired.
    let core = CoreIndex::new();
    let a0 = lower_src(b"module M\n  class C\n    L = [1, 2].freeze\n  end\nend\n");
    let a1 = lower_src(b"module M\n  class C\n    def go; L; end\n  end\nend\n");
    let idx = SourceIndex::build_project(&[&a0, &a1], &core);
    // Same file as the assignment: still folds (the harvest is unchanged).
    assert!(matches!(
        idx.literal_constant("L", &seg(&["M", "C"]), a0.file_key()),
        Some(ConstLit::Tuple(_))
    ));
    // The USE file did not assign it ⇒ no value, even though the namespace
    // matches exactly.
    assert_eq!(idx.literal_constant("L", &seg(&["M", "C"]), a1.file_key()), None);
}

#[test]
fn qualified_constant_value_is_consumed_only_in_the_assigning_file() {
    // Stage 2e is a pure SPELLING of the same harvest, so it inherits the
    // per-file gate. `M::C::L` read from inside `M::C` in another file.
    let core = CoreIndex::new();
    let a0 = lower_src(b"module M\n  class C\n    L = [1, 2].freeze\n  end\nend\n");
    let a1 = lower_src(b"module M\n  class C\n    def go; M::C::L; end\n  end\nend\n");
    let idx = SourceIndex::build_project(&[&a0, &a1], &core);
    assert!(matches!(
        idx.qualified_literal_constant("M::C::L", &seg(&["M", "C"]), a0.file_key()),
        Some(ConstLit::Tuple(_))
    ));
    assert_eq!(
        idx.qualified_literal_constant("M::C::L", &seg(&["M", "C"]), a1.file_key()),
        None
    );
}

#[test]
fn toplevel_constant_value_is_still_per_file() {
    // Probed with a `require_relative` in place: the reference is silent on
    // BOTH a container and a scalar read cross-file, so the gate is about
    // the FILE, not about namespace visibility or require reachability.
    let core = CoreIndex::new();
    let a0 = lower_src(b"TOPL = [1, 2].freeze\nSCAL = 5\n");
    let a1 = lower_src(b"require_relative \"a\"\nclass K\n  def go; TOPL; SCAL; end\nend\n");
    let idx = SourceIndex::build_project(&[&a0, &a1], &core);
    assert!(idx.literal_constant("TOPL", &seg(&["K"]), a0.file_key()).is_some());
    assert!(idx.literal_constant("SCAL", &seg(&["K"]), a0.file_key()).is_some());
    assert_eq!(idx.literal_constant("TOPL", &seg(&["K"]), a1.file_key()), None);
    assert_eq!(idx.literal_constant("SCAL", &seg(&["K"]), a1.file_key()), None);
}

#[test]
fn env_negative_check_stays_file_agnostic() {
    // Non-goal guard: slice A must not let the stage-2b `ENV` arm start
    // firing where it used to decline. Its decline predicate is
    // `literal_constant_visible_any_file`, which ignores the use file — a
    // project `ENV = { a: 1 }` in ANOTHER file still declines the arm.
    let core = CoreIndex::new();
    let a0 = lower_src(b"ENV = { a: 1 }.freeze\n");
    let a1 = lower_src(b"class K\n  def go; ENV; end\nend\n");
    let idx = SourceIndex::build_project(&[&a0, &a1], &core);
    // The per-file TYPING gate declines in the reading file …
    assert_eq!(idx.literal_constant("ENV", &seg(&["K"]), a1.file_key()), None);
    // … but the 2b decline predicate still sees it from either file.
    assert!(idx.literal_constant_visible_any_file("ENV", &seg(&["K"])));
    assert!(idx.literal_constant_visible_any_file("ENV", &[]));
    // And the coarser project-write set (the arm's other decline) is
    // unaffected by slice A.
    assert!(idx.project_writes_constant("ENV"));
}

#[test]
fn lexical_scopes_records_qualified_spans() {
    // The per-file lexical scope table qualifies nested class/module bodies
    // so a use-site prefix can be recovered by span containment.
    let ast = lower_src(
        b"module A\n  module B\n    class C\n    end\n  end\nend\n",
    );
    let scopes = lexical_scopes(&ast);
    let quals: Vec<Vec<String>> = scopes.iter().map(|(_, q)| q.clone()).collect();
    assert!(quals.contains(&seg(&["A"])));
    assert!(quals.contains(&seg(&["A", "B"])));
    assert!(quals.contains(&seg(&["A", "B", "C"])));
    // Innermost scope has the narrowest span (nested last).
    assert_eq!(scopes.len(), 3);
}

#[test]
fn method_body_spans_are_method_defs_only() {
    // Every `def` body (instance and singleton) contributes a span; a
    // `class << self` body does not (it is a CLASS scope, not a method one).
    let ast = lower_src(
        b"class K\n  def a; 1; end\n  def self.b; 2; end\n  class << self\n    def c; 3; end\n  end\nend\n",
    );
    // `a`, `self.b`, and the `c` inside `class << self` = 3 method defs; the
    // singleton-class body itself is excluded.
    assert_eq!(method_body_spans(&ast).len(), 3);
    assert!(method_body_spans(&lower_src(b"x = 1\n")).is_empty());
}

#[test]
fn project_declares_method_sees_nested_defs() {
    // A reopened CORE class contributes methods RBS cannot know about, and a
    // def nested in a block or a conditional counts — the reference's
    // `Scope#discovered_method?` is keyed by qualified class over every def
    // in the body, not just its direct children.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"class String\n  def direct; 1; end\n  [1].each do\n    def in_block; 2; end\n  end\n  if true\n    def in_if; 3; end\n  end\nend\n",
        &core,
    );
    assert!(idx.project_declares_method(None, "String", "direct"));
    assert!(idx.project_declares_method(None, "String", "in_block"));
    assert!(idx.project_declares_method(None, "String", "in_if"));
    assert!(!idx.project_declares_method(None, "String", "never_defined"));
    // Keyed by the QUALIFIED name — a nested class does not leak outward.
    let (_b, idx2) = build_one(
        b"module Outer\n  class Inner\n    def only_here; 1; end\n  end\nend\n",
        &core,
    );
    assert!(idx2.project_declares_method(None, "Outer::Inner", "only_here"));
    assert!(!idx2.project_declares_method(None, "Outer", "only_here"));
    assert!(!idx2.project_declares_method(None, "Inner", "only_here"));
}

#[test]
fn mutated_params_are_position_aware() {
    // `def m(x, a); a << 1; end` records index 1 ONLY: passing a local at
    // position 0 must not widen it (probed against the oracle).
    let core = CoreIndex::new();
    let (_a, idx) = build_one(b"def m(x, a)\n  a << 1\nend\n", &core);
    assert!(idx.method_mutates_param("m", 1));
    assert!(!idx.method_mutates_param("m", 0));
    // A mutator on a DIFFERENT local records nothing.
    let (_b, idx2) = build_one(b"def n(a)\n  b = []\n  b << 1\n  a.size\nend\n", &core);
    assert!(!idx2.method_mutates_param("n", 0));
    // A non-mutating call on the param records nothing.
    let (_c, idx3) = build_one(b"def p(a)\n  a.size\nend\n", &core);
    assert!(!idx3.method_mutates_param("p", 0));
    // An unknown method name is never mutating.
    assert!(!idx.method_mutates_param("totally_unknown", 0));
}

#[test]
fn toplevel_defs_include_receiver_bearing_defs() {
    // The reference keys a def with an EMPTY lexical prefix under its
    // `<toplevel>` table unless the receiver is `self`, so `def IO.foo`
    // resolves a later bare `foo` and `def self.bar` does not.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"def IO.recv_def; 1; end\ndef self.self_def; 2; end\ndef plain_def; 3; end\n",
        &core,
    );
    assert!(idx.is_toplevel_def(None, "recv_def"));
    assert!(idx.is_toplevel_def(None, "plain_def"));
    assert!(!idx.is_toplevel_def(None, "self_def"));
    // Inside a class body the lexical prefix is non-empty ⇒ not toplevel.
    let (_b, idx2) = build_one(b"class K\n  def self.klass_singleton; 1; end\nend\n", &core);
    assert!(!idx2.is_toplevel_def(None, "klass_singleton"));
}

// --- issue #141: receiver-eval def attribution (upstream `fb781023`) ----

#[test]
fn class_eval_def_belongs_to_receiver_not_toplevel() {
    // The motivating bug: `Minitest::Test.class_eval { def expect }` put
    // `expect` in the project-wide toplevel table, silently resolving
    // every unrelated bare `expect(...)`. The def belongs to the
    // receiver.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"class Minitest::Test\nend\nMinitest::Test.class_eval do\n  def expect; 1; end\nend\n",
        &core,
    );
    assert!(!idx.is_toplevel_def(None, "expect"));
    assert!(idx.project_declares_method(None, "Minitest::Test", "expect"));
}

#[test]
fn eval_receiver_resolution_variants() {
    let core = CoreIndex::new();
    // An undeclared receiver still names itself: `Missing.class_eval`
    // files under `Missing` (the as-written fallback), never toplevel.
    let (_a, idx) = build_one(b"Missing.class_eval do\n  def m; 1; end\nend\n", &core);
    assert!(!idx.is_toplevel_def(None, "m"));
    assert!(idx.project_declares_method(None, "Missing", "m"));
    // A DYNAMIC receiver (a local) names nothing: the block's def files
    // nowhere — not toplevel, not the enclosing lexical class.
    let (_b, idx2) = build_one(
        b"class Outer\n  def setup(x)\n    x.class_eval do\n      def leaked; 1; end\n    end\n  end\nend\n",
        &core,
    );
    assert!(!idx2.is_toplevel_def(None, "leaked"));
    assert!(!idx2.project_declares_method(None, "Outer", "leaked"));
    // Receiver resolution is through the LEXICAL nesting only —
    // `Module.nesting` does not change under `class_eval`, so a `B`
    // inside `A.class_eval` names `B`, not `A::B` (the as-written
    // fallback when no rung declares it).
    let (_c, idx3) = build_one(
        b"class A\n  class B\n  end\nend\nA.class_eval do\n  B.class_eval do\n    def deep; 1; end\n  end\nend\n",
        &core,
    );
    assert!(!idx3.is_toplevel_def(None, "deep"));
    assert!(idx3.project_declares_method(None, "B", "deep"));
    // When the lexical nesting does declare the rung it wins: `B` under
    // `class A` resolves to `A::B`.
    let (_d, idx4) = build_one(
        b"class A\n  class B\n  end\n  B.class_eval do\n    def deep; 1; end\n  end\nend\n",
        &core,
    );
    assert!(!idx4.is_toplevel_def(None, "deep"));
    assert!(idx4.project_declares_method(None, "A::B", "deep"));
}

#[test]
fn instance_eval_def_is_singleton_side() {
    // `X.instance_eval { def m }` binds `X.m` — singleton side
    // (`kind = :singleton`). The port's existence table answers
    // `:instance` queries only, so the name files NOWHERE — an instance
    // call `x.sm` must still witness absent (Blocking-3 fix) — and it is
    // never toplevel even for `X == Object`.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"class X; end\nX.instance_eval do\n  def sm; 1; end\nend\nObject.instance_eval do\n  def osm; 1; end\nend\n",
        &core,
    );
    assert!(!idx.is_toplevel_def(None, "sm"));
    assert!(!idx.project_declares_method(None, "X", "sm"));
    assert!(!idx.is_toplevel_def(None, "osm"));
    assert!(!idx.project_declares_method(None, "Object", "osm"));
}

#[test]
fn object_class_eval_def_stays_toplevel() {
    // The Object collapse: instance-side defs owned by `Object` are the
    // toplevel methods. `Object.class_eval { def m }` keeps `m`
    // bare-callable; `Object.instance_eval`, `class << self` inside one,
    // and a self-targeting `def Object.m` are singleton-side and do not.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"Object.class_eval do\n  def via_eval; 1; end\nend\nObject.class_exec do\n  def via_exec; 1; end\nend\n",
        &core,
    );
    assert!(idx.is_toplevel_def(None, "via_eval"));
    assert!(idx.is_toplevel_def(None, "via_exec"));
    let (_b, idx2) = build_one(
        b"Object.class_eval do\n  class << self\n    def oss; 1; end\n  end\n  def Object.orecv; 1; end\nend\n",
        &core,
    );
    assert!(!idx2.is_toplevel_def(None, "oss"));
    assert!(!idx2.is_toplevel_def(None, "orecv"));
    // The self-targeting receiver def is `:singleton` kind — filed
    // nowhere, so an `Object.new.orecv` call witnesses absent.
    assert!(!idx2.project_declares_method(None, "Object", "orecv"));
    // Kernel / BasicObject get NO collapse — instance methods under them
    // are not bare-callable (oracle: `helper_kern` / `helper_bo` fire).
    let (_c, idx3) = build_one(
        b"Kernel.class_eval do\n  def helper_kern; 1; end\nend\nBasicObject.class_eval do\n  def helper_bo; 1; end\nend\n",
        &core,
    );
    assert!(!idx3.is_toplevel_def(None, "helper_kern"));
    assert!(!idx3.is_toplevel_def(None, "helper_bo"));
}

#[test]
fn singleton_class_body_defs_are_singleton_side() {
    // `class << C` binds defs on `C`'s singleton — the names file
    // NOWHERE in the instance-side tables (`kind = :singleton`), never
    // toplevel; the same inside `Object.class_eval` does not collapse.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"class C; end\nclass << C\n  def csing; 1; end\nend\n",
        &core,
    );
    assert!(!idx.is_toplevel_def(None, "csing"));
    assert!(!idx.project_declares_method(None, "C", "csing"));
    // `class << self` inside a class body: the self prefix is `D`.
    let (_b, idx2) = build_one(
        b"class D\n  class << self\n    def dsing; 1; end\n  end\nend\n",
        &core,
    );
    assert!(!idx2.is_toplevel_def(None, "dsing"));
    assert!(!idx2.project_declares_method(None, "D", "dsing"));
}

#[test]
fn rooted_eval_receiver_ignores_lexical_nesting() {
    // Blocking-2 — `Source::ConstantPath.rooted?`: a `::`-spelled receiver
    // re-anchors at the top level BEFORE the lexical walk, so `::String`
    // inside `module A` names `String`, while a bare `String` rung still
    // resolves through the file's own lexical declarations.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"module A
  ::String.class_eval do
    def rm; 1; end
  end
end
",
        &core,
    );
    assert!(idx.project_declares_method(None, "String", "rm"));
    assert!(!idx.project_declares_method(None, "A::String", "rm"));
    // Lexical control: a bare `String` under `module A` where the file
    // declares `A::String` names `A::String`, not the core class.
    let (_b, idx2) = build_one(
        b"class A::String
end
module A
  String.class_eval do
    def lm; 1; end
  end
end
",
        &core,
    );
    assert!(idx2.project_declares_method(None, "A::String", "lm"));
    assert!(!idx2.project_declares_method(None, "String", "lm"));
    // `class << ::C` resolves rooted too.
    let (_c, idx3) = build_one(
        b"module A
  class << ::String
    def rs; 1; end
  end
end
",
        &core,
    );
    // Singleton side — files nowhere in the instance table.
    assert!(!idx3.project_declares_method(None, "String", "rs"));
    assert!(!idx3.project_declares_method(None, "A::String", "rs"));
}

#[test]
fn singleton_defs_do_not_pollute_instance_table() {
    // Blocking-3 — `kind = :singleton` names (`def self.x`, `class <<`,
    // `instance_eval` defs, a self-targeting receiver def) file NOWHERE
    // in the instance-kind existence tables, so `obj.m` still witnesses
    // absent (the reference's `discovered_method?(:instance)` never sees
    // them either).
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"class String
  def self.sing1; 1; end
end
Float.instance_eval do
  def sing2; 1; end
end
class Array
  class << self
    def sing3; 1; end
  end
end
",
        &core,
    );
    assert!(!idx.project_declares_method(None, "String", "sing1"));
    assert!(!idx.project_declares_method(None, "Float", "sing2"));
    assert!(!idx.project_declares_method(None, "Array", "sing3"));
    // The instance-side control still files.
    let (_b, idx2) = build_one(
        b"class String
  def inst1; 1; end
end
",
        &core,
    );
    assert!(idx2.project_declares_method(None, "String", "inst1"));
}

#[test]
fn orphan_arena_defs_file_under_span_context() {
    // Blocking-1 — defs lowered into the arena without a
    // `def_walk_children` edge (range endpoints are lowered for
    // reachability then dropped) still file under the context their span
    // sits in; the reference's `compact_child_nodes` walk reaches every
    // Prism child.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"(def rlo; 0; end)..(def rhi; 9; end)
",
        &core,
    );
    assert!(idx.is_toplevel_def(None, "rlo"));
    assert!(idx.is_toplevel_def(None, "rhi"));
    // Under an eval block the same orphan lands on the receiver.
    let (_b, idx2) = build_one(
        b"String.class_eval do
  x = (def ev_orph; 1; end)..2
end
",
        &core,
    );
    assert!(idx2.project_declares_method(None, "String", "ev_orph"));
    // Inside a class body the orphan is an instance method.
    let (_c, idx3) = build_one(
        b"class K
  y = (def km_orph; 1; end)..2
end
",
        &core,
    );
    assert!(idx3.project_declares_method(None, "K", "km_orph"));
    // A def inside another def's span files nowhere — the reference's
    // DefNode arm never descends.
    let (_d, idx4) = build_one(
        b"def outer_orph
  (def in_range_orph; 1; end)..2
end
",
        &core,
    );
    assert!(idx4.is_toplevel_def(None, "outer_orph"));
    assert!(!idx4.is_toplevel_def(None, "in_range_orph"));
    // Under an orphan singleton context the def files nowhere (singleton
    // side), not the enclosing class.
    let (_e, idx5) = build_one(
        b"class << Object
  (def sing_orph; 1; end)..2
end
",
        &core,
    );
    assert!(!idx5.project_declares_method(None, "Object", "sing_orph"));
    assert!(!idx5.is_toplevel_def(None, "sing_orph"));
}

#[test]
fn rooted_class_header_resets_lexical_prefix() {
    // `Source::ConstantPath.declaration_prefix` — a `::`-rooted header
    // RESETS the body's lexical prefix to the header's own name:
    // `module M; class ::Object` opens `Object` (its defs collapse to
    // bare-callable), `class ::String` reopens `String` — never
    // `M::Object` / `M::String` (review round 2, blocking 1).
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"module M
  class ::Object
    def rooted_obj = 1
  end
end
",
        &core,
    );
    assert!(idx.is_toplevel_def(None, "rooted_obj"));
    // `::Kernel` / `::BasicObject` reset too but get NO Object collapse —
    // the reference fires on the bare call (must-still-fire control).
    let (_b, idx2) = build_one(
        b"module M
  module ::Kernel
    def rooted_kern = 1
  end
end
",
        &core,
    );
    assert!(!idx2.is_toplevel_def(None, "rooted_kern"));
    assert!(idx2.project_declares_method(None, "Kernel", "rooted_kern"));
    assert!(!idx2.project_declares_method(None, "M::Kernel", "rooted_kern"));
    // `class ::String` files under `String` — the lexical-`M::String`
    // mis-key was a pre-existing FP this also closes.
    let (_c, idx3) = build_one(
        b"module M
  class ::String
    def rooted_str = 1
  end
end
",
        &core,
    );
    assert!(idx3.project_declares_method(None, "String", "rooted_str"));
    assert!(!idx3.project_declares_method(None, "M::String", "rooted_str"));
    // Lexical control: a NON-rooted `class Object` inside `module M`
    // still names `M::Object` — the bare call keeps firing.
    let (_d, idx4) = build_one(
        b"module M
  class Object
    def lexobj = 1
  end
end
",
        &core,
    );
    assert!(!idx4.is_toplevel_def(None, "lexobj"));
    assert!(idx4.project_declares_method(None, "M::Object", "lexobj"));
    // `private` does not change the filing (the reference records the
    // running default without removing the name).
    let (_e, idx5) = build_one(
        b"module M
  class ::Object
    private
    def ro_priv = 1
  end
end
",
        &core,
    );
    assert!(idx5.is_toplevel_def(None, "ro_priv"));
}

#[test]
fn self_anchored_header_rides_rebound_self() {
    // `self_anchored_decl_prefix` — a `class self::X` header under a
    // REBOUND self names `owner::X`, not the lexical `X`: inside
    // `Object.class_eval` it records `Object::String` (so `"s".m` still
    // fires), inside `class <<` it is unnameable, and inside a plain
    // `module M` it resolves lexically (`M::String`).
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"Object.class_eval do
  class self::String
    def self_hdr = 1
  end
end
",
        &core,
    );
    assert!(idx.project_declares_method(None, "Object::String", "self_hdr"));
    assert!(!idx.project_declares_method(None, "String", "self_hdr"));
    let (_b, idx2) = build_one(
        b"class << Object
  class self::String
    def u_self = 1
  end
end
",
        &core,
    );
    // Unnameable under `class <<` — files nowhere.
    assert!(!idx2.project_declares_method(None, "Object::String", "u_self"));
    assert!(!idx2.project_declares_method(None, "String", "u_self"));
    // Lexical fallback with no rebound self: `module M` makes self `M`.
    let (_c, idx3) = build_one(
        b"module M
  class self::String
    def lex_self = 1
  end
end
",
        &core,
    );
    assert!(idx3.project_declares_method(None, "M::String", "lex_self"));
    assert!(!idx3.project_declares_method(None, "String", "lex_self"));
}

#[test]
fn rooted_header_under_singleton_cref_reanchors() {
    // `decl_nameable_under_cref?` — a `::`-rooted header escapes an
    // unnameable `class <<` cref (its name is still reachable), while a
    // bare header stays ownerless.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"class << Object
  class ::String
    def rr_s = 1
  end
end
",
        &core,
    );
    assert!(idx.project_declares_method(None, "String", "rr_s"));
    let (_b, idx2) = build_one(
        b"class << Object
  class Bare
    def rr_b = 1
  end
end
",
        &core,
    );
    // A bare header below `class <<` names `#<singleton>::Bare` — nothing.
    assert!(!idx2.project_declares_method(None, "Bare", "rr_b"));
    assert!(!idx2.project_declares_method(None, "Object::Bare", "rr_b"));
}

#[test]
fn multi_segment_header_contributes_one_rung() {
    // `Source::ConstantPath.declaration_prefix` pushes the rendered name
    // as ONE prefix element, so `lexical_nesting_for_prefix` yields a
    // rung only at DECLARATION boundaries — `class ::M::N` inside
    // `module Outer` nests `M::N` alone (no bare `M` rung), and
    // `class A::B` inside `module M` nests `M::A::B`, `M` (never a
    // partial-segment `M::A` rung). Round-3: the split made `String`
    // resolve to `M::String`, a port-only `call.undefined-method`.
    let core = CoreIndex::new();
    // Rooted multi-segment header: the `M` rung must NOT exist.
    let (_a, idx) = build_one(
        b"module M
  class String
  end
end
module Outer
  class ::M::N
                  String.class_eval do
      def injected = 1
    end
  end
end
",
        &core,
    );
    assert!(idx.project_declares_method(None, "String", "injected"));
    assert!(!idx.project_declares_method(None, "M::String", "injected"));
    // Same for a `class <<` operand — resolved against the same rungs.
    let (_b, idx2) = build_one(
        b"module M
  class String
  end
end
module Outer
  class ::M::N
                  class << String
      def s_inj = 1
    end
  end
end
",
        &core,
    );
    // Singleton-side defs file nowhere on the instance side regardless —
    // assert only that the M::String instance table saw nothing.
    assert!(!idx2.project_declares_method(None, "M::String", "s_inj"));
    // Unrooted multi-segment: the enclosing `M` rung stays live (the
    // header pushes ONE `A::B` element) — `M::String` resolves — while
    // a PARTIAL segment `M::A` is never a rung: `M::A::String` declared
    // but unreachable from `class A::B`'s body.
    let (_c, idx3) = build_one(
        b"module A3
end
module M
  class String
  end
  class A3::String
  end
end
              module M
  class A3::B
    String.class_eval do
      def mid = 1
    end
  end
              class A::B
    String.class_eval do
      def via_m = 1
    end
  end
end
",
        &core,
    );
    // `class A::B` inside `module M`: rungs M::A::B, M — `M::String` wins.
    assert!(idx3.project_declares_method(None, "M::String", "via_m"));
    assert!(!idx3.project_declares_method(None, "String", "via_m"));
    // `class A3::B`: rungs M::A3::B, M — `M::String` wins over both
    // `M::A3::String` (M::A3 is NOT a rung) and `String`.
    assert!(idx3.project_declares_method(None, "M::String", "mid"));
    assert!(!idx3.project_declares_method(None, "M::A3::String", "mid"));
}

#[test]
fn def_attribution_is_per_file_macros_are_cross_file() {
    // `finalize_def_index` + `seed_discovered_methods`: a plain `def`
    // suppresses only in the file declaring it (a cross-file `def` is the
    // ADR-17 monkey-patch case `undefined-method` surfaces), while a
    // call-introduced name (`attr_reader`) survives `subtract_def_methods`
    // and suppresses cross-file.
    let core = CoreIndex::new();
    let a = lower_src(b"class String
  def xs_def; 1; end
  attr_reader :xs_attr
end
");
    let b = lower_src(b"\"s\".xs_def\n\"s\".xs_attr\n");
    let idx = SourceIndex::build_project(&[&a, &b], &core);
    // The declaring file's overlay sees both.
    assert!(idx.project_declares_method(Some(a.file_key()), "String", "xs_def"));
    assert!(idx.project_declares_method(Some(a.file_key()), "String", "xs_attr"));
    // The sibling file sees the macro but NOT the plain def.
    assert!(!idx.project_declares_method(Some(b.file_key()), "String", "xs_def"));
    assert!(idx.project_declares_method(Some(b.file_key()), "String", "xs_attr"));
    // `None` keeps the union-over-all-files answer for legacy callers.
    assert!(idx.project_declares_method(None, "String", "xs_def"));
}

#[test]
fn object_eval_def_is_per_file_only() {
    // `Object.class_eval { def m }` resolves a bare `m` ONLY in the file
    // that declares it — `source_declared_method?` reads `Object` through
    // the per-file overlay — while a toplevel `def` is project-wide
    // (`top_level_def_for`) and an `Object` macro is cross-file
    // (`subtract_def_methods` keeps it).
    let core = CoreIndex::new();
    let a = lower_src(b"Object.class_eval do
  def obj_m; 1; end
end
def top_m; 1; end
");
    let b = lower_src(b"obj_m
top_m
");
    let idx = SourceIndex::build_project(&[&a, &b], &core);
    assert!(idx.is_toplevel_def(Some(a.file_key()), "obj_m"));
    assert!(!idx.is_toplevel_def(Some(b.file_key()), "obj_m"));
    // A real toplevel `def` resolves in BOTH files.
    assert!(idx.is_toplevel_def(Some(a.file_key()), "top_m"));
    assert!(idx.is_toplevel_def(Some(b.file_key()), "top_m"));
    // An `Object` macro survives the def subtraction → cross-file.
    let c = lower_src(b"Object.class_eval do
  attr_reader :obj_attr
end
");
    let d = lower_src(b"obj_attr
");
    let idx2 = SourceIndex::build_project(&[&c, &d], &core);
    assert!(idx2.is_toplevel_def(Some(c.file_key()), "obj_attr"));
    assert!(idx2.is_toplevel_def(Some(d.file_key()), "obj_attr"));
}

#[test]
fn factory_block_def_attribution() {
    let core = CoreIndex::new();
    // `K = Class.new { def m }` — the block's def belongs to the class
    // the WRITE names, not the enclosing lexical scope and not toplevel.
    let (_a, idx) = build_one(b"K = Class.new do\n  def km; 1; end\nend\n", &core);
    assert!(!idx.is_toplevel_def(None, "km"));
    assert!(idx.project_declares_method(None, "K", "km"));
    // An anonymous factory block keeps the reference's toplevel leniency
    // (upstream #319): `Module.new { def m }` still resolves a bare call.
    let (_b, idx2) = build_one(b"Module.new do\n  def anon_m; 1; end\nend\n", &core);
    assert!(idx2.is_toplevel_def(None, "anon_m"));
}

#[test]
fn eval_block_macros_file_under_receiver() {
    // `record_call_node_methods`: `attr_reader`, `define_method`,
    // `alias_method` and the module-attr macros introduce methods under
    // the eval block's receiver — `Object` collapses them to toplevel.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"class W; end\nW.class_eval do\n  attr_reader :wattr\n  define_method(:wdm) { 1 }\n  attr_accessor :wacc\n  alias_method :walias, :wattr\nend\n",
        &core,
    );
    for m in ["wattr", "wdm", "wacc", "wacc=", "walias"] {
        assert!(idx.project_declares_method(None, "W", m), "missing {m}");
        assert!(!idx.is_toplevel_def(None, m), "{m} leaked to toplevel");
    }
    let (_b, idx2) = build_one(
        b"Object.class_eval do\n  attr_reader :oattr\n  define_method(:odm) { 1 }\nend\n",
        &core,
    );
    assert!(idx2.is_toplevel_def(None, "oattr"));
    assert!(idx2.is_toplevel_def(None, "odm"));
    // A toplevel `attr_reader` records nothing (the reference declines an
    // empty prefix) — must-still-fire control.
    let (_c, idx3) = build_one(b"attr_reader :tl_attr\n", &core);
    assert!(!idx3.is_toplevel_def(None, "tl_attr"));
}

#[test]
fn class_eval_bare_and_self_receivers() {
    // A bare `class_eval` / `self.class_eval` keeps the enclosing self:
    // inside `Object.class_eval` the nested bare eval still owns Object;
    // at file toplevel the self is unnameable — defs file nowhere.
    let core = CoreIndex::new();
    let (_a, idx) = build_one(
        b"Object.class_eval do\n  class_eval do\n    def nested_bare; 1; end\n  end\n  self.class_eval do\n    def nested_self; 1; end\n  end\nend\n",
        &core,
    );
    assert!(idx.is_toplevel_def(None, "nested_bare"));
    assert!(idx.is_toplevel_def(None, "nested_self"));
    let (_b, idx2) = build_one(
        b"class_eval do\n  def tl_eval_def; 1; end\nend\nself.class_eval do\n  def tl_self_eval_def; 1; end\nend\n",
        &core,
    );
    assert!(!idx2.is_toplevel_def(None, "tl_eval_def"));
    assert!(!idx2.is_toplevel_def(None, "tl_self_eval_def"));
}
