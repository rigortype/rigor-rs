use super::*;
use rigor_index::CoreIndex;
use rigor_parse::{lower, parse, LoweredAst};
use rigor_types::{Interner, Scalar, Type};

fn run(src: &[u8]) -> Vec<Diagnostic> {
    let ast = lower(&parse(src));
    let mut interner = Interner::new();
    let index = CoreIndex::new();
    analyze(&ast, &mut interner, &index)
}

#[test]
fn flags_typo_method_on_string_literal() {
    let src = b"s = \"Hello\"\ns.lenght\n";
    let diags = run(src);
    assert_eq!(diags.len(), 1);
    let d = &diags[0];
    assert_eq!(d.rule_id, CALL_UNDEFINED_METHOD);
    assert_eq!(d.message, "undefined method `lenght' for \"Hello\"");
    // Severity must be Error for undefined-method.
    assert_eq!(d.severity, Severity::Error);
    assert_eq!(d.source_family, "builtin");
    // receiver_type matches the reference's rendering: the literal value
    // `"Hello"` (with surrounding double quotes), not the bare class name.
    assert_eq!(d.receiver_type.as_deref(), Some("\"Hello\""));
    assert_eq!(d.method_name.as_deref(), Some("lenght"));
    // The span must cover exactly `lenght`.
    assert_eq!(&src[d.start_offset..d.end_offset], b"lenght");
}

/// Census mechanism 1: an `is_a?`-narrowed Dynamic local witnesses
/// `call.undefined-method` against the narrowed class, rendered as the
/// plain class name (`for Hash`) — and ONLY undefined-method (a narrowed
/// receiver never feeds wrong-arity/ATM in this slice).
#[test]
fn narrowed_local_witnesses_undefined_method() {
    let src =
        b"def f(value)\n  if value.is_a?(Hash)\n    value.frobnicate_zzz\n  end\nend\n";
    let diags = run(src);
    assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
    let d = &diags[0];
    assert_eq!(d.rule_id, CALL_UNDEFINED_METHOD);
    assert_eq!(d.message, "undefined method `frobnicate_zzz' for Hash");
    assert_eq!(d.receiver_type.as_deref(), Some("Hash"));
    assert_eq!(&src[d.start_offset..d.end_offset], b"frobnicate_zzz");
}

/// CARRIER FIDELITY (docs/notes/20260808-narrowing-carrier-fidelity-fp.md),
/// END TO END. `narrow_class_other` narrows a `Dynamic`/`Top` carrier only,
/// so "we narrow only Dynamic" is a subset rule exactly while `Dynamic`
/// means the same thing on both engines — and it does not. Every source
/// below is oracle-measured SILENT on the pinned reference (fresh cwd,
/// `--no-cache`) and emitted `undefined method 'frobnicate_zzz' for Hash`
/// on master: a live violation of the sound-subset contract (ADR-0002).
#[test]
fn coarse_carrier_narrowing_is_silent_end_to_end() {
    // fp1 — the note's archetype (gitlab-foss `spec_hash = spec || {}`),
    // in the bare-statement form and in the ivar-write form.
    assert!(run(b"def f(spec)\n  h = spec || {}\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n")
        .is_empty());
    assert!(run(
        b"def f(spec)\n  h = spec || {}\n  @spec = h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"
    )
    .is_empty());
    // fp1' — the `raise`-guard early-return form of the same carrier.
    assert!(run(
        b"def f(spec)\n  h = spec || {}\n  raise ArgumentError unless h.is_a?(Hash)\n\n  h.frobnicate_zzz\nend\n"
    )
    .is_empty());
    // fp2 — a project method whose return TAIL is a `Logical`.
    assert!(run(
        b"class C\n  def config\n    c = mk\n    raise ArgumentError unless c.is_a?(Hash)\n\n    @config = c.frobnicate_zzz\n  end\n\n  def mk\n    unknown_zzz || {}\n  end\nend\n"
    )
    .is_empty());
    // The rest of the audited carriers the reference types precisely.
    for src in [
        &b"def f(cond)\n  h = while cond\n    break({})\n  end\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        &b"def f(spec)\n  h = begin\n    spec\n  rescue StandardError\n    {}\n  end\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        &b"def f(cond, spec)\n  h = case cond\n  when 1 then spec\n  else {}\n  end\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        &b"def f(cond, spec)\n  h = cond ? spec : {}\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        &b"def f\n  h = (1..2)\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        &b"class C\n  def f\n    h = self\n    h.is_a?(Hash) ? h.frobnicate_zzz : h\n  end\nend\n"[..],
        &b"def f\n  h = proc { |x| x }\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        &b"def f\n  h = __method__\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        &b"def f(spec)\n  h = defined?(spec)\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
    ] {
        assert!(run(src).is_empty(), "expected silence for {:?}", String::from_utf8_lossy(src));
    }
}

/// The carrier-fidelity decline is NOT a blanket silencing: the ordinary
/// Dynamic-parameter narrowing — and every allow-listed carrier — still
/// witnesses. A decline that silenced everything would pass an FP gate too,
/// so these positive controls are what makes the gate meaningful.
#[test]
fn narrowable_carriers_still_witness_end_to_end() {
    for src in [
        // a method parameter, and a block parameter
        &b"def f(h)\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        &b"def f(o)\n  o.each { |h| h.is_a?(Hash) ? h.frobnicate_zzz : h }\nend\n"[..],
        // keyword / optional / rest / block parameters
        &b"def f(k: nil)\n  h = k\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        &b"def f(o = nil)\n  h = o\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        &b"def f(*a)\n  h = a\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        // `@ivar` / `$gvar` reads
        &b"def f\n  h = @x\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        &b"def f\n  h = $gx\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        // a call through a narrowable receiver (plain, chained, `[]`,
        // safe-nav, block-bearing, ivar receiver)
        &b"def f(spec)\n  h = spec.unknown_zzz\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        &b"def f(spec)\n  h = spec.foo_zzz.bar_zzz\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        &b"def f(spec)\n  h = spec[0]\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        &b"def f(spec)\n  h = spec&.dup\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        &b"def f(spec)\n  h = spec.map { |x| x }\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        &b"def f\n  h = @obj.foo_zzz\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        // destructuring — even off a `Logical` RHS (measured: both fire)
        &b"def f(spec)\n  a, h = spec\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        &b"def f(spec)\n  a, h = (spec || {})\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        // `case`/`when` and the `raise`-guard early-return form
        &b"def f(v)\n  case v\n  when Hash\n    v.frobnicate_zzz\n  end\nend\n"[..],
        &b"def f(spec)\n  raise ArgumentError unless spec.is_a?(Hash)\n\n  spec.frobnicate_zzz\nend\n"[..],
    ] {
        let diags = run(src);
        assert_eq!(
            diags.len(),
            1,
            "expected one undefined-method for {:?}, got {diags:?}",
            String::from_utf8_lossy(src)
        );
        assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
        assert_eq!(diags[0].message, "undefined method `frobnicate_zzz' for Hash");
    }
}

// --- S2: qualified-name class-narrowing witnessing -----------------------
// docs/notes/20260808-qualified-witnessing-mini-spec.md. Every row was
// measured against the pinned reference (fresh cwd, `--no-cache`, plugin
// path pinned); the probe id in each comment is the row in
// 20260808-qualified-witnessing-probes.md.

/// The witness now fires for a guard class that is NAMESPACED, or top-level
/// but outside the nine-name `CORE_CLASSES` array — the two independent
/// blockers S2 removed. The message renders the FULL resolved path.
#[test]
fn qualified_guard_class_witnesses_absence() {
    for (src, method, expect) in [
        // p1a/p1c/p1d/p1f — vendored core+stdlib namespaced classes
        (&b"def f(v)\n  return unless v.is_a?(File::Stat)\n  v.frobnicate_zzz\nend\n"[..], "frobnicate_zzz", "File::Stat"),
        (&b"def f(v)\n  return unless v.is_a?(URI::HTTP)\n  v.frobnicate_zzz\nend\n"[..], "frobnicate_zzz", "URI::HTTP"),
        (&b"def f(v)\n  return unless v.is_a?(Encoding::Converter)\n  v.frobnicate_zzz\nend\n"[..], "frobnicate_zzz", "Encoding::Converter"),
        (&b"def f(v)\n  return unless v.is_a?(Enumerator::Lazy)\n  v.frobnicate_zzz\nend\n"[..], "frobnicate_zzz", "Enumerator::Lazy"),
        // r8/v4 — a DEPTH-3 decl, the S0 fix's payoff
        (&b"def f(v)\n  return unless v.is_a?(Bundler::Source::Git)\n  v.frobnicate_zzz\nend\n"[..], "frobnicate_zzz", "Bundler::Source::Git"),
        (&b"def f(v)\n  return unless v.is_a?(Bundler::Source::Rubygems)\n  v.frobnicate_zzz\nend\n"[..], "frobnicate_zzz", "Bundler::Source::Rubygems"),
        // q4c — an ambiguous LEAF. (q5, the qualified MODULE twin, moved to
        // the declines below: upstream #739 retracted it in `v0.3.8`.)
        (&b"def f(v)\n  return unless v.is_a?(Random::Base)\n  v.frobnicate_zzz\nend\n"[..], "frobnicate_zzz", "Random::Base"),
        // q4 — NO leaf fallback: `superclass` is a `::Class` method, and
        // `Digest::Class` must not inherit it
        (&b"def f(v)\n  return unless v.is_a?(Digest::Class)\n  v.superclass\nend\n"[..], "superclass", "Digest::Class"),
        // u1/u2/u4/u5 — top-level, outside CORE_CLASSES
        (&b"def f(v)\n  return unless v.is_a?(Time)\n  v.frobnicate_zzz\nend\n"[..], "frobnicate_zzz", "Time"),
        (&b"def f(v)\n  return unless v.is_a?(Range)\n  v.frobnicate_zzz\nend\n"[..], "frobnicate_zzz", "Range"),
        (&b"def f(v)\n  return unless v.is_a?(Struct)\n  v.frobnicate_zzz\nend\n"[..], "frobnicate_zzz", "Struct"),
        (&b"def f(v)\n  return unless v.is_a?(Pathname)\n  v.frobnicate_zzz\nend\n"[..], "frobnicate_zzz", "Pathname"),
        // p3c — a leading `::` is stripped, rendering the bare path
        (&b"def f(v)\n  return unless v.is_a?(::File::Stat)\n  v.frobnicate_zzz\nend\n"[..], "frobnicate_zzz", "File::Stat"),
        // p1g/p9a/r4/r5/r3 — every composition the top-level narrowing
        // already supported behaves identically with a qualified class
        (&b"def f(v)\n  if v.is_a?(File::Stat)\n    v.frobnicate_zzz\n  end\nend\n"[..], "frobnicate_zzz", "File::Stat"),
        (&b"def f(v)\n  return unless v.instance_of?(File::Stat)\n  v.frobnicate_zzz\nend\n"[..], "frobnicate_zzz", "File::Stat"),
        (&b"def f(v, c)\n  return unless c && v.is_a?(File::Stat)\n  v.frobnicate_zzz\nend\n"[..], "frobnicate_zzz", "File::Stat"),
        (&b"def f(v)\n  case v\n  when File::Stat then v.frobnicate_zzz\n  end\nend\n"[..], "frobnicate_zzz", "File::Stat"),
        // p1e/p1f — a CHAIN address (stage 3a-3) takes the same routing
        (&b"def f(h)\n  return unless h.last.is_a?(File::Stat)\n  h.last.frobnicate_zzz\nend\n"[..], "frobnicate_zzz", "File::Stat"),
    ] {
        let diags = run(src);
        assert_eq!(
            diags.len(),
            1,
            "expected one undefined-method for {:?}, got {diags:?}",
            String::from_utf8_lossy(src)
        );
        assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
        assert_eq!(
            diags[0].message,
            format!("undefined method `{method}' for {expect}")
        );
        assert_eq!(diags[0].receiver_type.as_deref(), Some(expect));
    }
}

/// The must-STAY-SILENT half. Every row is measured silent on BOTH engines;
/// each would be a false positive if the matching gate were dropped.
#[test]
fn qualified_guard_witness_declines() {
    for src in [
        // p7a — the class's OWN method
        &b"def f(v)\n  return unless v.is_a?(File::Stat)\n  v.directory?\nend\n"[..],
        // p7c — an Object method
        &b"def f(v)\n  return unless v.is_a?(File::Stat)\n  v.frozen?\nend\n"[..],
        // q5b — an Object method on a MODULE target (RBS's implicit
        // `::Object` self-type; S1)
        &b"def f(v)\n  return unless v.is_a?(Digest::Instance)\n  v.frozen?\nend\n"[..],
        // q5 — upstream #739 / PR #741 (`3636649f`, `v0.3.8`): an
        // instance-side MODULE receiver declines outright now. The value is
        // an instance of whatever class includes the module, and that class
        // contributes an arbitrary surface, so nothing can prove a method
        // absent. Qualified and top-level spellings, and the `Class`/`Module`
        // metaclass twins from #742 / PR #743 (`23341a87`). Fixture 101.
        &b"def f(v)\n  return unless v.is_a?(Digest::Instance)\n  v.frobnicate_zzz\nend\n"[..],
        &b"def f(v)\n  return unless v.is_a?(Enumerable)\n  v.frobnicate_zzz\nend\n"[..],
        &b"def f(v)\n  return unless v.is_a?(Comparable)\n  v.frobnicate_zzz\nend\n"[..],
        &b"def f(v)\n  return unless v.is_a?(Kernel)\n  v.frobnicate_zzz\nend\n"[..],
        &b"def f(v)\n  case v\n  when Comparable then v.frobnicate_zzz\n  end\nend\n"[..],
        &b"def f(v)\n  return unless v.is_a?(Class)\n  v.frobnicate_zzz\nend\n"[..],
        &b"def f(v)\n  return unless v.is_a?(Module)\n  v.frobnicate_zzz\nend\n"[..],
        // …and the SINGLETON reads of the two metaclass constants, which the
        // reference's `unenumerable_receiver?` declines above the
        // instance/singleton split.
        &b"Class.frobnicate_zzz\n"[..],
        &b"Module.frobnicate_zzz\n"[..],
        // p7b/v2 — inherited over the AS-WRITTEN chain, through two
        // ambiguous leaves (S1)
        &b"def f(v)\n  return unless v.is_a?(Digest::SHA256)\n  v.hexdigest\nend\n"[..],
        &b"def f(v)\n  return unless v.is_a?(Digest::SHA256)\n  v.digest\nend\n"[..],
        // v1/v3 — an `attr_reader` on `URI::Generic`, own and inherited (S1)
        &b"def f(v)\n  return unless v.is_a?(URI::Generic)\n  v.host\nend\n"[..],
        &b"def f(v)\n  return unless v.is_a?(URI::HTTP)\n  v.host\nend\n"[..],
        // q4b/v5 — own methods on a declaring class and on a gem class
        &b"def f(v)\n  return unless v.is_a?(Digest::Class)\n  v.digest\nend\n"[..],
        &b"def f(v)\n  return unless v.is_a?(Gem::Version)\n  v.segments\nend\n"[..],
        // p2/p2b — a class that exists nowhere, qualified and bare
        &b"def f(v)\n  return unless v.is_a?(Foo::Bar::Baz)\n  v.frobnicate_zzz\nend\n"[..],
        &b"def f(v)\n  return unless v.is_a?(Zorkmid)\n  v.frobnicate_zzz\nend\n"[..],
        // p5/q1 — an IN-SOURCE-only project class (ADR-0033 provenance),
        // namespaced and top-level
        &b"module Proj\n  class Thing\n  end\nend\ndef f(v)\n  return unless v.is_a?(Proj::Thing)\n  v.frobnicate_zzz\nend\n"[..],
        &b"class Thing\nend\ndef f(v)\n  return unless v.is_a?(Thing)\n  v.frobnicate_zzz\nend\n"[..],
        // q6 — a project REOPEN merges: its own method silences
        &b"module URI\n  class HTTP\n    def mine_zzz\n      1\n    end\n  end\nend\ndef f(v)\n  return unless v.is_a?(URI::HTTP)\n  v.mine_zzz\nend\n"[..],
        // r2 — the ELSE edge of a positive guard
        &b"def f(v)\n  if v.is_a?(File::Stat)\n    0\n  else\n    v.frobnicate_zzz\n  end\nend\n"[..],
        // r7 — a sequential DISJOINT re-guard. The reference carries the
        // first guard's `Nominal[File::Stat]` into the second, which
        // collapses it to `Bot`. rigor-rs reproduces the silence through
        // R3: the early-return propagation now re-seeds the PRE-JOIN local
        // fact, so the conflicting re-guard drops it instead of minting
        // against a wiped env.
        &b"def f(v)\n  return unless v.is_a?(File::Stat)\n  return unless v.is_a?(URI::HTTP)\n  v.frobnicate_zzz\nend\n"[..],
        // The same shape on two CORE names — a PRE-EXISTING false positive
        // (`s1_two_returns_sequential` in the next/break build note) that
        // the same re-seed closes.
        &b"def f(v)\n  return unless v.is_a?(Hash)\n  return unless v.is_a?(String)\n  v.frobnicate_zzz\nend\n"[..],
        // r3 — a SUBCLASS re-guard. The reference narrows DOWN
        // (`narrow_nominal_to_class`'s `:superclass` arm) and FIRES
        // ``… for Digest::SHA256``; rigor-rs's R3 is coarser (any
        // class change drops the fact), so this is a DECLINE — a coverage
        // gap, not a divergence in the FP direction. Recovering it needs a
        // qualified-aware `class_ordering`, deliberately out of scope (see
        // the S3 section of the mini-spec note).
        &b"def f(v)\n  return unless v.is_a?(Digest::Base)\n  return unless v.is_a?(Digest::SHA256)\n  v.frobnicate_zzz\nend\n"[..],
        // safe-nav is outside the envelope
        &b"def f(v)\n  return unless v.is_a?(File::Stat)\n  v&.frobnicate_zzz\nend\n"[..],
    ] {
        let diags = run(src);
        assert!(
            diags.is_empty(),
            "expected silence for {:?}, got {diags:?}",
            String::from_utf8_lossy(src)
        );
    }
}

/// The anti-over-suppression half of upstream #739/#742. Declining every
/// `Singleton` receiver, or every receiver whose guard names a module, would
/// pass the declines above and silence all four of these — which is why the
/// port keys the module half on the INSTANCE side and reads the guard's
/// RESOLVED carrier, not the guard's written name. Every row measured firing
/// on the oracle at pin `ffb456b0` (rows c4/c5/c11/x11; fixture 101).
#[test]
fn module_receiver_decline_leaves_the_enumerable_surfaces_alone() {
    for (src, expect) in [
        // c4/c5 — a namespace module's OWN singleton surface is real.
        (&b"require \"digest\"\nDigest::Instance.frobnicate_zzz\n"[..], "singleton(Digest::Instance)"),
        (&b"Comparable.frobnicate_zzz\n"[..], "singleton(Comparable)"),
        // c11 — an ordinary class singleton, the row a `Singleton`-wide
        // decline would take with it.
        (&b"String.frobnicate_zzz\n"[..], "singleton(String)"),
        // x11 — a module guard the environment can ORDER against the carrier
        // keeps the carrier's bound (`Array < Enumerable`), so the receiver
        // is `Array` and the witness stands.
        (&b"def f\n  h = Array.new\n  h.frobnicate_zzz if h.is_a?(Enumerable)\nend\n"[..], "Array"),
    ] {
        let diags = run(src);
        assert_eq!(
            diags.len(),
            1,
            "expected one undefined-method for {:?}, got {diags:?}",
            String::from_utf8_lossy(src)
        );
        assert_eq!(diags[0].receiver_type.as_deref(), Some(expect));
    }
}

/// p6 — the other half of q6: a project reopen ADDS to the RBS surface
/// instead of replacing it, so a still-absent method still fires and
/// `constant_shadowed` must NOT be widened to reopens.
#[test]
fn project_reopen_of_a_gem_namespace_still_witnesses() {
    let src = &b"module URI\n  class HTTP\n    def mine_zzz\n      1\n    end\n  end\nend\ndef f(v)\n  return unless v.is_a?(URI::HTTP)\n  v.frobnicate_zzz\nend\n"[..];
    let diags = run(src);
    assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
    assert_eq!(
        diags[0].message,
        "undefined method `frobnicate_zzz' for URI::HTTP"
    );
}

// --- disjoint-guard suppression -----------------------------------------
// docs/notes/20260808-disjoint-guard-suppression.md. Every row below was
// measured against the pinned reference (`--no-cache`, fresh cwd); the
// probe name in each comment is the row in that note's tables.

/// SILENCED: a call whose bare-local receiver a disjoint guard collapsed to
/// `Bot` in the reference. Guard predicates, statement forms, carriers and
/// the fact's lifetime — the whole measured family.
#[test]
fn disjoint_guard_suppresses_the_guarded_local() {
    for src in [
        // the reported archetype, in each statement form (probes
        // base_disj_undefmethod, s_if, s_if_modifier, toplevel, ternary_rhs,
        // ternary_arg, s_early_return, elsif_disj)
        &b"def f\n  h = [1, 2]\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"[..],
        &b"def f\n  h = [1, 2]\n  if h.is_a?(Hash)\n    h.frobnicate_zzz\n  end\nend\n"[..],
        &b"def f\n  h = [1, 2]\n  h.frobnicate_zzz if h.is_a?(Hash)\nend\n"[..],
        &b"h = [1, 2]\nh.frobnicate_zzz if h.is_a?(Hash)\n"[..],
        &b"def f\n  h = [1, 2]\n  y = h.is_a?(Hash) ? h.frobnicate_zzz : 0\n  y\nend\n"[..],
        &b"def f\n  h = [1, 2]\n  puts(h.is_a?(Hash) ? h.frobnicate_zzz : 0)\nend\n"[..],
        &b"def f\n  h = [1, 2]\n  return 0 unless h.is_a?(Hash)\n  h.frobnicate_zzz\nend\n"[..],
        &b"def f\n  h = [1, 2]\n  raise \"x\" unless h.is_a?(Hash)\n  h.frobnicate_zzz\nend\n"[..],
        &b"def f(c)\n  h = [1, 2]\n  if c\n    0\n  elsif h.is_a?(Hash)\n    h.frobnicate_zzz\n  end\nend\n"[..],
        // guard predicates: kind_of?, instance_of? (any name mismatch,
        // including a SUPERCLASS — the reference's `exact:` path), `===`
        // (probes g_kind_of, g_instance_of_disjoint, g_instance_of_super,
        // iof_nominal_super, iof_nominal_unknown, g_triple_eq, tripleeq_if)
        &b"def f\n  h = [1, 2]\n  h.frobnicate_zzz if h.kind_of?(Hash)\nend\n"[..],
        &b"def f\n  h = [1, 2]\n  h.frobnicate_zzz if h.instance_of?(Hash)\nend\n"[..],
        &b"def f\n  h = [1, 2]\n  h.frobnicate_zzz if h.instance_of?(Enumerable)\nend\n"[..],
        &b"def f\n  h = Array.new\n  h.frobnicate_zzz if h.instance_of?(Enumerable)\nend\n"[..],
        &b"def f\n  h = Array.new\n  h.frobnicate_zzz if h.instance_of?(UnknownZzz)\nend\n"[..],
        &b"def f\n  h = [1, 2]\n  if Hash === h\n    h.frobnicate_zzz\n  end\nend\n"[..],
        // `case`/`when`, single and multi-condition (probes s_case_when,
        // case_multi_disj)
        &b"def f\n  h = [1, 2]\n  case h\n  when Hash then h.frobnicate_zzz\n  else 0\n  end\nend\n"[..],
        &b"def f\n  h = [1, 2]\n  case h\n  when Hash, Integer then h.frobnicate_zzz\n  else 0\n  end\nend\n"[..],
        // carriers: array/hash literal (empty and not), %w, splat, a
        // mutator-widened nominal, `Array.new`, a shape-preserving chain
        // (probes x_*_disj)
        &b"def f\n  h = []\n  h.frobnicate_zzz if h.is_a?(Hash)\nend\n"[..],
        &b"def f\n  h = { a: 1 }\n  h.frobnicate_zzz if h.is_a?(Array)\nend\n"[..],
        &b"def f\n  h = {}\n  h.frobnicate_zzz if h.is_a?(Array)\nend\n"[..],
        &b"def f\n  h = %w[a b]\n  h.frobnicate_zzz if h.is_a?(Hash)\nend\n"[..],
        &b"def f(spec)\n  h = *spec\n  h.frobnicate_zzz if h.is_a?(Hash)\nend\n"[..],
        &b"def f\n  h = []\n  h << 1\n  h.frobnicate_zzz if h.is_a?(Hash)\nend\n"[..],
        &b"def f\n  h = Array.new\n  h.frobnicate_zzz if h.is_a?(Hash)\nend\n"[..],
        &b"def f\n  h = Hash.new\n  h.frobnicate_zzz if h.is_a?(Array)\nend\n"[..],
        &b"def f\n  h = [1, 2].compact\n  h.frobnicate_zzz if h.is_a?(Hash)\nend\n"[..],
        &b"def f\n  h = [1, 2].freeze\n  h.frobnicate_zzz if h.is_a?(Hash)\nend\n"[..],
        // the fact's REACH inside the branch: a nested conditional, a nested
        // block body, a second guard on the same local, a chain hop, a
        // mutator call in between (probes nest_deep, nest_block_recv,
        // bot_into_block, bot_into_block_doend, double_guard, bot_then_match,
        // bot_after_inner, bot_after_block_call, bot_after_while,
        // bot_after_begin, bot_mutator_use)
        &b"def f\n  h = [1, 2]\n  if h.is_a?(Hash)\n    if true\n      h.frobnicate_zzz\n    end\n  end\nend\n"[..],
        &b"def f\n  h = [1, 2]\n  if h.is_a?(Hash)\n    [1].each { h.frobnicate_zzz }\n  end\nend\n"[..],
        &b"def f\n  h = [1, 2]\n  if h.is_a?(Hash)\n    [1].each do |x|\n      h.frobnicate_zzz\n    end\n  end\nend\n"[..],
        &b"def f\n  h = [1, 2]\n  if h.is_a?(Enumerable)\n    if h.is_a?(Hash)\n      h.frobnicate_zzz\n    end\n  end\nend\n"[..],
        &b"def f\n  h = [1, 2]\n  if h.is_a?(Hash)\n    if h.is_a?(Array)\n      h.frobnicate_zzz\n    end\n  end\nend\n"[..],
        &b"def f\n  h = [1, 2]\n  if h.is_a?(Hash)\n    if true\n      0\n    end\n    h.frobnicate_zzz\n  end\nend\n"[..],
        &b"def f\n  h = [1, 2]\n  if h.is_a?(Hash)\n    [1].each { |x| x }\n    h.frobnicate_zzz\n  end\nend\n"[..],
        &b"def f\n  h = [1, 2]\n  if h.is_a?(Hash)\n    while false\n      0\n    end\n    h.frobnicate_zzz\n  end\nend\n"[..],
        &b"def f\n  h = [1, 2]\n  if h.is_a?(Hash)\n    begin\n      0\n    rescue\n      0\n    end\n    h.frobnicate_zzz\n  end\nend\n"[..],
        &b"def f\n  h = [1, 2]\n  if h.is_a?(Hash)\n    h.push(3)\n    h.frobnicate_zzz\n  end\nend\n"[..],
        // S3 (2026-08-08): a SHAPED carrier collapses under any guard the
        // ordering does not make it a subclass of — `Unknown` included,
        // because `narrow_shape_to_class` asks `subclass_of?` rather than
        // `disjoint?`. These five were LIVE false positives before S3
        // (probes r1/r1b/r1f/r1d/r1e/r1g): a resolvable-disjoint qualified
        // guard, the Hash-shaped carrier, the `if` form, an unresolvable
        // top-level name, an unresolvable qualified name, and an in-source
        // project class (which the mint declines to NARROW to but must
        // still SEE — that is what `mintable: false` carries).
        &b"def f\n  v = [1, 2]\n  return unless v.is_a?(File::Stat)\n  v.frobnicate_zzz\nend\n"[..],
        &b"def f\n  v = { a: 1 }\n  return unless v.is_a?(URI::HTTP)\n  v.frobnicate_zzz\nend\n"[..],
        &b"def f\n  v = [1, 2]\n  if v.is_a?(File::Stat)\n    v.frobnicate_zzz\n  end\nend\n"[..],
        &b"def f\n  v = [1, 2]\n  return unless v.is_a?(Zorkmid)\n  v.frobnicate_zzz\nend\n"[..],
        &b"def f\n  v = [1, 2]\n  return unless v.is_a?(Foo::Bar::Baz)\n  v.frobnicate_zzz\nend\n"[..],
        &b"module Proj\n  class Thing\n  end\nend\ndef f\n  v = [1, 2]\n  return unless v.is_a?(Proj::Thing)\n  v.frobnicate_zzz\nend\n"[..],
        // A DECLINE S3 costs, measured and accepted: `h = []` then
        // `h << 1` under an unresolvable guard. The reference widens that
        // carrier to a NOMINAL `Array[Dynamic[top]]`, and since the
        // `v0.3.8` re-pin that nominal WIDENS under an unorderable guard
        // rather than staying conservative, so both engines are silent —
        // this row and the two NOMINAL rows below now agree for the same
        // reason, where before the re-pin they diverged.
        &b"def f\n  h = []\n  h << 1\n  h.frobnicate_zzz if h.is_a?(UnknownZzz)\nend\n"[..],
        // A NOMINAL carrier under a guard class the hierarchy cannot ORDER:
        // upstream #533 item 4 (`70ca7e74`) answers `untyped` there — "the
        // guard proved membership in a class the engine cannot name, which
        // destroys the old knowledge". Both rows asserted FIRING `for Array`
        // in the anti-over-suppression test until the `v0.3.4 → v0.3.8`
        // re-pin; re-measured at `ffb456b0` both are reference-SILENT, and
        // the first is fixture 86 row 134, one of the four re-pin false
        // positives. See `ClassFact::Widened`.
        &b"def f\n  h = Array.new\n  h.frobnicate_zzz if h.is_a?(UnknownZzz)\nend\n"[..],
        &b"def f(spec)\n  h = *spec\n  h.frobnicate_zzz if h.is_a?(UnknownZzz)\nend\n"[..],
        // `v0.3.9`'s `6cde8381` (#657 item 2) gave the same `declines_bot?`
        // to the SHAPED carriers: a `Tuple` projects through `Array` and a
        // `HashShape` through `Hash`, so an unorderable guard class leaves
        // the ordering `Unknown` there too. Both rows fired `for Array` /
        // `for Hash` on the call AFTER the `if` until the re-pin — `Bot` is
        // the join identity and `Widened` absorbs — and both are the fixture
        // 100 rows b15b/b30b that retracted.
        &b"def f\n  h = [1, 2]\n  h.frobnicate_yyy if h.is_a?(UnknownZzz)\n  h.frobnicate_zzz\nend\n"[..],
        &b"def f\n  h = { a: 1 }\n  h.frobnicate_yyy if h.is_a?(UnknownZzz)\n  h.frobnicate_zzz\nend\n"[..],
    ] {
        let diags = run(src);
        assert!(
            diags.is_empty(),
            "expected silence for {:?}, got {diags:?}",
            String::from_utf8_lossy(src)
        );
    }
}

/// ANTI-OVER-SUPPRESSION. Silence is the fix here, so a rule that silenced
/// too much would sail through the FP gate while destroying coverage. Every
/// row is measured FIRING on the reference and must keep firing here: a
/// NON-disjoint guard, the unguarded and pre-guard uses, the falsey edge, a
/// DIFFERENT local inside the branch, a call nested in the suppressed call's
/// arguments, a rebind, and a guard class our hierarchy cannot decide.
#[test]
fn disjoint_guard_suppression_does_not_over_reach() {
    for (src, expect) in [
        // NON-disjoint guards: a superclass/module the carrier includes, the
        // same class, Object (probes n_super_enumerable, n_same_class,
        // n_object, x_*_ok, tripleeq_nondisj, elsif_nondisj)
        (&b"def f\n  h = [1, 2]\n  h.frobnicate_zzz if h.is_a?(Enumerable)\nend\n"[..], "Array"),
        (&b"def f\n  h = [1, 2]\n  h.frobnicate_zzz if h.is_a?(Array)\nend\n"[..], "Array"),
        (&b"def f\n  h = [1, 2]\n  h.frobnicate_zzz if h.is_a?(Object)\nend\n"[..], "Array"),
        (&b"def f\n  h = [1, 2]\n  h.frobnicate_zzz if h.kind_of?(Enumerable)\nend\n"[..], "Array"),
        (&b"def f\n  h = [1, 2]\n  h.frobnicate_zzz if h.instance_of?(Array)\nend\n"[..], "Array"),
        (&b"def f\n  h = [1, 2]\n  if Enumerable === h\n    h.frobnicate_zzz\n  end\nend\n"[..], "Array"),
        (&b"def f\n  h = { a: 1 }\n  h.frobnicate_zzz if h.is_a?(Enumerable)\nend\n"[..], "Hash"),
        // (The two `ClassOrdering::Unknown`-on-a-NOMINAL rows that used to
        // sit here moved to the silence test at the `v0.3.8` re-pin —
        // upstream #533 item 4 widens that arm to `untyped`. The rows below
        // are the controls that the widening must NOT swallow.)
        //
        // A SHAPED carrier collapses to `Bot` on a PROVEN-DISJOINT guard,
        // and `Bot` is the JOIN IDENTITY — so the call AFTER the conditional
        // still fires where a widened one would be silent. (The `Unknown`
        // twin of this row moved to the silence test at the `v0.3.9` re-pin:
        // `6cde8381` gave `narrow_shape_to_class` the same `declines_bot?`
        // the nominal arm had.)
        (&b"def f\n  h = [1, 2]\n  h.frobnicate_yyy if h.is_a?(Comparable)\n  h.frobnicate_zzz\nend\n"[..], "Array"),
        // A TERMINATING truthy edge widens only the path that returns; the
        // code after runs on the untouched falsey edge.
        (&b"def f\n  h = Array.new\n  return if h.is_a?(UnknownZzz)\n  h.frobnicate_zzz\nend\n"[..], "Array"),
        // A REBIND after the widening clears it.
        (&b"def f\n  h = Array.new\n  h.frobnicate_yyy if h.is_a?(UnknownZzz)\n  h = Array.new\n  h.frobnicate_zzz\nend\n"[..], "Array"),
        // A widening established INSIDE a block does not escape it.
        (&b"def f\n  h = Array.new\n  [1].each do |_i|\n    h.frobnicate_yyy if h.is_a?(UnknownZzz)\n  end\n  h.frobnicate_zzz\nend\n"[..], "Array"),
        // S3 anti-over-suppression: a SHAPED carrier under a guard it IS a
        // subclass of survives and still witnesses. All three measured
        // firing on the reference (`… for [1, 2]` / `… for { a: 1 }`).
        (&b"def f\n  v = [1, 2]\n  return unless v.is_a?(Enumerable)\n  v.frobnicate_zzz\nend\n"[..], "Array"),
        (&b"def f\n  v = [1, 2]\n  return unless v.is_a?(Object)\n  v.frobnicate_zzz\nend\n"[..], "Array"),
        (&b"def f\n  v = { a: 1 }\n  return unless v.is_a?(Enumerable)\n  v.frobnicate_zzz\nend\n"[..], "Hash"),
        // the FALSEY edge of a disjoint guard is NOT narrowed
        // (`narrow_nominal_not_class` preserves it) — probes s_unless_body,
        // s_negated_if, s_case_when_else_branch
        (&b"def f\n  h = [1, 2]\n  unless h.is_a?(Hash)\n    h.frobnicate_zzz\n  end\nend\n"[..], "Array"),
        (&b"def f\n  h = [1, 2]\n  if !h.is_a?(Hash)\n    h.frobnicate_zzz\n  end\nend\n"[..], "Array"),
        (&b"def f\n  h = [1, 2]\n  case h\n  when Hash then 0\n  else h.frobnicate_zzz\n  end\nend\n"[..], "Array"),
        // a `when` clause whose conditions do NOT all collapse — the
        // reference unions them (probe case_multi_mixed)
        (&b"def f\n  h = [1, 2]\n  case h\n  when Hash, Array then h.frobnicate_zzz\n  else 0\n  end\nend\n"[..], "Array"),
        // a use BEFORE the guard, and AFTER the conditional (probes
        // before_guard_fires, after_branch)
        (&b"def f\n  h = [1, 2]\n  h.frobnicate_zzz\n  0 if h.is_a?(Hash)\nend\n"[..], "Array"),
        (&b"def f\n  h = [1, 2]\n  if h.is_a?(Hash)\n    0\n  end\n  h.frobnicate_zzz\nend\n"[..], "Array"),
        // a REBIND kills the fact — in the branch and inside a block
        // (probes bot_rebind_use, rebind_in_branch, bot_block_rebind)
        (&b"def f\n  h = [1, 2]\n  if h.is_a?(Hash)\n    h = [3, 4]\n    h.frobnicate_zzz\n  end\nend\n"[..], "Array"),
        (&b"def f\n  h = [1, 2]\n  if h.is_a?(Hash)\n    [1].each do\n      h = [3, 4]\n      h.frobnicate_zzz\n    end\n  end\nend\n"[..], "Array"),
        // a non-disjoint guard's early-return propagation still witnesses
        (&b"def f\n  h = [1, 2]\n  return 0 unless h.is_a?(Enumerable)\n  h.frobnicate_zzz\nend\n"[..], "Array"),
        // a nested `def` is an independent scope (probe bot_nested_def)
        (&b"def f\n  h = [1, 2]\n  if h.is_a?(Hash)\n    def g\n      h = [5, 6]\n      h.frobnicate_zzz\n    end\n  end\nend\n"[..], "Array"),
    ] {
        let diags = run(src);
        assert_eq!(
            diags.len(),
            1,
            "expected one undefined-method for {:?}, got {diags:?}",
            String::from_utf8_lossy(src)
        );
        assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
        assert_eq!(
            diags[0].message,
            format!("undefined method `frobnicate_zzz' for {expect}")
        );
    }
}

/// `h[k] ||= v` / `h[k] &&= v` / `h[k] op= v` — a compound index write stores
/// through `[]=` on its receiver, so the reference's `IndexWriteWidening`
/// (`index_write_widening.rb`, upstream #560) widens the binding exactly as
/// `h[k] = v` does: the literal shape is gone and a later ELEMENT read is
/// silent. These rows fired `call.undefined-method` for the STALE element
/// type (`for 1`) before `Node::IndexWrite` existed (rigor-rs#135). All
/// measured silent on the reference at `e59b7b89`.
#[test]
fn index_compound_writes_widen_the_receiver_binding() {
    for src in [
        // Straight-line, all three compound forms, Array and Hash seeds.
        &b"h = {a: 1}\nh[:a] += 1\nh[:a].upcase\n"[..],
        &b"h = {a: 1}\nh[:a] ||= 2\nh[:a].upcase\n"[..],
        &b"h = {a: 1}\nh[:a] &&= 2\nh[:a].upcase\n"[..],
        &b"a = [1]\na[0] += 1\na.first.upcase\n"[..],
        &b"a = [1]\na[0] ||= 2\na.first.upcase\n"[..],
        // Conditional / loop-contained writes widen to Dynamic — the read is
        // still silent (the reference's `Scope#join` equivalent declines).
        &b"h = {a: 1}\nif rand > 0\n  h[:a] += 1\nend\nh[:a].upcase\n"[..],
        &b"h = {a: 1}\nh[:a] += 1 if rand > 0\nh[:a].upcase\n"[..],
        // A second compound store on the already-widened carrier stays silent.
        &b"h = {a: 1}\nh[:a] += 1\nh[:a] += 2\nh[:a].upcase\n"[..],
    ] {
        let diags = run(src);
        assert!(
            diags.is_empty(),
            "expected silence for {:?}, got {diags:?}",
            String::from_utf8_lossy(src)
        );
    }

    // `h[k] ||= v` in an operand-transparent position ALSO records the
    // stored slot's type (`eval_index_or_write` →
    // `Scope#with_indexed_narrowing`, rigor-rs#325): `h[:a] ||= 1` stores
    // `narrow_truthy(1) | 1` = `1`, and the narrowing intercepts the later
    // `h[:a]` read ahead of the widened carrier — so the reference FIRES
    // `for 1` here (probed at `e59b7b89`), unlike the `||= 2` row above
    // where the stored `1 | 2` union keeps `upcase` silent.
    let diags = run(b"h = {a: 1}\nx = (h[:a] ||= 1)\nh[:a].upcase\n");
    assert_eq!(
        diags.len(),
        1,
        "expected one undefined-method (`for 1`), got {diags:?}"
    );
    assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
    assert_eq!(diags[0].message, "undefined method `upcase' for 1");
}

/// rigor-rs#325: `h[k] ||= v` in an operand-transparent recovery position —
/// a splat argument, a container element, anywhere the write evaluates
/// inline without its scope reaching a join — still records the stored
/// slot: `eval_index_or_write` runs `Scope#with_indexed_narrowing` keyed on
/// `(h, k)`, so a later `h[k]` read sees `narrow_truthy(h[k]) | v` instead
/// of the receiver's literal element type. `&&=` / `op=` never record (the
/// reference's `eval_index_write` widens only), and a multi-index `||=`
/// splices a region, not a slot. Every row probed at `e59b7b89`.
#[test]
fn index_or_write_records_the_stored_slot_through_transparent_positions() {
    for src in [
        // The primary reproducer: the stored `"s"` answers `h[:a]`, not the
        // literal's `1` — silent both engines.
        &b"h = {a: 1}\nputs(*[h[:a] ||= \"s\"])\nh[:a].upcase\n"[..],
        // `narrow_truthy(1) | 2` = `1 | 2` — a union keeps `upcase` silent.
        &b"h = {a: 1}\nputs(*[h[:a] ||= 2])\nh[:a].upcase\n"[..],
        &b"h = {a: 1}\nputs(*[h[:a] ||= nil])\nh[:a].upcase\n"[..],
        // `&&=` records no narrowing — the `[]=` widening alone silences.
        &b"h = {a: 1}\nputs(*[h[:a] &&= \"s\"])\nh[:a].upcase\n"[..],
        &b"h = {a: 1}\nputs(*[h[:a] &&= 2])\nh[:a].upcase\n"[..],
        // Array slot through a splat; a container-element position.
        &b"a = [1]\nputs(*[a[0] ||= \"s\"])\na[0].upcase\n"[..],
        &b"h = {a: 1}\nputs({k: h[:a] ||= \"s\"})\nh[:a].upcase\n"[..],
        // A later direct `h[k] = v` invalidates the recorded slot — the
        // read falls back to the widened carrier, still silent.
        &b"h = {a: 1}\nputs(*[h[:a] ||= \"s\"])\nh[:a] = 2\nh[:a].upcase\n"[..],
        // A multi-index `||=` splices a region (`single_index_argument`
        // declines the record); a joined-position `||=` keeps the `[]=`
        // widening only — both silent.
        &b"h = {a: 1}\nputs(*[h[0, 1] ||= \"s\"])\nh[:a].upcase\n"[..],
        &b"h = {a: 1}\nx = 1 rescue (h[:a] ||= \"s\")\nh[:a].upcase\n"[..],
    ] {
        let diags = run(src);
        assert!(
            diags.is_empty(),
            "expected silence for {:?}, got {diags:?}",
            String::from_utf8_lossy(src)
        );
    }
}

/// The narrowing's controls (rigor-rs#325): a `||=` storing a truthy
/// constant keeps firing on the STORED type (`narrow_truthy(1) | 1` = `1`),
/// and rebinding the receiver drops every slot fact rooted at it — `h[:a]`
/// on the fresh `{b: 3}` reads `nil`.
#[test]
fn index_or_write_narrowing_keeps_its_controls() {
    let diags = run(b"h = {a: 1}\nputs(*[h[:a] ||= 1])\nh[:a].upcase\n");
    assert_eq!(
        diags.len(),
        1,
        "expected one undefined-method (`for 1`), got {diags:?}"
    );
    assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
    assert_eq!(diags[0].message, "undefined method `upcase' for 1");

    let diags =
        run(b"h = {a: 1}\nputs(*[h[:a] ||= \"s\"])\nh = {b: 3}\nh[:a].upcase\n");
    assert_eq!(
        diags.len(),
        1,
        "expected one undefined-method (`for nil`), got {diags:?}"
    );
    assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
    assert_eq!(diags[0].message, "undefined method `upcase' for nil");
}

/// The widening's controls: the read WITHOUT the write keeps firing on the
/// pinned literal (the stale shape is real there), and the carrier itself
/// still witnesses a method Hash lacks — `for Hash`, the widened nominal.
#[test]
fn index_compound_write_widening_keeps_its_controls() {
    let diags = run(b"h = {a: 1}\nh[:a].upcase\n");
    assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
    assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);

    let diags = run(b"h = {a: 1}\nh[:a] += 1\nh.frobnicate_zzz\n");
    assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
    assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
    assert_eq!(diags[0].message, "undefined method `frobnicate_zzz' for Hash");
}

/// The suppression is per-CALL-NODE, keyed on the guarded local being the
/// receiver — not a span blanket over the branch. A call on a DIFFERENT
/// local, and a call nested in the suppressed call's own arguments, both
/// keep firing (the reference does exactly this: only the local's own
/// carrier is `Bot`, the branch is NOT dead).
#[test]
fn disjoint_guard_suppression_is_per_call_not_per_branch() {
    // The message-span offset of the method token in `<recv>.frobnicate_zzz`.
    fn at(src: &[u8], recv_call: &[u8]) -> usize {
        src.windows(recv_call.len()).position(|w| w == recv_call).unwrap() + 2
    }
    // probe scope_if_two_stmts — only the `g` statement survives
    let src = &b"def f\n  h = [1, 2]\n  g = [3, 4]\n  if h.is_a?(Hash)\n    h.frobnicate_zzz\n    g.frobnicate_zzz\n  end\nend\n"[..];
    let diags = run(src);
    assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
    assert_eq!(diags[0].start_offset, at(src, b"g.frobnicate_zzz"));
    // probe nest_arg_other — the ARGUMENT's own call fires, the receiver's
    // does not
    let src = &b"def f\n  h = [1, 2]\n  g = [3, 4]\n  h.frobnicate_zzz(g.frobnicate_zzz) if h.is_a?(Hash)\nend\n"[..];
    let diags = run(src);
    assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
    assert_eq!(diags[0].start_offset, at(src, b"g.frobnicate_zzz"));
    // probe nest_h_as_arg — the guarded local in ARGUMENT position does not
    // suppress the enclosing call
    let src = &b"def f\n  h = [1, 2]\n  g = [3, 4]\n  g.frobnicate_zzz(h) if h.is_a?(Hash)\nend\n"[..];
    let diags = run(src);
    assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
    assert_eq!(diags[0].start_offset, at(src, b"g.frobnicate_zzz"));
}

/// The narrowed witness stays SILENT when the method exists on the
/// narrowed class, and for the use-after-`if` decline. The `&&` predicate
/// that used to sit here now WITNESSES (stage 3a-1, probe c1a — the
/// reference fires); its falsey-edge control lives in the infer matrix.
#[test]
fn narrowed_local_silent_on_existing_method_and_declines() {
    assert!(run(b"def f(value)\n  if value.is_a?(Hash)\n    value.merge!(a: 1)\n  end\nend\n")
        .is_empty());
    assert_eq!(
        run(b"def f(value)\n  if value.is_a?(Hash) && value.foo\n    value.frobnicate_zzz\n  end\nend\n")
            .len(),
        1
    );
    assert!(run(
        b"def f(value)\n  if value.is_a?(Hash)\n  end\n  value.frobnicate_zzz\nend\n"
    )
    .is_empty());
}

#[test]
fn parenthesized_receiver_types_through_the_parens() {
    // `(15).frobnicate` — a parenthesized literal receiver types as its inner
    // Constant (parens are pure grouping), so undefined-method witnesses.
    // Real-corpus coverage-gap audit: closed ~13 undefined-method gaps.
    let diags = run(b"(15).frobnicate\n");
    assert_eq!(diags.len(), 1, "expected undefined-method, got {diags:?}");
    assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
    assert_eq!(diags[0].receiver_type.as_deref(), Some("15"));
    // A valid method through the parens stays silent.
    assert!(run(b"(15).succ\n").is_empty(), "valid method must be silent");
}

#[test]
fn known_method_is_silent() {
    let diags = run(b"s = \"Hello\"\ns.length\n");
    assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
}

#[test]
fn dynamic_receiver_is_silent() {
    // `@x` is an untyped ivar => Dynamic[top] => never guess. (An ivar, not a
    // bare `x`, so `call.unresolved-toplevel` — a separate rule — stays out.)
    let diags = run(b"@x.foo\n");
    assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
}

// --- call.possible-nil-receiver (the nilable-RBS-return slice) -----------

/// Diagnostics filtered to just the nil-receiver rule.
fn nil_diags(src: &[u8]) -> Vec<Diagnostic> {
    run(src)
        .into_iter()
        .filter(|d| d.rule_id == CALL_POSSIBLE_NIL_RECEIVER)
        .collect()
}

#[test]
fn nil_receiver_fires_on_nilable_core_return_no_guard() {
    // `s : String` (via String.new), `s.byteslice -> String?` mints
    // `String | nil`; `upcase` is on String, absent on NilClass; no guard
    // ⇒ fire. Byte-exact with the oracle (verified against the reference:
    // line 4, col 5, error). The nil-source RHS receiver `s` is a
    // NON-constant Nominal (the unfoldable case the oracle also fires on).
    let src = b"def f\n  s = String.new\n  x = s.byteslice(0, 2)\n  x.upcase\nend\n";
    let diags = nil_diags(src);
    assert_eq!(diags.len(), 1, "expected one nil-receiver diag, got {diags:?}");
    let d = &diags[0];
    assert_eq!(d.rule_id, CALL_POSSIBLE_NIL_RECEIVER);
    assert_eq!(d.severity, Severity::Error, "balanced profile ⇒ error");
    assert_eq!(d.source_family, "builtin");
    assert_eq!(d.method_name.as_deref(), Some("upcase"));
    assert_eq!(
        d.message,
        "possible nil receiver: `upcase' is undefined on NilClass"
    );
    // Anchored on the method-name token `upcase`.
    assert_eq!(&src[d.start_offset..d.end_offset], b"upcase");
}

#[test]
fn nil_receiver_silent_on_constant_receiver_oracle_folds() {
    // A LITERAL receiver (`"hello".byteslice`) is constant-folded by the
    // reference to a concrete non-nil value ⇒ it never sees `C | nil` and
    // stays silent. rigor-rs must NOT mint nil from a Constant RHS receiver
    // (the zero-FP keystone vs. the oracle's folding).
    let src = b"def f\n  x = \"hello\".byteslice(0, 2)\n  x.upcase\nend\n";
    assert!(
        nil_diags(src).is_empty(),
        "constant receiver must not mint nil (oracle folds it)"
    );
}

#[test]
fn nil_receiver_silent_on_method_present_on_nilclass() {
    // `to_s` lives on NilClass ⇒ the call is sound on the nil arm ⇒ silent
    // (matches NilClass's tiny method set: to_s/to_a/inspect/nil?/…).
    let src = b"def f\n  s = String.new\n  x = s.byteslice(0, 2)\n  x.to_s\nend\n";
    assert!(nil_diags(src).is_empty(), "to_s is on NilClass ⇒ silent");
}

#[test]
fn nil_receiver_silent_on_guards() {
    // Every guard form the decline scan recognizes ⇒ ZERO diagnostics
    // (each verified against the oracle, which narrows and stays silent).
    let prelude = "def f\n  s = String.new\n  x = s.byteslice(0, 2)\n";
    let cases: &[&str] = &[
        // `.nil?` guard then use.
        "  return if x.nil?\n  x.upcase\nend\n",
        // truthy guard via `unless`.
        "  raise unless x\n  x.upcase\nend\n",
        // x in an `if` predicate.
        "  if x then x.upcase end\nend\n",
        // x as a `&&` operand.
        "  x && x.upcase\nend\n",
        // safe-nav on x.
        "  x&.upcase\nend\n",
        // reassignment guarded by nil?.
        "  x = \"d\" if x.nil?\n  x.upcase\nend\n",
        // `||=` reassignment (op-write).
        "  x ||= \"d\"\n  x.upcase\nend\n",
    ];
    for tail in cases {
        let src = format!("{prelude}{tail}");
        let diags = nil_diags(src.as_bytes());
        assert!(
            diags.is_empty(),
            "guarded case must be silent:\n{src}\ngot {diags:?}"
        );
    }
}

#[test]
fn nil_receiver_silent_on_dynamic_and_chained_receiver() {
    // RHS receiver is a method param (Dynamic) ⇒ no known core class ⇒ no
    // mint. And a chained `n.to_s.byteslice` (n.to_s is Dynamic) ⇒ silent.
    let param = b"def f(s)\n  x = s.byteslice(0, 2)\n  x.upcase\nend\n";
    assert!(nil_diags(param).is_empty(), "Dynamic RHS receiver ⇒ silent");
    let chained = b"def f(n)\n  x = n.to_s.byteslice(0, 2)\n  x.upcase\nend\n";
    assert!(nil_diags(chained).is_empty(), "chained Dynamic ⇒ silent");
}

#[test]
fn nil_receiver_silent_on_non_nilable_return() {
    // `s.upcase -> String` (NOT nilable) ⇒ no nil minted ⇒ silent even
    // though `lenght` is absent (that path is undefined-method's job, and
    // here `length` is present so nothing fires at all).
    let src = b"def f\n  s = String.new\n  x = s.upcase\n  x.length\nend\n";
    assert!(
        nil_diags(src).is_empty(),
        "non-nilable return must not mint nil"
    );
}

// --- rigor-rs#352: String `[]=`/`||=`/index-target widening + nilable reads ---
//
// The reference's `StringMutation.widen_constant` (string_mutation.rb) drops a
// literal String's value pin to `Nominal[String]` under an in-place mutator —
// `[]=` included — and `String#[]` is RBS `String?`, so a post-mutation
// `s[k]` reads `String | nil`: a DIRECT chained call dispatches on the
// non-nil fragment (`try_non_nil_receiver_retry`), and a local assigned from
// the read is a `call.possible-nil-receiver` source. Every row below was
// probed byte-exact against the pinned reference (`e59b7b89`).

/// Post-`[]=` chained reads must not report `undefined-method` — the FP the
/// issue reports. The read is `String | nil`; `frobnicate_zzz` is absent on
/// BOTH arms, so nothing fires (the reference stays silent).
#[test]
fn string_index_write_then_chained_read_is_silent() {
    for src in [
        b"s = \"abc\"; s[0] = 5; s[0].frobnicate_zzz\n".as_slice(),
        b"s = \"abc\"; s[0] ||= \"x\"; s[0].frobnicate_zzz\n".as_slice(),
        b"s = \"abc\"; s[0], z = 5, 6; s[0].frobnicate_zzz\n".as_slice(),
        b"s = \"abc\"; s << \"x\"; s[0].frobnicate_zzz\n".as_slice(),
        b"s = \"abc\"; s.upcase!; s[0].frobnicate_zzz\n".as_slice(),
    ] {
        assert!(
            run(src).is_empty(),
            "post-mutation chained read must be silent: {:?}",
            std::str::from_utf8(src).unwrap()
        );
    }
}

/// The non-nil retry still types a DEFINED String method through the
/// `String | nil` receiver: `s[0].strip` answers `String`, so a bad method on
/// the RESULT witnesses `for String` exactly like the reference.
#[test]
fn string_index_write_strip_chain_witnesses_on_string() {
    let diags = run(b"s = \"abc\"; s[0] = 5; s[0].strip.frobnicate_zzz\n");
    assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
    let d = &diags[0];
    assert_eq!(d.rule_id, CALL_UNDEFINED_METHOD);
    assert_eq!(d.message, "undefined method `frobnicate_zzz' for String");
}

/// A local assigned from the post-mutation read is `String | nil`: `upcase`
/// is absent on NilClass and present on String ⇒ `possible-nil-receiver`
/// fires, for every store form that widens `s` (`[]=`, compound `+=`/`&&=`,
/// masgn / `for` / `rescue` index targets, and other String mutators).
#[test]
fn string_index_write_assigned_read_fires_possible_nil() {
    for src in [
        b"s = \"abc\"; s[0] = 5; x = s[0]; x.upcase\n".as_slice(),
        b"s = \"abc\"; s[0] += \"x\"; x = s[0]; x.upcase\n".as_slice(),
        b"s = \"abc\"; s[0] &&= \"x\"; x = s[0]; x.upcase\n".as_slice(),
        b"s = \"abc\"; s[0], z = 5, 6; x = s[0]; x.upcase\n".as_slice(),
        b"s = \"abc\"; for s[0] in [5]; end; x = s[0]; x.upcase\n".as_slice(),
        b"s = \"abc\"; begin; raise; rescue => s[0]; end; x = s[0]; x.upcase\n".as_slice(),
        b"s = \"abc\"; s << \"x\"; x = s[0]; x.upcase\n".as_slice(),
        b"s = \"abc\"; s.upcase!; x = s[0]; x.upcase\n".as_slice(),
    ] {
        let diags = nil_diags(src);
        assert_eq!(
            diags.len(),
            1,
            "expected one possible-nil diag for {:?}, got {diags:?}",
            std::str::from_utf8(src).unwrap()
        );
        assert_eq!(
            diags[0].message,
            "possible nil receiver: `upcase' is undefined on NilClass"
        );
    }
}

/// rigor-rs#352 review: a REBIND of the index-target local inside the `for` /
/// `begin-rescue` construct wins over the `[]=` widening — the post-construct
/// join types `s` from the pre-construct binding and the rebound value, so
/// `x = s[0]` is not a nilable `String | nil` source and `x.upcase` stays
/// silent. Every row probed silent on the reference; the port previously
/// reinserted the PRE-construct `[]=` widening over the rebind's Dynamic and
/// fired `call.possible-nil-receiver`.
#[test]
fn string_index_target_rebind_inside_construct_wins() {
    for src in [
        b"s = \"abc\"; for s[0] in [5]; s = \"q\"; end; x = s[0]; x.upcase\n".as_slice(),
        b"s = \"abc\"; for s[0] in [5]; z, s = 1, \"q\"; end; x = s[0]; x.upcase\n".as_slice(),
        b"s = \"abc\"; for s[0] in [5]; s = \"q\"; s = \"r\"; end; x = s[0]; x.upcase\n".as_slice(),
        b"s = \"abc\"; for s[0] in [5]; if true; s = \"q\"; end; end; x = s[0]; x.upcase\n"
            .as_slice(),
        b"s = \"abc\"; for s, s[0] in [[1,2]]; end; x = s[0]; x.upcase\n".as_slice(),
        b"s = \"abc\"; begin; raise; rescue => s[0]; s = \"q\"; end; x = s[0]; x.upcase\n"
            .as_slice(),
        b"s = \"abc\"; begin; raise; rescue => s[0]; ensure; s = \"q\"; end; x = s[0]; x.upcase\n"
            .as_slice(),
        // A chained call on the indexed read after a body rebind is silent on
        // both engines (the receiver is not a known-nilable source).
        b"s = \"abc\"; for s[0] in [5]; s = \"q\"; end; s[0].frobnicate\n".as_slice(),
        b"s = \"abc\"; begin; raise; rescue => s[0]; s = \"q\"; end; s[0].frobnicate\n"
            .as_slice(),
    ] {
        assert!(
            nil_diags(src).is_empty(),
            "body rebind must win over the []= widening: {:?}",
            std::str::from_utf8(src).unwrap()
        );
    }
}

/// Measured declines against the reference (the zero-FP-safe side): a
/// `s = nil` body arm joins `s` to `String | nil`, which the reference reads
/// as a nilable `[]` RECEIVER on `s[0]`; `s += "z"` is an op-write rebind the
/// reference still reads String-ish. The port keeps the conservatively
/// `Dynamic` binding and declines. Pinned silent so a future join-model fix
/// updates the pin deliberately.
#[test]
fn string_index_target_rebind_declined_gaps() {
    for src in [
        b"s = \"abc\"; for s[0] in [5]; s = nil; end; x = s[0]; x.upcase\n".as_slice(),
        b"s = \"abc\"; for s[0] in [5]; s = nil; end; s[0].frobnicate\n".as_slice(),
        b"s = \"abc\"; for s[0] in [5]; s += \"z\"; end; x = s[0]; x.upcase\n".as_slice(),
    ] {
        assert!(
            nil_diags(src).is_empty(),
            "documented decline vs the reference: {:?}",
            std::str::from_utf8(src).unwrap()
        );
    }
}

/// `s[k] ||= v` records the stored-slot narrowing
/// `s[k] -> narrow_truthy(s[k]) | v`: a non-nil `v` keeps the slot non-nil,
/// so `x = s[0]` is NOT a nilable source and `x.upcase` stays silent — but a
/// `nil` default keeps the slot nilable, and a later `[]=`/masgn store or a
/// rebind of `s` invalidates the record (each probed against the reference).
#[test]
fn string_index_or_write_slot_narrowing() {
    // `||= "x"` narrows the slot non-nil ⇒ silent.
    assert!(
        nil_diags(b"s = \"abc\"; s[0] ||= \"x\"; x = s[0]; x.upcase\n").is_empty(),
        "recorded non-nil slot must not mint a nilable fact"
    );
    // Rebind of `s` drops the slot record ⇒ still silent: the fresh literal
    // folds `s[0]` to a concrete char (keystone declines a Constant receiver).
    assert!(
        nil_diags(b"s = \"abc\"; s[0] ||= \"x\"; s = \"abc\"; x = s[0]; x.upcase\n")
            .is_empty(),
        "rebind must drop the slot record"
    );
    // Nilable record / invalidated record ⇒ fires.
    for src in [
        b"s = \"abc\"; s[0] ||= nil; x = s[0]; x.upcase\n".as_slice(),
        b"s = \"abc\"; s[0] ||= \"x\"; s[0] = 5; x = s[0]; x.upcase\n".as_slice(),
        b"s = \"abc\"; s[0] ||= \"x\"; s[0], z = 5, 6; x = s[0]; x.upcase\n".as_slice(),
        b"s = \"abc\"; s[0] ||= \"x\"; s << \"y\"; x = s[0]; x.upcase\n".as_slice(),
    ] {
        let diags = nil_diags(src);
        assert_eq!(
            diags.len(),
            1,
            "expected one possible-nil diag for {:?}, got {diags:?}",
            std::str::from_utf8(src).unwrap()
        );
    }
}

/// Methods defined on NilClass (`to_s`, `inspect`, `nil?`) never mint a
/// possible-nil diagnostic — the call is sound on the nil arm.
#[test]
fn string_index_read_nilclass_methods_stay_silent() {
    for src in [
        b"s = \"abc\"; s[0] = 5; x = s[0]; x.to_s.frobnicate_zzz\n".as_slice(),
        b"s = \"abc\"; s[0] = 5; x = s[0]; x.inspect.frobnicate_zzz\n".as_slice(),
        b"s = \"abc\"; s[0] = 5; x = s[0]; x.nil?.frobnicate_zzz\n".as_slice(),
    ] {
        assert!(
            nil_diags(src).is_empty(),
            "NilClass-defined method must not fire possible-nil: {:?}",
            std::str::from_utf8(src).unwrap()
        );
    }
}

#[test]
fn flags_wrong_arity_on_string_include() {
    // `String#include?` is arity (1, 1); two args is wrong-arity.
    let src = b"s = \"x\"\ns.include?(\"a\", \"b\")\n";
    let diags = run(src);
    assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
    let d = &diags[0];
    assert_eq!(d.rule_id, CALL_WRONG_ARITY);
    assert_eq!(d.severity, Severity::Error);
    assert_eq!(d.source_family, "builtin");
    assert_eq!(d.receiver_type.as_deref(), Some("String"));
    assert_eq!(d.method_name.as_deref(), Some("include?"));
    assert_eq!(
        d.message,
        "wrong number of arguments to `include?' on String (given 2, expected 1)"
    );
    // Anchored on the method-name token `include?`.
    assert_eq!(&src[d.start_offset..d.end_offset], b"include?");
}

#[test]
fn wrong_arity_renders_range_for_gsub() {
    // `String#gsub` is arity (1, 2); three args -> `expected 1..2`.
    let src = b"s = \"x\"\ns.gsub(\"a\", \"b\", \"c\")\n";
    let diags = run(src);
    assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
    let d = &diags[0];
    assert_eq!(d.rule_id, CALL_WRONG_ARITY);
    assert_eq!(d.severity, Severity::Error);
    assert_eq!(d.receiver_type.as_deref(), Some("String"));
    assert_eq!(d.method_name.as_deref(), Some("gsub"));
    assert_eq!(
        d.message,
        "wrong number of arguments to `gsub' on String (given 3, expected 1..2)"
    );
}

#[test]
fn correct_arity_is_silent() {
    // 1-arg include?, 1-arg and 2-arg gsub are all within envelope.
    assert!(run(b"s = \"x\"\ns.include?(\"a\")\n").is_empty());
    assert!(run(b"s = \"x\"\ns.gsub(\"a\")\n").is_empty());
    assert!(run(b"s = \"x\"\ns.gsub(\"a\", \"b\")\n").is_empty());
}

#[test]
fn nil_literal_receiver_is_undefined_method() {
    // `x = nil; x.upcase` — receiver types to Constant[Nil]; the reference
    // routes a definitely-nil receiver to `call.undefined-method`, not
    // `possible-nil-receiver`. We match that.
    let src = b"x = nil\nx.upcase\n";
    let diags = run(src);
    assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
    let d = &diags[0];
    assert_eq!(d.rule_id, CALL_UNDEFINED_METHOD);
    assert_eq!(d.severity, Severity::Error);
    assert_eq!(d.message, "undefined method `upcase' for nil");
    assert_eq!(&src[d.start_offset..d.end_offset], b"upcase");
}

#[test]
fn no_false_positives_on_valid_code() {
    // A spread of valid calls across modeled classes must stay silent —
    // no arity, undefined-method, or nil diagnostics.
    assert!(run(b"s = \"x\"\ns.upcase\n").is_empty());
    assert!(run(b"n = 1\nn.abs\n").is_empty());
    assert!(run(b"s = \"hi\"\ns.gsub(\"a\", \"b\")\n").is_empty());
    // Dynamic (ivar) receiver with any arity stays silent (never guess).
    assert!(run(b"@x.foo(1, 2, 3)\n").is_empty());
    // A nullary call in its valid form stays silent.
    assert!(run(b"s = \"x\"\ns.chars\n").is_empty());
}

#[test]
fn variadic_arity_method_does_not_fire() {
    // `String#concat` is variadic (`(*string | Integer) -> self`), so its
    // arity envelope has no upper bound => wrong-arity must NOT fire no
    // matter how many positional args are passed. (Real RBS now models a
    // concrete envelope for nearly every method; a variadic one is the case
    // where many args are still legal.)
    let diags = run(b"s = \"x\"\ns.concat(\"a\", \"b\")\n");
    assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
}

#[test]
fn wrong_arity_declines_splat_args() {
    // Issue #165 — the reference's `plain_positional_call?` declines ANY
    // call with a splat argument; the port must not count each `*a` as one
    // positional. Every shape below is silent on the oracle at `e59b7b89`.
    for src in [
        &b"[1, 2].first(*[5], *[5])\n"[..],
        b"w = [5]\n[1, 2].first(*w, *w)\n",
        b"def m(w) = [1, 2].first(*w, *w)\n",
        b"[1, 2].first(*[5], 1)\n",
        b"[1, 2].first(1, *[5])\n",
        b"[1, 2].first(*[], *[])\n",
        b"\"abc\".center(*[5], *[5], *[5])\n",
        b"[1, 2]&.first(1, *[5])\n",
    ] {
        let diags = run(src);
        assert!(
            diags.iter().all(|d| d.rule_id != CALL_WRONG_ARITY),
            "splat call must not fire wrong-arity: {diags:?} for {src:?}"
        );
    }
}

#[test]
fn wrong_arity_declines_keyword_hash_and_forwarding() {
    // The same `simple_positional?` gate declines a bare keyword-hash and a
    // `...` forwarding argument — both silent on the oracle.
    for src in [
        &b"[1, 2].first(1, a: 2)\n"[..],
        b"[1, 2].first(1, 2, a: 3)\n",
        b"def m(...) = [1, 2].first(1, ...)\n",
    ] {
        let diags = run(src);
        assert!(
            diags.iter().all(|d| d.rule_id != CALL_WRONG_ARITY),
            "non-plain-positional call must not fire wrong-arity: {diags:?} for {src:?}"
        );
    }
}

#[test]
fn wrong_arity_still_fires_with_anonymous_block_pass() {
    // The `&` anonymous block-pass rides Prism's `block()`, not
    // `arguments()`, so `plain_positional_call?` does not see it: the
    // oracle fires `first(1, 2, &)` (given 2, expected 0..1) and the port
    // must keep firing the same tuple.
    let src = b"def m(&) = [1, 2].first(1, 2, &)\n";
    let diags = run(src);
    assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
    let d = &diags[0];
    assert_eq!(d.rule_id, CALL_WRONG_ARITY);
    assert_eq!(
        d.message,
        "wrong number of arguments to `first' on Array (given 2, expected 0..1)"
    );
    assert_eq!(&src[d.start_offset..d.end_offset], b"first");
}

#[test]
fn block_bearing_call_is_not_witnessed() {
    // `{...}.select { block }.keys` — `select` with a block returns a Hash
    // (`.keys` is valid), and `select` with a block takes 0 positional args
    // (no wrong-arity). Block-form RETURN typing is now modeled, but a VALID
    // chained call on the (correct) block result must still stay silent.
    let diags = run(b"h = {a: 1}\nx = h.select { |k, v| v > 0 }.keys\n");
    assert!(diags.is_empty(), "block-call chain must be silent, got {diags:?}");
    // The same chain without the witnessing chain still silent on the block call.
    let diags2 = run(b"[1, 2].each_with_index { |e, i| e }\n");
    assert!(diags2.is_empty(), "expected no diagnostics, got {diags2:?}");
    // The exact reported FP shape (gitlab-foss authorize_granular_scopes_service.rb:102):
    // a hash-literal-shorthand receiver chained DIRECTLY into `.select { }.keys`.
    // Two FPs must NOT fire: (a) wrong-arity on `select` (block ⇒ 0 positional
    // args, but the no-block envelope is 1..N — arity stays silent on block
    // calls), and (b) undefined-method `keys` on the block result (the block
    // form returns Hash, on which `keys` is valid). The reference is silent on
    // this whole line; rigor-rs must be too.
    let diags3 = run(
        b"def f(token, boundaries, permissions)\n{ token:, boundaries:, permissions: }.select { |_, value| value.nil? }.keys\nend\n",
    );
    assert!(diags3.is_empty(), "literal-receiver block chain must be silent, got {diags3:?}");
}

#[test]
fn block_call_result_typo_is_witnessed() {
    // RECOVERED coverage (CURRENT_WORK §4): the block-form RETURN is now
    // RBS-modeled, so a typo on the CHAINED result is witnessed again,
    // matching the reference. Guarded on the real RBS tree (under the stub
    // fallback block returns are unmodeled ⇒ silent ⇒ no diagnostic to find).
    let idx = CoreIndex::new();
    if !idx.knows_class("Enumerable") || !idx.class_has_method("Array", "map") {
        return;
    }
    // `arr.map { }.frist` -> map block form returns Array; `.frist` undefined.
    let diags = run(b"arr = [1, 2, 3]\narr.map { |n| n + 1 }.frist\n");
    assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
    assert_eq!(diags[0].rule_id, "call.undefined-method");
    assert_eq!(diags[0].method_name.as_deref(), Some("frist"));

    // `arr.select { }.frist` -> Array; `.frist` undefined.
    let diags = run(b"arr = [1, 2, 3]\narr.select { |n| n > 1 }.frist\n");
    assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
    assert_eq!(diags[0].method_name.as_deref(), Some("frist"));

    // `arr.each { }.frist` -> `each` returns self (Array); `.frist` undefined.
    let diags = run(b"arr = [1, 2, 3]\narr.each { |n| n }.frist\n");
    assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
    assert_eq!(diags[0].method_name.as_deref(), Some("frist"));

    // `s.tap { }.lenght` -> `tap` returns self (String); `.lenght` undefined.
    let diags = run(b"s = \"hello\"\ns.tap { |x| x }.lenght\n");
    assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
    assert_eq!(diags[0].method_name.as_deref(), Some("lenght"));
}

#[test]
fn in_source_return_chain_typo_is_witnessed() {
    // ADR-0023 tier-4b: `user.full_name.lenght` where `def full_name;
    // "#{a} #{b}"; end` infers full_name : String, so `.lenght` on the
    // String result is witnessed against the real String RBS.
    let src = b"class User\n  def full_name\n    \"#{first} #{last}\"\n  end\nend\nuser = User.new\nuser.full_name.lenght\n";
    let diags = run(src);
    assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
    assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
    assert_eq!(diags[0].method_name.as_deref(), Some("lenght"));
    assert_eq!(diags[0].receiver_type.as_deref(), Some("String"));
}

#[test]
fn in_source_return_chain_valid_call_stays_silent() {
    // The other side: a VALID method on the inferred core return must NOT
    // fire — `full_name : String`, and `.length` is valid on String.
    let src = b"class User\n  def full_name\n    \"#{first} #{last}\"\n  end\nend\nuser = User.new\nuser.full_name.length\n";
    let diags = run(src);
    assert!(diags.is_empty(), "valid String#length on the inferred return must be silent, got {diags:?}");
}

#[test]
fn in_source_passthrough_param_return_is_witnessed() {
    // ADR-0023 tier-4b call-site PARAMETER BINDING: `def echo(x); x; end`
    // returns its arg's type, so `c.echo("a")` binds String and `.lenght`
    // witnesses against String — the reference witnesses the same call
    // (`undefined method 'lenght' for "a"`, same class, value-render aside).
    let src = b"class C\n  def echo(x)\n    x\n  end\nend\nc = C.new\nc.echo(\"a\").lenght\n";
    let diags = run(src);
    assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
    assert_eq!(diags[0].method_name.as_deref(), Some("lenght"));
    assert_eq!(diags[0].receiver_type.as_deref(), Some("String"));
}

#[test]
fn in_source_core_transform_param_return_is_witnessed() {
    // Core-transform via the param: `def up(x); x.upcase; end` returns the
    // core return of `String#upcase` (String) when the arg is a String, so
    // `.frob` on the result witnesses against String.
    let src = b"class C\n  def up(x)\n    x.upcase\n  end\nend\nc = C.new\nc.up(\"a\").frob\n";
    let diags = run(src);
    assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
    assert_eq!(diags[0].method_name.as_deref(), Some("frob"));
    assert_eq!(diags[0].receiver_type.as_deref(), Some("String"));
}

#[test]
fn in_source_param_bound_unknown_arg_is_silent() {
    // The decline side: a param-bound method whose ARG types Dynamic (an
    // unknown receiver's result) ⇒ no core class to bind ⇒ silent.
    let src = b"class C\n  def echo(x)\n    x\n  end\nend\nc = C.new\nc.echo(@whatever).lenght\n";
    let diags = run(src);
    assert!(diags.is_empty(), "param bound to an unknown-typed arg must stay silent, got {diags:?}");
}

#[test]
fn in_source_splat_param_method_is_silent() {
    // A splat signature declines param binding entirely (no 1:1 index map),
    // so even a String arg does not witness — a missed witness, never an FP.
    let src = b"class C\n  def echo(*xs)\n    xs\n  end\nend\nc = C.new\nc.echo(\"a\").lenght\n";
    let diags = run(src);
    assert!(diags.is_empty(), "splat-param method must decline param binding, got {diags:?}");
}

#[test]
fn block_call_result_valid_call_stays_silent() {
    // The other side of the recovery: a VALID method on the (correctly
    // modeled) block result must NOT fire — `Hash#select { }` returns Hash,
    // so `.keys` is valid (the FP class the placeholder originally guarded).
    let idx = CoreIndex::new();
    if !idx.knows_class("Enumerable") || !idx.class_has_method("Array", "map") {
        return;
    }
    // `h.select { }.keys` -> Hash#keys valid -> silent.
    let diags = run(b"h = { a: 1 }\nh.select { |k, v| v > 0 }.keys\n");
    assert!(diags.is_empty(), "Hash#select block result is Hash; .keys valid, got {diags:?}");
    // `h.reject { }.keys` -> Hash#reject block form returns Hash -> .keys valid.
    let diags = run(b"h = { a: 1 }\nh.reject { |k, v| v > 0 }.keys\n");
    assert!(diags.is_empty(), "Hash#reject block result is Hash; .keys valid, got {diags:?}");
    // `arr.map { }.first` -> Array#first valid -> silent.
    let diags = run(b"arr = [1, 2, 3]\narr.map { |n| n }.first\n");
    assert!(diags.is_empty(), "Array#map block result is Array; .first valid, got {diags:?}");
}

// --- in-source / non-core `.new` instances: reference leniency -----------
//
// The reference does NOT witness `undefined-method` on a project-defined
// class instance, nor on a non-core `X.new` instance (Pathname/Set/Struct):
// it gates on `rbs_class_known?` (check_rules.rb:556) and treats a miss there
// leniently (ADR-0023 tier-4). rigor-rs mirrors that — these receivers are
// typed (for chaining) but never witnessed. Every case below MUST be silent.

#[test]
fn in_source_instance_typo_is_silent_lenient() {
    // `class Point; def x; end; end; p = Point.new; p.y` — `y` is undefined on
    // Point, but Point is a project class (not RBS-known) ⇒ the reference stays
    // silent (leniency: Ruby defines methods dynamically). So must rigor-rs.
    let diags = run(b"class Point\n  def x\n  end\nend\np = Point.new\np.y\n");
    assert!(diags.is_empty(), "project-class miss must be silent, got {diags:?}");
}

#[test]
fn defined_in_source_method_is_silent() {
    // `p.x` where Point defines `x` ⇒ no diagnostic (and silent regardless).
    let diags = run(b"class Point\n  def x\n  end\nend\np = Point.new\np.x\n");
    assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
}

#[test]
fn inherited_object_method_on_source_instance_is_silent() {
    // `p.frozen?` — inherited from Object via the source class's implicit
    // super; must not be a false positive.
    let diags = run(b"class Point\n  def x\n  end\nend\np = Point.new\np.frozen?\n");
    assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
}

#[test]
fn unknown_superclass_keeps_source_instance_silent() {
    // `class User < ApplicationRecord; end; u = User.new; u.anything` — silent
    // both because the super is unknown AND because a project class is never
    // witnessed. The zero-FP keystone for Rails models.
    let diags = run(
        b"class User < ApplicationRecord\nend\nu = User.new\nu.totally_made_up_xyz\n",
    );
    assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
}

#[test]
fn source_subclass_typo_is_silent_lenient() {
    // `class Animal; def speak; end; end; class Dog < Animal; end` — neither
    // an inherited method nor a typo is witnessed on the project class `Dog`
    // (reference leniency), even though the chain Dog->Animal->Object is known.
    let ok = run(b"class Animal\n  def speak\n  end\nend\nclass Dog < Animal\nend\nd = Dog.new\nd.speak\n");
    assert!(ok.is_empty(), "inherited method must be silent, got {ok:?}");
    let bad = run(b"class Animal\n  def speak\n  end\nend\nclass Dog < Animal\nend\nd = Dog.new\nd.fly\n");
    assert!(bad.is_empty(), "project-class typo must be silent (leniency), got {bad:?}");
}

#[test]
fn reopened_source_class_is_silent_lenient() {
    // A project class is never witnessed, reopened or not — including a typo.
    assert!(run(b"class C\n  def a\n  end\nend\nclass C\n  def b\n  end\nend\nc = C.new\nc.a\n").is_empty());
    let typo = run(b"class C\n  def a\n  end\nend\nclass C\n  def b\n  end\nend\nc = C.new\nc.zzz\n");
    assert!(typo.is_empty(), "project-class typo must be silent (leniency), got {typo:?}");
}

#[test]
fn non_core_rbs_new_instance_typo_is_silent_lenient() {
    // `Pathname.new("a").nonexist` — Pathname is RBS-known but NOT a core
    // class round-tripped by id, so it resolves only through the registry
    // surface. The reference is silent on `Pathname.new.typo` (leniency on a
    // non-core `.new` instance); rigor-rs mirrors that — always silent.
    let diags = run(b"p = Pathname.new(\"a\")\np.nonexist\n");
    assert!(diags.is_empty(), "non-core .new instance miss must be silent, got {diags:?}");
}

#[test]
fn metaclass_constructor_chained_new_is_silent() {
    // `Struct.new(:a, :b).new(1, 2)` — `Struct.new` returns a CLASS, not a
    // Struct instance; the chained `.new` must not be witnessed absent.
    let diags = run(b"Struct.new(:a, :b).new(1, 2)\n");
    assert!(diags.is_empty(), "Struct.new(...).new must be silent, got {diags:?}");
}

#[test]
fn core_new_instance_typo_still_flags() {
    // The core `.new` path is still witnessed (matches the reference, which
    // flags `Array.new.bogus`): `Array` IS a core class round-tripped by id.
    let idx = CoreIndex::new();
    let diags = run(b"Array.new.bogus_xyz\n");
    if idx.knows_class("Array") {
        assert_eq!(diags.len(), 1, "expected core .new typo flagged, got {diags:?}");
        assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
        assert_eq!(diags[0].method_name.as_deref(), Some("bogus_xyz"));
    }
}

#[test]
fn real_rbs_method_on_rbs_instance_is_silent() {
    // `Pathname.new("a").basename` — a real method, never a false positive.
    let diags = run(b"p = Pathname.new(\"a\")\np.basename\n");
    assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
}

#[test]
fn dynamic_unknown_constant_new_is_silent() {
    // `Widget.new.foo` where Widget is unknown ⇒ Dynamic ⇒ silent.
    let diags = run(b"w = Widget.new\nw.foo\n");
    assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
}

// --- singleton (class-method) witnessing on bare constants ---------------
//
// A bare top-level RBS constant (`Time`, `Array`) types to `Singleton(C)`;
// a class-method typo on it is witnessed (`Time.current`), while real class
// methods, instance-only names, `.new`, and project-class collisions stay
// silent. All guarded on real RBS being loaded (stub ⇒ assert silent).

#[test]
fn time_current_flags_singleton() {
    let idx = CoreIndex::new();
    let diags = run(b"Time.current\n");
    if idx.knows_class("Time") {
        assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
        let d = &diags[0];
        assert_eq!(d.rule_id, CALL_UNDEFINED_METHOD);
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.message, "undefined method `current' for singleton(Time)");
        assert_eq!(d.receiver_type.as_deref(), Some("singleton(Time)"));
        assert_eq!(d.method_name.as_deref(), Some("current"));
    } else {
        assert!(diags.is_empty(), "stub fallback must be silent, got {diags:?}");
    }
}

#[test]
fn time_real_class_methods_and_new_are_silent() {
    // `Time.now` / `Time.name` are real class methods; `Time.new` constructs
    // an instance (intercepted before singleton typing). All silent.
    assert!(run(b"Time.now\n").is_empty(), "Time.now must be silent");
    assert!(run(b"Time.name\n").is_empty(), "Time.name must be silent");
    assert!(run(b"Time.new\n").is_empty(), "Time.new must be silent");
}

#[test]
fn array_wrap_flags_singleton_but_new_is_silent() {
    let idx = CoreIndex::new();
    // `Array.wrap` is an ActiveSupport extension, not core ⇒ flagged absent.
    // (`@x` ivar arg, not a bare `x`, so unresolved-toplevel stays out.)
    let diags = run(b"Array.wrap(@x)\n");
    if idx.knows_class("Array") {
        assert_eq!(diags.len(), 1, "expected Array.wrap flagged, got {diags:?}");
        assert_eq!(diags[0].message, "undefined method `wrap' for singleton(Array)");
        assert_eq!(diags[0].receiver_type.as_deref(), Some("singleton(Array)"));
    } else {
        assert!(diags.is_empty(), "stub fallback must be silent, got {diags:?}");
    }
    // `Array.new` constructs an instance ⇒ silent (not singleton-typed).
    assert!(run(b"Array.new\n").is_empty(), "Array.new must be silent");
}

#[test]
fn project_class_collision_is_silent() {
    // A file that DEFINES `class Group` and also calls `Group.where(1)`: even
    // though `Group` may be a top-level RBS class, the project defines it, so
    // the gate refuses to singleton-type it ⇒ silent (cross-file zero-FP).
    let diags = run(b"class Group\nend\nGroup.where(1)\n");
    assert!(diags.is_empty(), "project-class collision must be silent, got {diags:?}");
}

#[test]
fn secure_random_hex_is_silent_extend_surface() {
    // `SecureRandom.hex` — its class methods come via an `extend`ed module, so
    // the class-method surface is incomplete ⇒ conservative ⇒ silent.
    let diags = run(b"SecureRandom.hex\n");
    assert!(diags.is_empty(), "SecureRandom.hex must be silent, got {diags:?}");
}

// -- flow.dead-assignment --------------------------------------------
//
// Pure AST/structural; faithful port of `DeadAssignmentCollector`. Each test
// mirrors a skip/fire case verified against the oracle.

/// The single dead-assignment diagnostic in `src`, or panic if not exactly 1.
fn dead(src: &[u8]) -> Vec<Diagnostic> {
    run(src)
        .into_iter()
        .filter(|d| d.rule_id == FLOW_DEAD_ASSIGNMENT)
        .collect()
}

#[test]
fn dead_assignment_fires_on_genuine_dead_write() {
    // `def foo; result = 1; 77; end` — `result` is written, never read, and
    // not the trailing statement ⇒ fires. Byte-exact against the oracle.
    let src = b"def foo\n  result = 1\n  77\nend\n";
    let diags = dead(src);
    assert_eq!(diags.len(), 1, "expected one dead-assignment, got {diags:?}");
    let d = &diags[0];
    assert_eq!(d.rule_id, FLOW_DEAD_ASSIGNMENT);
    assert_eq!(d.severity, Severity::Warning);
    assert_eq!(d.source_family, "builtin");
    assert_eq!(d.receiver_type, None);
    assert_eq!(d.method_name, None);
    assert_eq!(d.message, "local `result' assigned in `foo' but never read");
    // Anchored on the NAME token `result` (col 3 in the oracle).
    assert_eq!(&src[d.start_offset..d.end_offset], b"result");
}

#[test]
fn dead_assignment_trailing_write_is_silent() {
    // `def foo; result = 1; end` — the write IS the trailing statement
    // (implicit return) ⇒ silent.
    assert!(dead(b"def foo\n  result = 1\nend\n").is_empty());
}

#[test]
fn dead_assignment_underscore_prefix_is_silent() {
    // `_unused` is intentionally-unused by convention ⇒ silent.
    assert!(dead(b"def foo\n  _unused = 1\n  77\nend\n").is_empty());
}

#[test]
fn dead_assignment_op_write_read_is_silent() {
    // THE FP-GATE CASE: `total = 0; total += 1; other` — the op-write reads
    // `total`, so `total` is read ⇒ the plain write must NOT flag.
    let diags = dead(b"def f\n  total = 0\n  total += 1\n  other\nend\n");
    assert!(diags.is_empty(), "op-write read must suppress dead-assignment, got {diags:?}");
    // and the same for ||= / &&=.
    assert!(dead(b"def f\n  x = 0\n  x ||= 5\n  y\nend\n").is_empty());
    assert!(dead(b"def f\n  x = 0\n  x &&= 5\n  y\nend\n").is_empty());
}

#[test]
fn dead_assignment_read_in_block_is_silent() {
    // A read inside a block body counts as a read ⇒ silent.
    let diags = dead(b"def f\n  x = 1\n  [1].each { |n| x }\n  77\nend\n");
    assert!(diags.is_empty(), "block read must suppress, got {diags:?}");
}

#[test]
fn dead_assignment_read_in_interpolation_is_silent() {
    // A read inside string interpolation counts as a read ⇒ silent.
    let diags = dead(b"def f\n  x = 1\n  \"v=#{x}\"\n  77\nend\n");
    assert!(diags.is_empty(), "interpolation read must suppress, got {diags:?}");
}

#[test]
fn dead_assignment_def_receiver_read_is_silent() {
    // A local used as a singleton-def RECEIVER (`def x.m`) IS read — the
    // receiver is evaluated in the enclosing scope. Real-corpus FP audit
    // (textbringer): before lowering the receiver, `x` looked assigned-but-
    // never-read here.
    let diags = dead(b"def f\n  x = Object.new\n  def x.m\n    1\n  end\n  77\nend\n");
    assert!(diags.is_empty(), "def-receiver read must suppress, got {diags:?}");
}

#[test]
fn dead_assignment_block_pass_read_is_silent() {
    // A read inside a `&expr` block-pass argument counts as a read ⇒ silent.
    // Regression: a `&action` block-pass previously lowered to nothing, so the
    // `action` read never surfaced in the arena and the loop-condition write
    // was falsely flagged (gitlab-foss after_commit_queue.rb, matched vs the
    // v0.2.6 oracle which stays silent).
    let src = b"def f\n  while x = q.pop\n    g(&x)\n  end\nend\n";
    assert!(dead(src).is_empty(), "block-pass `&x` read must suppress, got {:?}", dead(src));
    // The direct form too: `foo(&blk)` after `blk = ...`.
    assert!(
        dead(b"def f\n  blk = make\n  run(&blk)\nend\n").is_empty(),
        "a `&blk` read must count"
    );
}

/// rigor-rs#137 (upstream rigor#1245): a block/lambda in VALUE position is a
/// lexical boundary — a name the closure BINDS reads `Dynamic[top]` inside,
/// never the outer local it shadows; captured names still read the outer
/// binding; and the shadowed write inside never leaks out.
#[test]
fn block_param_shadows_outer_local_in_value_position() {
    // The issue row: `o` inside `map { |o| … }` is the parameter, not the
    // outer `{ x: 1 }` — reference-silent, and so are we.
    assert!(
        run(b"def show(x) = x\no = { x: 1 }\nshow([1, 2].map { |o| o + 1 })\n").is_empty(),
        "bound `o` must not read the outer hash shape"
    );
    // The same-name write inside the block is the closure's own — the outer
    // `o` keeps its binding and still fires afterwards.
    let diags =
        run(b"def show(x) = x\no = { x: 1 }\nshow([1].each { |o| o = 2 })\no.frobnicate\n");
    assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
    assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
    // A CAPTURED name is not shadowed: `o` inside reads the outer binding and
    // still fires.
    let diags =
        run(b"def show(x) = x\no = { x: 1 }\nshow([1, 2].map { |x| o + 1 })\n");
    assert_eq!(diags.len(), 1, "captured `o` must still witness, got {diags:?}");
    assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
    // Lambdas, `;` block-locals, `do…end` and implicit `it` are the same
    // boundary.
    assert!(run(b"def show(x) = x\no = { x: 1 }\nshow(->(o) { o + 1 })\n").is_empty());
    assert!(run(b"def show(x) = x\no = { x: 1 }\nshow([1].map { |y; o| o + 1 })\n").is_empty());
    assert!(run(b"def show(x) = x\no = { x: 1 }\nshow([1].map do |o| o + 1 end)\n").is_empty());
    // A bound name under a CROSSED closure (no `Node::Call` carries its
    // `locals`) is shadowed too — `super { |o| … }`.
    assert!(
        run(b"def m\n  o = nil\n  super { |o| o.frobnicate }\nend\n").is_empty(),
        "crossed-block `o` is the parameter, not the outer nil"
    );
}

#[test]
fn dead_assignment_nested_def_isolation() {
    // An OUTER write read only by an INNER def is a closure capture? No — a
    // nested `def` is a fresh scope, but the reference gathers READS with no
    // def barrier, so an inner read of `x` DOES count. Conversely the inner
    // def's OWN write `y` is scanned as its own unit and fires there. Here we
    // assert: outer `x` written+read-in-inner is silent; inner `y` dead fires
    // (one diagnostic, anchored in the inner def).
    let src = b"def outer\n  x = 1\n  def inner\n    y = 2\n    3\n  end\n  x\nend\n";
    let diags = dead(src);
    assert_eq!(diags.len(), 1, "expected one (inner y), got {diags:?}");
    assert_eq!(diags[0].message, "local `y' assigned in `inner' but never read");
    // And the outer write is NOT a candidate inside `inner` (def barrier on
    // writes): `def inner` body doesn't see outer `x`.
    assert!(!diags.iter().any(|d| d.message.contains("`x'")));
}

#[test]
fn dead_assignment_multi_write_is_silent() {
    // `a, b = foo` lowers to `Node::MultiWrite`, NOT `LocalVariableWrite`, so
    // its targets are never dead-assignment candidates ⇒ silent, matching the
    // reference (`DeadAssignmentCollector` skips `MultiWriteNode` — its write
    // semantics are intertwined with a wider tuple binding). This is a PARITY
    // GUARD: the multi-write arena lowering must not make destructured
    // targets fireable.
    let diags = dead(b"def f\n  a, b = bar\n  77\nend\n");
    assert!(diags.is_empty(), "multi-write must be silent, got {diags:?}");
    // Also silent for the splat / nested / ignorable target forms.
    let diags = dead(b"def f\n  a, (b, c), *r = bar\n  77\nend\n");
    assert!(diags.is_empty(), "multi-write forms must be silent, got {diags:?}");
}

#[test]
fn dead_assignment_reads_inside_multi_write_targets_count() {
    // FP REGRESSION (netrc 0.11.0 `Netrc#[]=`, caught by the corpus sweep):
    // a NON-LOCAL multi-write target embeds a local READ (`item` in
    // `item[3], item[5] = info`). The reference gathers reads from the whole
    // Prism subtree, so `item` is read and `item = …` is NOT dead. The arena
    // must therefore carry those embedded expressions (`MultiWrite::
    // target_exprs`) — dropping them made `item = …` fire.
    let src = b"def setter(k, info)\n  if item = @data.detect { |d| d[1] == k }\n    item[3], item[5] = info\n  end\nend\n";
    let diags = dead(src);
    assert!(diags.is_empty(), "a read inside a multi-write target counts, got {diags:?}");
}

#[test]
fn dead_assignment_top_level_and_class_body_writes_are_silent() {
    // Top-level and class/module BODY assignments are never scanned (only
    // named def bodies are) ⇒ silent.
    assert!(dead(b"result = 1\n77\n").is_empty());
    assert!(dead(b"class C\n  CONST_LOCAL = 1\n  77\nend\n").is_empty());
}

#[test]
fn dead_assignment_fires_inside_class_method_body() {
    // A genuine dead write inside a class instance method fires, named by the
    // method (`bar`), exactly once (no double-emit from the method_bodies
    // harvest).
    let src = b"class C\n  def bar\n    tmp = 1\n    99\n  end\nend\n";
    let diags = dead(src);
    assert_eq!(diags.len(), 1, "expected one, got {diags:?}");
    assert_eq!(diags[0].message, "local `tmp' assigned in `bar' but never read");
}

#[test]
fn dead_assignment_read_after_write_is_silent() {
    // The basic positive-control: a write that IS later read stays silent.
    assert!(dead(b"def f\n  x = 1\n  x\n  77\nend\n").is_empty());
}

#[test]
fn dead_assignment_begin_rescue_trailing_unwrapped() {
    // A method whose body is a `begin ... end` — the trailing statement is the
    // begin block's last statement. `result = 1` as that tail is an implicit
    // return ⇒ silent.
    let src = b"def f\n  begin\n    result = 1\n  end\nend\n";
    assert!(dead(src).is_empty(), "begin-wrapped trailing write must be silent");
}

// -- flow.unreachable-branch ------------------------------------------
//
// Pure SYNTACTIC/AST; faithful port of `unreachable_branch_diagnostic`. Each
// case was verified byte-exact against the Ruby oracle. The keyword-inversion
// (`if` vs `unless`) cases are the parity keystone: anchoring on the wrong
// branch would land the diagnostic on LIVE code.

/// The `flow.unreachable-branch` diagnostics in `src`, in source order.
fn unreach(src: &[u8]) -> Vec<Diagnostic> {
    run(src)
        .into_iter()
        .filter(|d| d.rule_id == FLOW_UNREACHABLE_BRANCH)
        .collect()
}

/// 1-based (line, column) of a byte offset in `src` — the same coordinates the
/// CLI/JSON reporter prints, so anchors can be asserted against the oracle.
fn line_col(src: &[u8], offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for &b in &src[..offset] {
        if b == b'\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

#[test]
fn unreachable_branch_if_false_anchors_dead_then() {
    // `if false…else…` — falsey predicate, THEN branch dead. Oracle: 2:3
    // (the dead then-branch's first statement), "always falsey".
    let src = b"if false\n  dead_then\nelse\n  live_else\nend\n";
    let d = unreach(src);
    assert_eq!(d.len(), 1, "expected one diagnostic, got {d:?}");
    assert_eq!(d[0].message, "unreachable branch: literal predicate is always falsey");
    assert_eq!(d[0].severity, Severity::Warning);
    assert_eq!(line_col(src, d[0].start_offset), (2, 3));
}

#[test]
fn unreachable_branch_unless_false_anchors_dead_else() {
    // `unless false…else…` — the KEYWORD INVERTS: falsey predicate kills the
    // ELSE branch. Oracle: 3:1 (the `else` keyword), "always falsey".
    let src = b"unless false\n  live_then\nelse\n  dead_else\nend\n";
    let d = unreach(src);
    assert_eq!(d.len(), 1, "expected one diagnostic, got {d:?}");
    assert_eq!(d[0].message, "unreachable branch: literal predicate is always falsey");
    assert_eq!(line_col(src, d[0].start_offset), (3, 1));
}

#[test]
fn unreachable_branch_if_true_anchors_dead_else() {
    // `if true…else…` — truthy predicate kills the ELSE branch. Oracle: 3:1
    // (the `else` keyword), "always truthy".
    let src = b"if true\n  live\nelse\n  dead\nend\n";
    let d = unreach(src);
    assert_eq!(d.len(), 1, "expected one diagnostic, got {d:?}");
    assert_eq!(d[0].message, "unreachable branch: literal predicate is always truthy");
    assert_eq!(line_col(src, d[0].start_offset), (3, 1));
}

#[test]
fn unreachable_branch_if_nil_kills_then() {
    // `nil` is falsey ⇒ THEN dead, "always falsey".
    let src = b"if nil\n  dead_n\nelse\n  live_n\nend\n";
    let d = unreach(src);
    assert_eq!(d.len(), 1, "expected one diagnostic, got {d:?}");
    assert_eq!(d[0].message, "unreachable branch: literal predicate is always falsey");
    assert_eq!(line_col(src, d[0].start_offset), (2, 3));
}

#[test]
fn unreachable_branch_truthy_literals_kill_else() {
    // Integer / String / Symbol literals are all truthy in Ruby (incl. `0`,
    // `""`) ⇒ ELSE dead, "always truthy". Verified against the oracle.
    for src in [
        b"if 5\n  a\nelse\n  b\nend\n".as_slice(),
        b"if \"x\"\n  a\nelse\n  b\nend\n".as_slice(),
        b"if :sym\n  a\nelse\n  b\nend\n".as_slice(),
    ] {
        let d = unreach(src);
        assert_eq!(d.len(), 1, "expected one diagnostic for {src:?}, got {d:?}");
        assert_eq!(
            d[0].message,
            "unreachable branch: literal predicate is always truthy"
        );
    }
}

#[test]
fn unreachable_branch_if_false_no_else_anchors_then() {
    // `if false; dead; end` (no else) — THEN dead, still fires (no else node
    // is needed; the dead branch is the present, non-empty then). Oracle: 2:3.
    let src = b"if false\n  dead_only\nend\n";
    let d = unreach(src);
    assert_eq!(d.len(), 1, "expected one diagnostic, got {d:?}");
    assert_eq!(line_col(src, d[0].start_offset), (2, 3));
}

#[test]
fn unreachable_branch_empty_dead_then_is_silent() {
    // `if false` with an EMPTY then but a live else — the dead (then) branch
    // is absent ⇒ DECLINE (verified silent in the oracle).
    let src = b"if false\nelse\n  live2\nend\n";
    assert!(unreach(src).is_empty(), "empty dead then must be silent");
    // `if false; end` — both branches empty ⇒ DECLINE.
    assert!(unreach(b"if false\nend\n").is_empty(), "no branches must be silent");
}

#[test]
fn unreachable_branch_empty_else_node_still_fires() {
    // `if true…else[empty]` — truthy kills the ELSE branch; the `else` clause
    // NODE exists even though its body is empty, so the oracle FIRES at 3:1.
    let src = b"if true\n  live\nelse\nend\n";
    let d = unreach(src);
    assert_eq!(d.len(), 1, "empty-but-present else node must fire, got {d:?}");
    assert_eq!(line_col(src, d[0].start_offset), (3, 1));
}

#[test]
fn unreachable_branch_non_literal_is_silent() {
    // Non-literal predicate (`if x`) ⇒ DECLINE.
    assert!(unreach(b"if x\n  a\nelse\n  b\nend\n").is_empty(), "variable predicate silent");
    // Constant predicate (`if DEBUG`) ⇒ DECLINE: the reference uses SYNTACTIC
    // literal detection, NOT the folder, so a constant never flags.
    assert!(
        unreach(b"if DEBUG\n  a\nelse\n  b\nend\n").is_empty(),
        "constant predicate must not fold ⇒ silent"
    );
    // Interpolated string (`"a#{x}"`) is NOT a plain literal ⇒ DECLINE.
    assert!(
        unreach(b"if \"a#{x}b\"\n  a\nelse\n  b\nend\n").is_empty(),
        "interpolated string predicate silent"
    );
}

#[test]
fn unreachable_branch_while_true_is_silent() {
    // `while true` is a LOOP (a different rule's territory), not an If ⇒ this
    // rule is silent here.
    assert!(
        unreach(b"while true\n  loopy\nend\n").is_empty(),
        "while-true is not unreachable-branch"
    );
}

#[test]
fn unreachable_branch_ternary_fires() {
    // Prism parses a ternary as an IfNode, so a literal-predicate ternary is
    // flagged too (verified against the oracle: `false ? a : b` fires falsey).
    let d = unreach(b"x = false ? aa : bb\n");
    assert_eq!(d.len(), 1, "literal-predicate ternary must fire, got {d:?}");
    assert_eq!(d[0].message, "unreachable branch: literal predicate is always falsey");
}

// -- flow.always-truthy-condition -------------------------------------
//
// The inferred-constant counterpart to unreachable-branch (ADR-0022 first
// flow slice). Fires only when the dominating flow scope folds the predicate
// to a `Type::Constant`; the branch-join is the zero-FP keystone. Each
// positive was verified byte-exact (rule, line, column) against the oracle.

/// The `flow.always-truthy-condition` diagnostics in `src`, in source order.
fn always_truthy(src: &[u8]) -> Vec<Diagnostic> {
    run(src)
        .into_iter()
        .filter(|d| d.rule_id == FLOW_ALWAYS_TRUTHY_CONDITION)
        .collect()
}

#[test]
fn always_truthy_literal_assigned_constant_fires() {
    // `ca = 5; if ca` — `ca` folds to `5` (dominating straight-line write) ⇒
    // always truthy. Oracle: 2:4 (the predicate node), "always truthy".
    let src = b"ca = 5\nif ca\n  puts ca\nend\n";
    let d = always_truthy(src);
    assert_eq!(d.len(), 1, "expected one diagnostic, got {d:?}");
    assert_eq!(
        d[0].message,
        "condition is always truthy (the surrounding flow proves it folds to a constant)"
    );
    assert_eq!(d[0].severity, Severity::Warning);
    assert_eq!(line_col(src, d[0].start_offset), (2, 4));
}

#[test]
fn always_truthy_nil_constant_is_falsey() {
    // `cb = nil; if cb` — only nil/false are falsey ⇒ "always falsey".
    let src = b"cb = nil\nif cb\n  noop\nend\n";
    let d = always_truthy(src);
    assert_eq!(d.len(), 1, "expected one diagnostic, got {d:?}");
    assert_eq!(
        d[0].message,
        "condition is always falsey (the surrounding flow proves it folds to a constant)"
    );
    assert_eq!(line_col(src, d[0].start_offset), (2, 4));
}

#[test]
fn always_truthy_inferred_fold_fires() {
    // `cc = 1 + 1; if cc` — an INFERRED constant (folded arithmetic, not a
    // syntactic literal). This is the case unreachable-branch cannot reach.
    let d = always_truthy(b"cc = 1 + 1\nif cc\n  noop\nend\n");
    assert_eq!(d.len(), 1, "inferred-constant predicate must fire, got {d:?}");
    assert!(d[0].message.contains("always truthy"));
}

#[test]
fn always_truthy_unless_false_is_falsey() {
    // The `unless` keyword: predicate `cd` folds to `false` ⇒ "always falsey"
    // (polarity is the predicate VALUE, independent of which branch runs).
    let d = always_truthy(b"cd = false\nunless cd\n  noop\nend\n");
    assert_eq!(d.len(), 1, "unless-false predicate must fire, got {d:?}");
    assert!(d[0].message.contains("always falsey"));
}

#[test]
fn always_truthy_branch_reassignment_widens_silent() {
    // THE KEYSTONE. `na = 5`, then a CONDITIONAL reassignment ⇒ `na` is
    // `5 | <recompute>` at the second `if` — the flow join widens it, so NO
    // fire. The flat (non-flow) env would keep `na = 5` and falsely fire.
    let src = b"na = 5\nif guard\n  na = recompute\nend\nif na\n  noop\nend\n";
    assert!(
        always_truthy(src).is_empty(),
        "a conditionally-reassigned local must NOT fold to a constant"
    );
}

#[test]
fn always_truthy_multi_write_rebind_widens_silent() {
    // FP REGRESSION (2026-07-25). `x = 5` then a MULTI-WRITE rebind
    // `x, _y = other, 2` ⇒ `x` is the destructured `Dynamic[top]` slot, not
    // `5`, so `if x` must be SILENT. Measured against the oracle before the
    // `Node::MultiWrite` lowering: rigor-rs fired at 4:6, the reference was
    // silent — the multi-write's target names never reached
    // `collect_flow_writes`, so the earlier binding survived the rebind.
    let src = b"def probe(other)\n  x = 5\n  x, _y = other, 2\n  if x\n    puts \"truthy\"\n  end\nend\n";
    assert!(
        always_truthy(src).is_empty(),
        "a multi-write rebind must widen the earlier binding"
    );
    // Same at top level, and through a nested / splat target.
    assert!(
        always_truthy(b"x = 5\na, (x, c) = other\nif x\n  noop\nend\n").is_empty(),
        "a NESTED multi-write target must widen the earlier binding"
    );
    assert!(
        always_truthy(b"x = 5\na, *x = other\nif x\n  noop\nend\n").is_empty(),
        "a SPLAT multi-write target must widen the earlier binding"
    );
}

#[test]
fn always_truthy_defensive_predicate_silent() {
    // A defensive predicate call (`nil?`/`empty?`/…) reads as an explicit
    // runtime check; the reference skips it ⇒ silent.
    assert!(
        always_truthy(b"nb = 5\nif nb.nil?\n  noop\nend\n").is_empty(),
        "defensive `.nil?` predicate must be silent"
    );
}

#[test]
fn always_truthy_loop_nested_silent() {
    // A predicate inside a loop/block body is suppressed (loop-mutation
    // modelling is incomplete) ⇒ silent, matching the reference envelope.
    let src = b"nc = 7\nwhile guard\n  if nc\n    noop\n  end\nend\n";
    assert!(always_truthy(src).is_empty(), "loop-nested predicate must be silent");
}

#[test]
fn always_truthy_param_never_folds_silent() {
    // A method parameter is `Dynamic[top]`, never a constant ⇒ silent.
    let src = b"def m(flag)\n  if flag\n    noop\n  end\nend\n";
    assert!(always_truthy(src).is_empty(), "a param predicate must never fold");
}

#[test]
fn always_truthy_skips_syntactic_literal() {
    // A SYNTACTIC literal predicate is owned by unreachable-branch; always-
    // truthy must NOT double-fire on it (the reference skips literals here).
    assert!(
        always_truthy(b"if true\n  live\nend\n").is_empty(),
        "literal predicate is unreachable-branch's domain, not always-truthy's"
    );
}

// -- ADR-0038 interprocedural literal-tail fold (end-to-end) -----------

#[test]
fn always_falsey_const_singleton_fold() {
    // `M.ro? -> false` ⇒ `if M.ro?` is always falsey. Byte-parity with the
    // oracle: message + the predicate-node anchor.
    let src = b"module M\n  def self.ro?\n    false\n  end\nend\nif M.ro?\n  noop\nend\n";
    let d = always_truthy(src);
    assert_eq!(d.len(), 1, "expected one diagnostic, got {d:?}");
    assert_eq!(
        d[0].message,
        "condition is always falsey (the surrounding flow proves it folds to a constant)"
    );
    // Anchor: the predicate `M.ro?` on line 6, column 4 (1-based, after `if `).
    assert_eq!(line_col(src, d[0].start_offset), (6, 4));
}

#[test]
fn always_truthy_const_singleton_depth_two_bang_fold() {
    // `read_write? = !read_only?` ⇒ `if Gitlab::Database.read_write?` is
    // always TRUTHY (the depth-2 interprocedural fold).
    let src = b"module Gitlab\n  module Database\n    def self.read_only?\n      false\n    end\n    def self.read_write?\n      !read_only?\n    end\n  end\nend\nif Gitlab::Database.read_write?\n  noop\nend\n";
    let d = always_truthy(src);
    assert_eq!(d.len(), 1, "expected one diagnostic, got {d:?}");
    assert!(d[0].message.contains("always truthy"), "got {}", d[0].message);
}

#[test]
fn always_falsey_implicit_self_instance_fold() {
    // An implicit-self `flag` in the SAME class folds to false.
    let src = b"class Widget\n  def flag\n    false\n  end\n  def check\n    if flag\n      noop\n    end\n  end\nend\n";
    let d = always_truthy(src);
    assert_eq!(d.len(), 1, "expected one diagnostic, got {d:?}");
    assert!(d[0].message.contains("always falsey"), "got {}", d[0].message);
}

#[test]
fn always_truthy_assignment_rhs_if_fold() {
    // An `if`-expression assigned to a local still fires on a folded predicate.
    let src = b"module M\n  def self.on?\n    true\n  end\nend\nx = if M.on?\n  1\nelse\n  2\nend\n";
    let d = always_truthy(src);
    assert_eq!(d.len(), 1, "expected one diagnostic, got {d:?}");
    assert!(d[0].message.contains("always truthy"), "got {}", d[0].message);
}

#[test]
fn always_truthy_defensive_predicate_name_silent() {
    // A project method literally named `empty?` is in the defensive skip
    // envelope — even though it folds, always-truthy must not fire.
    let src = b"class C\n  def empty?\n    false\n  end\n  def check\n    if empty?\n      noop\n    end\n  end\nend\n";
    assert!(
        always_truthy(src).is_empty(),
        "defensive-named predicate must stay silent"
    );
}

#[test]
fn always_truthy_cross_owner_const_call_silent() {
    // `Foo.read_only?` where `Bar` (not `Foo`) owns `read_only?` — own-class
    // resolution declines ⇒ no diagnostic (zero-FP keystone).
    let src = b"class Foo\nend\nmodule Bar\n  def self.read_only?\n    false\n  end\nend\nif Foo.read_only?\n  noop\nend\n";
    assert!(
        always_truthy(src).is_empty(),
        "cross-owner const call must not fold"
    );
}

#[test]
fn always_truthy_loop_nested_fold_silent() {
    // A folded implicit-self predicate INSIDE a block/loop is suppressed
    // (the reference's loop/block skip envelope).
    let src = b"class C\n  def flag\n    false\n  end\n  def check\n    [1].each do |i|\n      if flag\n        noop\n      end\n    end\n  end\nend\n";
    assert!(
        always_truthy(src).is_empty(),
        "loop-nested folded predicate must be silent"
    );
}

// -- call.unresolved-toplevel -----------------------------------------
//
// An implicit-self call at TOPLEVEL (outside any class/module) whose name
// resolves against neither the Object/Kernel surface nor a same-file toplevel
// def. Each case verified byte-exact (rule, line, column) against the oracle.

/// The `call.unresolved-toplevel` diagnostics in `src`, in source order.
fn unresolved(src: &[u8]) -> Vec<Diagnostic> {
    run(src)
        .into_iter()
        .filter(|d| d.rule_id == CALL_UNRESOLVED_TOPLEVEL)
        .collect()
}

#[test]
fn unresolved_toplevel_fires_on_undefined_call() {
    // A bare implicit-self call to an undefined method at toplevel. Oracle:
    // 1:1, method `undefined_xyz`.
    let src = b"undefined_xyz\n";
    let d = unresolved(src);
    assert_eq!(d.len(), 1, "expected one diagnostic, got {d:?}");
    assert_eq!(d[0].severity, Severity::Warning);
    assert!(d[0].message.starts_with("unresolved toplevel call to `undefined_xyz`"));
    assert_eq!(line_col(src, d[0].start_offset), (1, 1));
}

#[test]
fn unresolved_toplevel_kernel_method_resolves_silent() {
    // Kernel methods (`def self?.x` in core RBS ⇒ instance methods on Kernel,
    // included by Object) resolve ⇒ silent.
    for src in [
        b"puts \"x\"\n".as_slice(),
        b"require \"set\"\n".as_slice(),
        b"loop { break }\n".as_slice(),
        b"raise \"e\"\n".as_slice(),
    ] {
        assert!(
            unresolved(src).is_empty(),
            "Kernel call must resolve silently: {src:?}"
        );
    }
}

#[test]
fn unresolved_toplevel_same_file_def_silent() {
    // A same-file toplevel `def` resolves a later toplevel call to it.
    assert!(
        unresolved(b"def helper\n  42\nend\nhelper\n").is_empty(),
        "a same-file toplevel def must resolve the call"
    );
}

#[test]
fn unresolved_toplevel_inside_class_body_silent() {
    // An implicit-self call inside a class/module body is NOT toplevel
    // (ADR-24 leniency) ⇒ silent even when unresolved.
    assert!(
        unresolved(b"class Widget\n  some_macro\n  def run\n    also_missing\n  end\nend\n")
            .is_empty(),
        "in-class implicit-self calls are not toplevel"
    );
}

#[test]
fn unresolved_toplevel_fires_inside_toplevel_def_body() {
    // A toplevel `def`'s BODY is still toplevel (scope.toplevel? = outside any
    // class/module) ⇒ an unresolved implicit-self call there FIRES. Oracle: 2:3.
    let src = b"def m\n  still_missing\nend\n";
    let d = unresolved(src);
    assert_eq!(d.len(), 1, "toplevel def body is toplevel, got {d:?}");
    assert_eq!(line_col(src, d[0].start_offset), (2, 3));
}

#[test]
fn unresolved_toplevel_inside_anonymous_meta_class_body_silent() {
    // Upstream #319 (pin v0.3.4): `Class.new do … end` evaluates its block as
    // the new class's own body, so `self` there is that class and the rule
    // cannot fire. Away from constant-write position the reference is silent
    // on all four meta forms. Oracle (v0.3.4): no diagnostics on any of these.
    for src in [
        b"k = Class.new do\n  attr_reader :a\nend\n".as_slice(),
        b"m = Module.new do\n  some_macro\nend\n".as_slice(),
        b"s = Struct.new(:a) do\n  some_macro\nend\n".as_slice(),
        b"d = Data.define(:a) do\n  some_macro\nend\n".as_slice(),
        b"Class.new do\n  attr_reader :a\nend\n".as_slice(),
        b"@k = Class.new do\n  attr_reader :a\nend\n".as_slice(),
        b"k = Class.new(StandardError) do\n  attr_reader :a\nend\n".as_slice(),
    ] {
        assert!(
            unresolved(src).is_empty(),
            "an anonymous meta-class body is a class body: {src:?}"
        );
    }
}

#[test]
fn unresolved_toplevel_silent_in_constant_assigned_meta_class_body() {
    // Upstream #590 / `b3d688f7` (pin `v0.3.8`) closed the reference's own
    // asymmetry: `StatementEvaluator` gained a `ConstantWriteNode` handler
    // that enters the rvalue block through the same `enter_meta_class_body`
    // the #319 arm uses, so `self` there is the created class and
    // `Scope#toplevel?` no longer holds. Oracle at `ffb456b0`: silent on each.
    for src in [
        b"A = Class.new do\n  attr_reader :a\nend\n".as_slice(),
        b"A = Class.new(StandardError) do\n  attr_reader :a\nend\n".as_slice(),
        b"A = Module.new do\n  attr_reader :a\nend\n".as_slice(),
        b"A = Struct.new(:a) do\n  attr_reader :b\nend\n".as_slice(),
        b"A = Data.define(:a) do\n  attr_reader :b\nend\n".as_slice(),
    ] {
        let d = unresolved(src);
        assert!(d.is_empty(), "constant-assigned body is a class body: {src:?} {d:?}");
    }
}

#[test]
fn unresolved_toplevel_fires_outside_a_constant_assigned_meta_class_body() {
    // The control an over-broad #590 port WOULD silence: widening the
    // suppression from "the block body" to "the whole constant write", or to
    // "any block body", takes these two with it. Only the BLOCK BODY is a
    // class scope — the ARGUMENTS keep the enclosing (toplevel) scope, and a
    // block on a call that is NOT a meta-new selector (#316's DSL block) is
    // not a class body at all. Oracle at `ffb456b0`: 1:15, and 4:1 + 5:3.
    let src = b"A = Class.new(parent_of_x) do\n  attr_reader :a\nend\n";
    let d = unresolved(src);
    assert_eq!(d.len(), 1, "constant-write args stay toplevel, got {d:?}");
    assert_eq!(line_col(src, d[0].start_offset), (1, 15));

    let src = b"A = Class.new do\n  attr_reader :a\nend\nsome_dsl_call do\n  attr_reader :d\nend\n";
    let d = unresolved(src);
    assert_eq!(d.len(), 2, "#316 DSL block is not a class body, got {d:?}");
    let mut at: Vec<(usize, usize)> = d.iter().map(|x| line_col(src, x.start_offset)).collect();
    at.sort_unstable();
    assert_eq!(at, vec![(4, 1), (5, 3)]);
}

#[test]
fn unresolved_toplevel_fires_outside_an_anonymous_meta_class_body() {
    // Only the BLOCK BODY is a class scope. A call in the ARGUMENTS, and a
    // call after the block, keep the enclosing (toplevel) scope. Oracle
    // (v0.3.4): 1:15 and 4:1.
    let src = b"k = Class.new(parent_of_x) do\n  attr_reader :a\nend\nstill_missing\n";
    let d = unresolved(src);
    assert_eq!(d.len(), 2, "args and trailing call stay toplevel, got {d:?}");
    assert_eq!(line_col(src, d[0].start_offset), (1, 15));
    assert_eq!(line_col(src, d[1].start_offset), (4, 1));
}

#[test]
fn unresolved_toplevel_silent_inside_receiver_eval_blocks() {
    // Upstream #1135 (`ee33407e`, pin `e59b7b89`): the reference declines any
    // receiverless call inside the literal block of a call NAMED
    // `class_eval` / `module_eval` / `class_exec` / `module_exec` /
    // `instance_eval` / `instance_exec`, whatever the receiver — constant,
    // local, or none. Oracle at `e59b7b89`: silent on each.
    for sel in [
        "class_eval",
        "module_eval",
        "class_exec",
        "module_exec",
        "instance_eval",
        "instance_exec",
    ] {
        for src in [
            format!("class Foo; end\nFoo.{sel} do\n  zzz_missing\nend\n"),
            format!("class Foo; end\nx = Foo\nx.{sel} do\n  zzz_missing\nend\n"),
            format!("class Foo; end\nFoo.{sel} {{ zzz_missing }}\n"),
            format!(
                "Minitest::Test.{sel} do\n  include ::RSpec::Matchers\n  def m\n    zzz_nested\n  end\nend\n"
            ),
        ] {
            let d = unresolved(src.as_bytes());
            assert!(d.is_empty(), "eval block body is a class body: {src:?} {d:?}");
        }
    }
}

#[test]
fn unresolved_toplevel_fires_around_receiver_eval_blocks() {
    // The must-still-fire controls: only the LITERAL block counts. The eval
    // call's arguments, a block-pass operand, a receiverless `class_eval`
    // itself, and a call after the block keep the toplevel scope. Oracle at
    // `e59b7b89`: 2:16, 4:17, 5:1, 8:1.
    let src = b"class Foo; end\nFoo.class_eval(zzz_arg) do\nend\nFoo.class_eval(&zzz_pass)\nclass_eval do\n  zzz_body\nend\nzzz_after\n";
    let d = unresolved(src);
    let mut at: Vec<(usize, usize)> = d.iter().map(|x| line_col(src, x.start_offset)).collect();
    at.sort_unstable();
    assert_eq!(at, vec![(2, 16), (4, 17), (5, 1), (8, 1)], "got {d:?}");
}

#[test]
fn catalog_entries_are_correct() {
    let entry = catalog(CALL_UNDEFINED_METHOD).expect("catalog entry must exist");
    assert_eq!(entry.default_severity, Severity::Error);
    assert_eq!(entry.evidence_tier, "high");
    assert!(entry.documentation_url.contains("call-undefined-method"));

    let entry = catalog(CALL_WRONG_ARITY).expect("catalog entry must exist");
    assert_eq!(entry.default_severity, Severity::Error);
    assert_eq!(entry.evidence_tier, "high");

    let entry = catalog(CALL_POSSIBLE_NIL_RECEIVER).expect("catalog entry must exist");
    // `error` under the default balanced profile (matches the oracle).
    assert_eq!(entry.default_severity, Severity::Error);
    assert_eq!(entry.evidence_tier, "medium");

    let entry = catalog(FLOW_DEAD_ASSIGNMENT).expect("catalog entry must exist");
    assert_eq!(entry.default_severity, Severity::Warning);
    assert_eq!(entry.evidence_tier, "medium");
    assert!(entry.documentation_url.contains("flow-dead-assignment"));

    let entry = catalog(FLOW_UNREACHABLE_BRANCH).expect("catalog entry must exist");
    assert_eq!(entry.default_severity, Severity::Warning);
    assert_eq!(entry.evidence_tier, "high");
    assert!(entry.documentation_url.contains("flow-unreachable-branch"));

    let entry = catalog(FLOW_ALWAYS_TRUTHY_CONDITION).expect("catalog entry must exist");
    assert_eq!(entry.default_severity, Severity::Warning);
    assert_eq!(entry.evidence_tier, "medium");
    assert!(entry.documentation_url.contains("flow-always-truthy-condition"));

    let entry = catalog(CALL_UNRESOLVED_TOPLEVEL).expect("catalog entry must exist");
    assert_eq!(entry.default_severity, Severity::Warning);
    assert_eq!(entry.evidence_tier, "low");
    assert!(entry.documentation_url.contains("call-unresolved-toplevel"));

    assert!(catalog("unknown.rule").is_none());
}

// -- in-source suppression --------------------------------------------

fn diag(rule: &'static str) -> Diagnostic {
    Diagnostic {
        rule_id: rule,
        start_offset: 0,
        end_offset: 0,
        message: String::new(),
        severity: Severity::Error,
        source_family: "builtin",
        receiver_type: None,
        method_name: None,
    }
}

fn surviving_rules(
    diags: Vec<(usize, Diagnostic)>,
    comments: &[(usize, usize, String)],
) -> Vec<(usize, &'static str)> {
    filter_suppressed(diags, comments)
        .into_iter()
        .map(|(line, d)| (line, d.rule_id))
        .collect()
}

#[test]
fn line_disable_drops_only_that_lines_rule() {
    let diags = vec![
        (2, diag(CALL_UNDEFINED_METHOD)),
        (4, diag(CALL_UNDEFINED_METHOD)),
    ];
    let comments = vec![(4, 0, "# rigor:disable call.undefined-method".to_string())];
    // Only the L4 diagnostic is suppressed; L2 survives.
    assert_eq!(surviving_rules(diags, &comments), vec![(2, CALL_UNDEFINED_METHOD)]);
}

#[test]
fn line_disable_all_drops_every_rule_on_that_line() {
    let diags = vec![
        (3, diag(CALL_UNDEFINED_METHOD)),
        (3, diag(CALL_WRONG_ARITY)),
        (5, diag(CALL_WRONG_ARITY)),
    ];
    let comments = vec![(3, 0, "# rigor:disable all".to_string())];
    assert_eq!(surviving_rules(diags, &comments), vec![(5, CALL_WRONG_ARITY)]);
}

#[test]
fn disable_file_drops_rule_on_every_line() {
    let diags = vec![
        (2, diag(CALL_UNDEFINED_METHOD)),
        (9, diag(CALL_UNDEFINED_METHOD)),
        (9, diag(CALL_WRONG_ARITY)),
    ];
    // The directive sits on line 1 but scopes the whole file.
    let comments = vec![(1, 0, "# rigor:disable-file undefined-method".to_string())];
    assert_eq!(surviving_rules(diags, &comments), vec![(9, CALL_WRONG_ARITY)]);
}

#[test]
fn disable_file_all_drops_everything() {
    let diags = vec![
        (2, diag(CALL_UNDEFINED_METHOD)),
        (4, diag(CALL_WRONG_ARITY)),
        (6, diag(CALL_POSSIBLE_NIL_RECEIVER)),
    ];
    let comments = vec![(1, 0, "# rigor:disable-file all".to_string())];
    assert!(filter_suppressed(diags, &comments).is_empty());
}

#[test]
fn family_token_call_expands_to_all_call_rules() {
    let diags = vec![
        (2, diag(CALL_UNDEFINED_METHOD)),
        (2, diag(CALL_WRONG_ARITY)),
        (2, diag(CALL_POSSIBLE_NIL_RECEIVER)),
        (2, diag(CALL_UNRESOLVED_TOPLEVEL)),
    ];
    let comments = vec![(2, 0, "# rigor:disable call".to_string())];
    assert!(filter_suppressed(diags, &comments).is_empty());
}

#[test]
fn legacy_alias_resolves_to_canonical_id() {
    let diags = vec![(4, diag(CALL_UNDEFINED_METHOD))];
    let comments = vec![(4, 0, "# rigor:disable undefined-method".to_string())];
    assert!(filter_suppressed(diags, &comments).is_empty());
}

#[test]
fn comma_and_whitespace_separated_tokens() {
    let diags = vec![
        (2, diag(CALL_UNDEFINED_METHOD)),
        (2, diag(CALL_WRONG_ARITY)),
    ];
    let comments = vec![(2, 0, "# rigor:disable undefined-method, wrong-arity".to_string())];
    assert!(filter_suppressed(diags, &comments).is_empty());
}

#[test]
fn unrelated_rule_or_line_is_not_suppressed() {
    // A disable for a DIFFERENT rule on the same line must not drop it.
    let same_line = filter_suppressed(
        vec![(4, diag(CALL_UNDEFINED_METHOD))],
        &[(4, 0, "# rigor:disable wrong-arity".to_string())],
    );
    assert_eq!(same_line.len(), 1);

    // A disable on a DIFFERENT line must not drop it.
    let other_line = filter_suppressed(
        vec![(4, diag(CALL_UNDEFINED_METHOD))],
        &[(7, 0, "# rigor:disable undefined-method".to_string())],
    );
    assert_eq!(other_line.len(), 1);
}

#[test]
fn disable_file_negative_lookahead_not_read_as_line_disable() {
    // `disable-file` must NOT also register as a line-level `disable` for the
    // comment's own line (reference `(?!-file)`).
    let line_set =
        parse_suppression_comments(&[(3, 0, "# rigor:disable-file undefined-method".to_string())]);
    assert!(!line_set.0.contains_key(&3));
    assert!(line_set.1.suppresses(CALL_UNDEFINED_METHOD));
}

#[test]
fn internal_error_is_never_suppressed() {
    let diags = vec![(2, diag(INTERNAL_ERROR_RULE))];
    let comments = vec![(2, 0, "# rigor:disable all".to_string())];
    // Even `disable all` cannot silence an internal-error diagnostic.
    assert_eq!(filter_suppressed(diags, &comments).len(), 1);
}

#[test]
fn suppress_set_from_tokens_legacy_alias() {
    // The public config helper expands the legacy alias to its canonical id.
    let set = SuppressSet::from_tokens(&["undefined-method"]);
    assert!(set.suppresses(CALL_UNDEFINED_METHOD));
    assert!(!set.suppresses(CALL_WRONG_ARITY));
}

#[test]
fn suppress_set_from_tokens_call_family_and_canonical() {
    // `call` family wildcard expands to every implemented call.* id.
    let set = SuppressSet::from_tokens(&["call"]);
    assert!(set.suppresses(CALL_UNDEFINED_METHOD));
    assert!(set.suppresses(CALL_WRONG_ARITY));
    assert!(set.suppresses(CALL_POSSIBLE_NIL_RECEIVER));
    assert!(set.suppresses(CALL_UNRESOLVED_TOPLEVEL)); // #250
    // A canonical id passes through to itself.
    let set = SuppressSet::from_tokens(&[CALL_WRONG_ARITY]);
    assert!(set.suppresses(CALL_WRONG_ARITY));
    assert!(!set.suppresses(CALL_UNDEFINED_METHOD));
}

#[test]
fn suppress_set_from_tokens_never_matches_internal_error() {
    // Neither `all` nor an explicit `internal-error` token may match the
    // internal-error sentinel — it stays reportable through config too.
    assert!(!SuppressSet::from_tokens(&["all"]).suppresses(INTERNAL_ERROR_RULE));
    assert!(!SuppressSet::from_tokens(&["internal-error"]).suppresses(INTERNAL_ERROR_RULE));
}

#[test]
fn suppress_set_from_tokens_empty_and_unknown_are_inert() {
    let empty: [&str; 0] = [];
    assert!(!SuppressSet::from_tokens(&empty).suppresses(CALL_UNDEFINED_METHOD));
    // An unknown token matches no real diagnostic.
    let set = SuppressSet::from_tokens(&["not-a-real-rule"]);
    assert!(!set.suppresses(CALL_UNDEFINED_METHOD));
    assert!(!set.suppresses(CALL_WRONG_ARITY));
}

#[test]
fn inert_builtin_token_flags_only_typos_under_a_builtin_family() {
    // A built-in-family id that names no real rule → inert (a likely typo).
    assert!(is_inert_builtin_token("call.undefiend-method"));
    assert!(is_inert_builtin_token("flow.dead-assingment"));
    assert!(is_inert_builtin_token("def.override-visibility"));
    // A known canonical id → NOT flagged (recognized).
    assert!(!is_inert_builtin_token("call.undefined-method"));
    assert!(!is_inert_builtin_token("flow.always-truthy-condition"));
    // Even a canonical id rigor-rs doesn't yet emit → NOT flagged (the audit
    // uses the reference's FULL catalogue, not IMPLEMENTED_RULES).
    assert!(!is_inert_builtin_token("def.return-type-mismatch"));
    assert!(!is_inert_builtin_token("call.argument-type-mismatch"));
    // A bare family wildcard → NOT flagged (a valid `disable: [call]`).
    assert!(!is_inert_builtin_token("call"));
    assert!(!is_inert_builtin_token("flow"));
    // A non-built-in family (plugin / legacy alias / arbitrary) → NOT flagged
    // (may resolve at run time; under-warning is FP-safe).
    assert!(!is_inert_builtin_token("undefined-method")); // legacy alias
    assert!(!is_inert_builtin_token("rails.something")); // plugin family
    assert!(!is_inert_builtin_token("all"));
    assert!(!is_inert_builtin_token("not-a-real-rule"));
}

// --- ADR-35 slice 1: def.override-visibility-reduced ----------------------

/// The override-visibility diagnostics in one source string (single-file).
fn override_vis(src: &[u8]) -> Vec<Diagnostic> {
    run(src)
        .into_iter()
        .filter(|d| d.rule_id == DEF_OVERRIDE_VISIBILITY_REDUCED)
        .collect()
}

/// Cross-file: analyze `files[focus]` against a PROJECT source built over all
/// `files`, returning only the override-visibility diagnostics.
fn override_vis_project(files: &[&[u8]], focus: usize) -> Vec<Diagnostic> {
    let asts: Vec<_> = files.iter().map(|s| lower(&parse(s))).collect();
    let refs: Vec<&LoweredAst> = asts.iter().collect();
    let index = CoreIndex::new();
    let source = rigor_infer::SourceIndex::build_project(&refs, &index);
    let mut interner = Interner::new();
    analyze_with_source(&asts[focus], &mut interner, &index, &source)
        .into_iter()
        .filter(|d| d.rule_id == DEF_OVERRIDE_VISIBILITY_REDUCED)
        .collect()
}

#[test]
fn override_vis_fires_public_to_private_across_superclass() {
    // The oracle fixture: B < A, A#foo public, B#foo private ⇒ fires.
    let src = b"class A\n  def foo; end\nend\nclass B < A\n  private\n  def foo; end\nend\n";
    let diags = override_vis(src);
    assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
    let d = &diags[0];
    assert_eq!(d.rule_id, DEF_OVERRIDE_VISIBILITY_REDUCED);
    assert_eq!(d.severity, Severity::Warning);
    assert_eq!(d.source_family, "builtin");
    assert_eq!(d.method_name.as_deref(), Some("foo"));
    assert_eq!(
        d.message,
        "visibility of `foo' reduced from public to private (overrides A#foo); breaks substitutability"
    );
    // Anchored on the overriding def's name token.
    assert_eq!(&src[d.start_offset..d.end_offset], b"foo");
}

#[test]
fn override_vis_fires_public_to_protected() {
    let src = b"class A\n  def foo; end\nend\nclass B < A\n  protected\n  def foo; end\nend\n";
    let diags = override_vis(src);
    assert_eq!(diags.len(), 1);
    assert_eq!(
        diags[0].message,
        "visibility of `foo' reduced from public to protected (overrides A#foo); breaks substitutability"
    );
}

#[test]
fn override_vis_silent_on_widening() {
    // private parent ⇒ public override is a WIDENING (not a reduction) ⇒ silent.
    let src = b"class A\n  private\n  def foo; end\nend\nclass B < A\n  def foo; end\nend\n";
    assert!(override_vis(src).is_empty());
    // protected ⇒ public widening too.
    let src2 = b"class A\n  protected\n  def foo; end\nend\nclass B < A\n  def foo; end\nend\n";
    assert!(override_vis(src2).is_empty());
}

#[test]
fn override_vis_silent_when_ancestor_is_rbs_or_unknown() {
    // `class B < ApplicationRecord` — the super is not a project source class
    // ⇒ no project ancestor defines the method ⇒ silent (RBS carve-out).
    let src = b"class B < ApplicationRecord\n  private\n  def foo; end\nend\n";
    assert!(override_vis(src).is_empty());
}

#[test]
fn override_vis_silent_when_no_ancestor_defines() {
    // B < A but A does not define `foo` ⇒ silent.
    let src = b"class A\n  def other; end\nend\nclass B < A\n  private\n  def foo; end\nend\n";
    assert!(override_vis(src).is_empty());
}

#[test]
fn override_vis_silent_on_singleton_def() {
    // `def self.foo` is a singleton method — never in the visibility table ⇒
    // silent even under a bare `private`.
    let src = b"class A\n  def foo; end\nend\nclass B < A\n  private\n  def self.foo; end\nend\n";
    assert!(override_vis(src).is_empty());
}

#[test]
fn override_vis_silent_on_private_def_form() {
    // `private def foo` records `foo` at the running default (Public),
    // mirroring the reference gap ⇒ Public-vs-Public is no reduction ⇒ silent.
    let src = b"class A\n  def foo; end\nend\nclass B < A\n  private def foo; end\nend\n";
    assert!(override_vis(src).is_empty());
}

#[test]
fn override_vis_fires_across_included_module() {
    // M#foo public (included into B); B#foo private ⇒ fires, overrides M#foo.
    let src = b"module M\n  def foo; end\nend\nclass B\n  include M\n  private\n  def foo; end\nend\n";
    let diags = override_vis(src);
    assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
    assert_eq!(
        diags[0].message,
        "visibility of `foo' reduced from public to private (overrides M#foo); breaks substitutability"
    );
}

#[test]
fn override_vis_fires_cross_file() {
    // Parent A in file 0, subclass B (private override) in file 1 — built via
    // `build_project`, the walk resolves A across files and fires.
    let a = b"class A\n  def foo; end\nend\n" as &[u8];
    let b = b"class B < A\n  private\n  def foo; end\nend\n" as &[u8];
    let diags = override_vis_project(&[a, b], 1);
    assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
    assert_eq!(
        diags[0].message,
        "visibility of `foo' reduced from public to private (overrides A#foo); breaks substitutability"
    );
}

/// Issue #92 invariant 1, example 1 — FILE ORDER IS NORMATIVE. The project
/// index records `method_visibilities` first-write-wins, so which file
/// declares `Base#m` FIRST decides whether the override fires. `a,b` sees
/// `m` public on Base ⇒ `Sub#m` private reduces it ⇒ warns; `b,a` sees it
/// already private ⇒ silent.
///
/// This is a property of the pre-existing accumulator, not of any refactor —
/// which is exactly why it is pinned here: `SourceIndex::merge` replays the
/// per-file harvests in the caller's file order and must never sort them.
#[test]
fn override_vis_project_order_is_normative() {
    let a = b"class Base\n  def m\n    1\n  end\nend\n" as &[u8];
    let b = b"class Base\n  private\n  def m\n    2\n  end\nend\n\nclass Sub < Base\n  private\n  def m\n    3\n  end\nend\n" as &[u8];
    let forward = override_vis_project(&[a, b], 1);
    assert_eq!(forward.len(), 1, "a,b must fire, got {forward:?}");
    assert_eq!(
        forward[0].message,
        "visibility of `m' reduced from public to private (overrides Base#m); breaks substitutability"
    );
    let reversed = override_vis_project(&[b, a], 0);
    assert!(reversed.is_empty(), "b,a must stay silent, got {reversed:?}");
}

/// Issue #92 invariant 1, example 2 — the same for `include` ACCUMULATION
/// order, on a fully idiomatic shape (one class reopened in two files, each
/// adding an include). `override_ancestor_names` walks includes in
/// accumulated order, so the MRO's nearest defining ancestor flips with the
/// file order: M1 first ⇒ a public definer ⇒ fires; M2 first ⇒ a private
/// definer ⇒ silent.
#[test]
fn override_vis_project_include_order_is_normative() {
    let mods =
        b"module M1\n  def m; 1; end\nend\nmodule M2\n  private\n  def m; 2; end\nend\n"
            as &[u8];
    let a = b"class Foo\n  include M1\nend\n" as &[u8];
    let b = b"class Foo\n  include M2\n  private\n  def m; 3; end\nend\n" as &[u8];
    let forward = override_vis_project(&[mods, a, b], 2);
    assert_eq!(forward.len(), 1, "mods,a,b must fire, got {forward:?}");
    assert_eq!(
        forward[0].message,
        "visibility of `m' reduced from public to private (overrides M1#m); breaks substitutability"
    );
    let reversed = override_vis_project(&[mods, b, a], 1);
    assert!(reversed.is_empty(), "mods,b,a must stay silent, got {reversed:?}");
}

#[test]
fn override_vis_catalog_entry_matches_oracle() {
    let e = catalog(DEF_OVERRIDE_VISIBILITY_REDUCED).expect("catalog entry must exist");
    assert_eq!(e.default_severity, Severity::Warning);
    assert_eq!(e.evidence_tier, "high");
    assert_eq!(
        e.documentation_url,
        "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-def-override-visibility-reduced"
    );
}

// --- flow.always-raises (Integer ÷/% by constant-zero divisor) -----------

/// Diagnostics filtered to just the always-raises rule.
fn always_raises_diags(src: &[u8]) -> Vec<Diagnostic> {
    run(src)
        .into_iter()
        .filter(|d| d.rule_id == FLOW_ALWAYS_RAISES)
        .collect()
}

#[test]
fn always_raises_fires_on_literal_int_div_zero() {
    // Byte-exact with the oracle: `5 / 0` ⇒ error, message anchored on `/`.
    let src = b"5 / 0\n";
    let diags = always_raises_diags(src);
    assert_eq!(diags.len(), 1, "expected one diag, got {diags:?}");
    let d = &diags[0];
    assert_eq!(d.rule_id, FLOW_ALWAYS_RAISES);
    assert_eq!(d.severity, Severity::Error);
    assert_eq!(d.source_family, "builtin");
    assert_eq!(
        d.message,
        "always raises ZeroDivisionError: `/' by zero on Integer receiver"
    );
    // Span anchors on the operator token (the message loc), matching the
    // oracle's column.
    assert_eq!(&src[d.start_offset..d.end_offset], b"/");
}

#[test]
fn always_raises_fires_on_modulo_zero() {
    let src = b"10 % 0\n";
    let diags = always_raises_diags(src);
    assert_eq!(diags.len(), 1, "expected one diag, got {diags:?}");
    assert_eq!(
        diags[0].message,
        "always raises ZeroDivisionError: `%' by zero on Integer receiver"
    );
}

#[test]
fn always_raises_fires_through_local_binding() {
    // `x = 5; x / 0` — the receiver folds to `Constant[Int(5)]` (Integer-
    // rooted), the divisor is constant zero ⇒ fire (oracle parity).
    let src = b"x = 5\nx / 0\n";
    let diags = always_raises_diags(src);
    assert_eq!(diags.len(), 1, "expected one diag, got {diags:?}");
    assert_eq!(
        diags[0].message,
        "always raises ZeroDivisionError: `/' by zero on Integer receiver"
    );
}

#[test]
fn always_raises_fires_on_named_ops() {
    // `div`, `modulo`, `divmod` are in the reference's op set.
    for (src, op) in [
        (b"7.div(0)\n" as &[u8], "div"),
        (b"8.modulo(0)\n", "modulo"),
        (b"9.divmod(0)\n", "divmod"),
    ] {
        let diags = always_raises_diags(src);
        assert_eq!(diags.len(), 1, "expected one diag for {op}, got {diags:?}");
        assert_eq!(
            diags[0].message,
            format!("always raises ZeroDivisionError: `{op}' by zero on Integer receiver")
        );
    }
}

#[test]
fn always_raises_silent_on_nonzero_divisor() {
    // `5 / 2` — a valid division, never raises ⇒ silent.
    assert!(always_raises_diags(b"5 / 2\n").is_empty());
}

#[test]
fn always_raises_silent_on_float_divisor() {
    // `5 / 0.0` — Float division by zero is `Infinity`, NOT an error. The
    // oracle is silent; rigor-rs must be too (the divisor is not a constant
    // Integer zero).
    assert!(always_raises_diags(b"5 / 0.0\n").is_empty());
}

#[test]
fn always_raises_silent_on_float_receiver() {
    // `5.0 / 0` — Float receiver ⇒ Float division ⇒ `Infinity`, not an error.
    // The oracle declines (receiver not Integer-rooted); rigor-rs must too.
    assert!(always_raises_diags(b"5.0 / 0\n").is_empty());
}

#[test]
fn always_raises_silent_on_nonconstant_divisor() {
    // `x / y` with `y` non-constant ⇒ the divisor is not a constant zero ⇒
    // decline (never guess on a dynamic divisor).
    assert!(always_raises_diags(b"x = 5\nx / y\n").is_empty());
}

#[test]
fn always_raises_silent_on_dynamic_receiver() {
    // `x / 0` where `x` is never bound ⇒ Dynamic receiver, not Integer-rooted
    // ⇒ decline (zero-FP keystone).
    assert!(always_raises_diags(b"x / 0\n").is_empty());
}

#[test]
fn always_raises_silent_on_block_call() {
    // A block changes dispatch ⇒ decline. `5.div(0) { }` is contrived but
    // exercises the block gate.
    assert!(always_raises_diags(b"5.div(0) { 1 }\n").is_empty());
}

#[test]
fn always_raises_catalog_entry_matches_oracle() {
    let e = catalog(FLOW_ALWAYS_RAISES).expect("catalog entry must exist");
    assert_eq!(e.default_severity, Severity::Error);
    assert_eq!(e.evidence_tier, "high");
    assert_eq!(
        e.documentation_url,
        "https://rigor.typedduck.fail/manual/04-diagnostics/#rule-flow-always-raises"
    );
}

// --- ADR-0033: project `sig/`-declared class instance witnessing ----------

/// Analyze `src` against a CoreIndex built WITH a project `sig/` dir holding
/// `class Widget; def spin: () -> Integer; end`. Uses a real temp dir (sig
/// ingestion is filesystem-driven). Returns undefined-method diagnostics.
fn run_with_widget_sig(label: &str, src: &[u8]) -> Vec<Diagnostic> {
    // `label` makes the dir unique per test — tests run in parallel threads
    // sharing one process id, so a shared path would let one test's cleanup
    // wipe another's sig file mid-run.
    let dir = std::env::temp_dir()
        .join(format!("rigor-rules-sig-{}-{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp sig dir");
    std::fs::write(dir.join("widget.rbs"), "class Widget\n  def spin: () -> Integer\nend\n")
        .expect("write sig");
    let index = CoreIndex::for_project(&[], std::slice::from_ref(&dir));
    let ast = lower(&parse(src));
    let refs = [&ast];
    let source = rigor_infer::SourceIndex::build_project(&refs, &index);
    let mut interner = Interner::new();
    let diags = analyze_with_source(&ast, &mut interner, &index, &source)
        .into_iter()
        .filter(|d| d.rule_id == CALL_UNDEFINED_METHOD)
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    diags
}

#[test]
fn project_sig_new_instance_typo_is_witnessed() {
    // `Widget.new.spni` — Widget is declared in project sig/, so the reference
    // (and now rigor-rs) witnesses the typo on the `.new` instance.
    let diags = run_with_widget_sig("typo", b"Widget.new.spni\n");
    assert_eq!(diags.len(), 1, "expected witness, got {diags:?}");
    assert_eq!(diags[0].receiver_type.as_deref(), Some("Widget"));
    assert_eq!(diags[0].method_name.as_deref(), Some("spni"));
    assert_eq!(diags[0].severity, Severity::Error);
}

#[test]
fn project_sig_new_instance_valid_method_is_silent() {
    // `spin` IS declared ⇒ no diagnostic (the sig is authoritative both ways).
    assert!(run_with_widget_sig("valid", b"Widget.new.spin\n").is_empty());
}

#[test]
fn project_sig_new_instance_through_variable_is_witnessed() {
    // The instance type survives the local binding (`w = Widget.new; w.spni`).
    let diags = run_with_widget_sig("var", b"w = Widget.new\nw.spin\nw.spni\n");
    assert_eq!(diags.len(), 1, "expected one witness, got {diags:?}");
    assert_eq!(diags[0].receiver_type.as_deref(), Some("Widget"));
}

#[test]
fn bundled_stdlib_new_instance_stays_lenient_with_sig_loaded() {
    // Provenance gate: even with a project sig/ present, a bundled stdlib
    // class (`Pathname`, NOT project-sig) keeps the reference's `.new`
    // leniency — its typo must NOT be witnessed.
    assert!(run_with_widget_sig("pathname", b"Pathname.new(\"a\").spni\n").is_empty());
}

// -----------------------------------------------------------------------
// flow.duplicate-hash-key (v0.3.0)
// -----------------------------------------------------------------------

/// The diagnostics of one rule, in emit order.
fn of_rule(src: &[u8], rule: &str) -> Vec<Diagnostic> {
    run(src).into_iter().filter(|d| d.rule_id == rule).collect()
}

#[test]
fn dup_hash_key_symbol_shorthand_fires_once() {
    let d = of_rule(b"h = { a: 1, a: 2 }\n", FLOW_DUPLICATE_HASH_KEY);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].severity, Severity::Warning);
    assert_eq!(d[0].source_family, "builtin");
    assert_eq!(
        d[0].message,
        "duplicate hash key `:a' in the same literal; this entry overwrites the value first set at line 1"
    );
    // Anchored at the REPEAT key (`a` on the second entry, col 13 in the oracle).
    assert_eq!(d[0].start_offset, 12);
}

#[test]
fn dup_hash_key_string_uses_ruby_inspect_label() {
    let d = of_rule(b"h = { \"a\" => 1, \"a\" => 2 }\n", FLOW_DUPLICATE_HASH_KEY);
    assert_eq!(d.len(), 1);
    assert!(d[0].message.starts_with("duplicate hash key `\"a\"' "), "{}", d[0].message);
}

#[test]
fn dup_hash_key_integer_and_float_are_distinct_kinds() {
    // `1` and `1.0` are different keys (`1.eql?(1.0)` is false) ⇒ SILENT.
    assert!(of_rule(b"h = { 1 => 'x', 1.0 => 'y' }\n", FLOW_DUPLICATE_HASH_KEY).is_empty());
    // Same integer fires.
    assert_eq!(of_rule(b"h = { 1 => 'x', 1 => 'y' }\n", FLOW_DUPLICATE_HASH_KEY).len(), 1);
    // Same float fires.
    assert_eq!(of_rule(b"h = { 1.0 => 'x', 1.0 => 'y' }\n", FLOW_DUPLICATE_HASH_KEY).len(), 1);
}

#[test]
fn dup_hash_key_float_label_is_verbatim_slice() {
    // `1.0` and `1.00` are the same f64 ⇒ collide; the label is the VERBATIM
    // slice of the repeat (`1.00`), not a re-rendered value.
    let d = of_rule(b"h = { 1.0 => 'x', 1.00 => 'y' }\n", FLOW_DUPLICATE_HASH_KEY);
    assert_eq!(d.len(), 1);
    assert!(d[0].message.starts_with("duplicate hash key `1.00' "), "{}", d[0].message);
}

#[test]
fn dup_hash_key_string_vs_symbol_never_collide() {
    assert!(of_rule(b"h = { \"a\" => 1, a: 2 }\n", FLOW_DUPLICATE_HASH_KEY).is_empty());
}

#[test]
fn dup_hash_key_computed_and_interpolated_keys_silent() {
    assert!(of_rule(b"h = { foo => 1, foo => 2 }\n", FLOW_DUPLICATE_HASH_KEY).is_empty());
    assert!(of_rule(b"h = { \"#{x}\" => 1, \"#{x}\" => 2 }\n", FLOW_DUPLICATE_HASH_KEY).is_empty());
}

#[test]
fn dup_hash_key_splat_is_inert_pair_still_fires() {
    let d = of_rule(b"h = { **other, a: 1, a: 2 }\n", FLOW_DUPLICATE_HASH_KEY);
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn dup_hash_key_true_false_nil() {
    assert_eq!(of_rule(b"h = { nil => 1, nil => 2 }\n", FLOW_DUPLICATE_HASH_KEY).len(), 1);
    assert_eq!(of_rule(b"h = { true => 1, true => 2 }\n", FLOW_DUPLICATE_HASH_KEY).len(), 1);
}

#[test]
fn dup_hash_key_bare_keyword_args_fire() {
    let d = of_rule(b"def m(**o); end\nm(a: 1, a: 2)\n", FLOW_DUPLICATE_HASH_KEY);
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn dup_hash_key_nested_literal_is_own_scope() {
    // Only the NESTED `a:` pair fires; the outer `a:`/`b:` never cross-compare.
    let d = of_rule(b"h = { a: 1, b: { a: 2, a: 3 } }\n", FLOW_DUPLICATE_HASH_KEY);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("first set at line 1"), "{}", d[0].message);
}

#[test]
fn dup_hash_key_triple_all_reference_original() {
    // `{ a: 1, a: 2, a: 3 }` fires TWICE, both naming the ORIGINAL first line.
    let d = of_rule(b"h = { a: 1, a: 2, a: 3 }\n", FLOW_DUPLICATE_HASH_KEY);
    assert_eq!(d.len(), 2, "{d:?}");
    assert!(d.iter().all(|x| x.message.contains("first set at line 1")));
}

// -----------------------------------------------------------------------
// flow.return-in-ensure (v0.3.0)
// -----------------------------------------------------------------------

fn ret(src: &[u8]) -> Vec<Diagnostic> {
    of_rule(src, FLOW_RETURN_IN_ENSURE)
}

#[test]
fn return_in_ensure_fires_with_static_message() {
    let d = ret(b"def m\n  work\nensure\n  return 1\nend\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].severity, Severity::Warning);
    assert_eq!(
        d[0].message,
        "`return' inside `ensure' discards the method's in-flight return value and swallows any in-flight exception"
    );
}

#[test]
fn return_in_ensure_plain_block_fires_lambda_and_define_method_are_barriers() {
    assert_eq!(ret(b"def m\n  work\nensure\n  [1].each { return }\nend\n").len(), 1);
    assert!(ret(b"def m\n  work\nensure\n  lambda { return 1 }\nend\n").is_empty());
    assert!(ret(b"def m\n  work\nensure\n  -> { return 1 }\nend\n").is_empty());
    assert!(ret(b"def m\n  work\nensure\n  define_method(:f) { return 1 }\nend\n").is_empty());
    assert!(ret(b"def m\n  work\nensure\n  def nested; return 1; end\nend\n").is_empty());
}

#[test]
fn return_in_ensure_proc_block_is_not_a_barrier() {
    assert_eq!(ret(b"def m\n  work\nensure\n  proc { return 1 }\nend\n").len(), 1);
}

#[test]
fn return_in_ensure_two_returns_fire_twice() {
    assert_eq!(ret(b"def m\n  work\nensure\n  return 1\n  return 2\nend\n").len(), 2);
}

#[test]
fn return_in_ensure_toplevel_begin() {
    assert_eq!(ret(b"begin\n  work\nensure\n  return\nend\n").len(), 1);
}

#[test]
fn return_in_ensure_nested_begin_fires_once() {
    // The inner `return 3` is collected exactly once (when the inner
    // BeginRescue is dispatched), NOT double-counted by the outer walk.
    let d = ret(b"def outer\n  work\nensure\n  begin\n    inner\n  ensure\n    return 3\n  end\nend\n");
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn return_in_ensure_no_return_is_silent() {
    assert!(ret(b"def m\n  work\nensure\n  cleanup\nend\n").is_empty());
}

#[test]
fn return_in_ensure_descends_through_a_multi_write() {
    // PR #46 review nit: `node_children` needs a `MultiWrite` arm, or the
    // descent stops one hop short of the `Node::Return` the multi-write
    // lowering now puts in the arena. Oracle-verified: the reference fires
    // at 4:19 on this exact source (rigor-rs was silent before the arm —
    // not a regression, since master could not lower the return at all).
    let d = ret(b"def m(flag)\n  do_work\nensure\n  a, b = (flag ? (return 1) : 2), 3\n  [a, b]\nend\n");
    assert_eq!(d.len(), 1, "{d:?}");
    // The `target_exprs` half of the arm is correct descent but currently
    // unreachable for THIS rule: a `return` embedded in a non-local target
    // (`obj[flag ? (return 1) : 2], b = 3, 4`) never enters the arena,
    // because `collect_recoverable_children` recovers reads / writes /
    // calls and NOT `ReturnNode`. That gap is orthogonal to multi-writes
    // and pre-existing (silent on old AND new; the oracle fires at 4:15).
    // Pinned here so a later `ReturnNode` recovery flips it visibly.
    let d = ret(b"def m(flag)\n  do_work\nensure\n  obj[flag ? (return 1) : 2], b = 3, 4\nend\n");
    assert!(d.is_empty(), "known gap: ReturnNode is not a recoverable child; got {d:?}");
}

// -----------------------------------------------------------------------
// suppression.unknown-rule / suppression.empty (v0.3.0)
// -----------------------------------------------------------------------

/// Run the suppression surveillance over a single comment.
fn sup(comment: &str) -> Vec<Diagnostic> {
    suppression_marker_diagnostics(&[(1, 0, comment.to_string())])
}

#[test]
fn suppression_unknown_rule_fires_with_exact_message() {
    let d = sup("# rigor:disable call.no-such-rule");
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].rule_id, SUPPRESSION_UNKNOWN_RULE);
    assert_eq!(d[0].severity, Severity::Warning);
    assert_eq!(
        d[0].message,
        "unknown rule `call.no-such-rule` in `# rigor:disable` — the token matches no known rule, alias, or family, so this suppression has no effect. Likely a typo; `rigor explain <rule>` lists the canonical ids."
    );
}

#[test]
fn suppression_empty_bare_marker_fires() {
    let line = sup("# rigor:disable");
    assert_eq!(line.len(), 1);
    assert_eq!(line[0].rule_id, SUPPRESSION_EMPTY);
    assert_eq!(
        line[0].message,
        "`# rigor:disable` lists no rules, so this suppression has no effect. Name the rules to suppress (`# rigor:disable call.undefined-method`) or use `# rigor:disable all`."
    );
    let file = sup("# rigor:disable-file");
    assert_eq!(file.len(), 1);
    assert_eq!(file[0].rule_id, SUPPRESSION_EMPTY);
    assert!(file[0].message.contains("`# rigor:disable-file`"));
}

#[test]
fn suppression_multiple_unknown_tokens_share_anchor() {
    let d = sup("# rigor:disable call.undefined-method,call.bogus-one, call.bogus-two");
    assert_eq!(d.len(), 2, "{d:?}");
    assert!(d.iter().all(|x| x.rule_id == SUPPRESSION_UNKNOWN_RULE));
    assert!(d.iter().all(|x| x.start_offset == d[0].start_offset));
}

#[test]
fn suppression_known_tokens_stay_silent() {
    assert!(sup("# rigor:disable call").is_empty()); // family
    assert!(sup("# rigor:disable all").is_empty()); // wildcard
    assert!(sup("# rigor:disable undefined-method").is_empty()); // legacy alias
    assert!(sup("# rigor:disable rbs_extended.something").is_empty()); // non-check family
    assert!(sup("# rigor:disable flow.duplicate-hash-key").is_empty()); // new canonical id
    assert!(sup("# rigor:disable flow.shadowed-rescue-clause").is_empty()); // known-but-unimplemented
    assert!(sup("# rigor:disable suppression.unknown-rule").is_empty()); // self
    // #252: the reference's `effect` family and ids, `plugin_trust`, and the
    // sixth bare runtime id are known vocabulary too.
    assert!(sup("# rigor:disable effect").is_empty());
    assert!(sup("# rigor:disable effect.envelope-exceeded").is_empty());
    assert!(sup("# rigor:disable effect.annotations-unchecked").is_empty());
    assert!(sup("# rigor:disable plugin_trust.foo").is_empty());
    assert!(sup("# rigor:disable source-rbs-annotation-not-honoured").is_empty());
}

#[test]
fn suppression_prose_is_ignored() {
    // A recognised marker word inside documentation prose (non-token text
    // follows) stays an ordinary comment — neither pattern fires.
    assert!(sup("# this documents `# rigor:disable <rule>` usage").is_empty());
    assert!(sup("# see `rigor:disable-next-line` for the RuboCop reflex").is_empty());
    assert!(sup("# the `rigor:enable` spelling is not supported").is_empty());
}

#[test]
fn suppression_unknown_marker_fires_with_exact_message() {
    // The RuboCop reflex: hyphenated `disable-next-line` is invisible to the
    // real suppression grammar, so surveillance flags it (upstream 4e0ca475).
    let d = sup("# rigor:disable-next-line call.undefined-method");
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].rule_id, SUPPRESSION_UNKNOWN_MARKER);
    assert_eq!(d[0].severity, Severity::Warning);
    assert_eq!(
        d[0].message,
        "unrecognised suppression marker `rigor:disable-next-line` — Rigor's markers are `# rigor:disable <rules>` (suppresses on its own line) and `# rigor:disable-file <rules>`, so this comment suppresses nothing."
    );
}

#[test]
fn suppression_unknown_marker_covers_enable_and_bare_forms() {
    // `enable` with or without a suffix, and a bare next-line marker.
    assert_eq!(sup("# rigor:enable")[0].rule_id, SUPPRESSION_UNKNOWN_MARKER);
    assert_eq!(sup("# rigor:enable call.undefined-method")[0].rule_id, SUPPRESSION_UNKNOWN_MARKER);
    assert_eq!(sup("# rigor:enable-all")[0].rule_id, SUPPRESSION_UNKNOWN_MARKER);
    assert_eq!(sup("# rigor:disable-next-line")[0].rule_id, SUPPRESSION_UNKNOWN_MARKER);
    // `disable-file` with a further suffix is out-of-grammar too.
    assert_eq!(sup("# rigor:disable-files")[0].rule_id, SUPPRESSION_UNKNOWN_MARKER);
}

#[test]
fn suppression_unknown_marker_declines_real_and_nonmarker_forms() {
    // The two recognised bare markers are handled by empty/unknown-rule, never
    // the unknown-MARKER pass.
    assert!(sup("# rigor:disable-file").iter().all(|d| d.rule_id != SUPPRESSION_UNKNOWN_MARKER));
    assert!(sup("# rigor:disable").iter().all(|d| d.rule_id != SUPPRESSION_UNKNOWN_MARKER));
    // Not a marker word at all — no diagnostic.
    assert!(sup("# rigor:disablexyz").is_empty());
    assert!(sup("# rigor:enablexyz").is_empty());
    // `enable-` with nothing after the hyphen is not a marker.
    assert!(sup("# rigor:enable-").is_empty());
}

#[test]
fn suppression_self_suppression_via_filter() {
    // The surveillance diagnostic flows through filter_suppressed and is
    // suppressed by its own line when acknowledged alongside the bogus token.
    let comment = "# rigor:disable call.bogus suppression.unknown-rule";
    let diags = suppression_marker_diagnostics(&[(1, 0, comment.to_string())]);
    let with_lines: Vec<(usize, Diagnostic)> = diags.into_iter().map(|d| (1, d)).collect();
    let kept = filter_suppressed(with_lines, &[(1, 0, comment.to_string())]);
    assert!(kept.is_empty(), "self-suppression must silence the complaint: {kept:?}");
}

// -----------------------------------------------------------------------
// call.raise-non-exception (v0.3.0)
// -----------------------------------------------------------------------

/// The `call.raise-non-exception` diagnostics for `src`, in source order.
fn raise_diags(src: &[u8]) -> Vec<Diagnostic> {
    run(src)
        .into_iter()
        .filter(|d| d.rule_id == CALL_RAISE_NON_EXCEPTION)
        .collect()
}

/// The single rendered operand of a lone firing (`<type>` in the message).
fn one_raise_operand(src: &[u8]) -> String {
    let diags = raise_diags(src);
    assert_eq!(diags.len(), 1, "expected exactly one firing, got {diags:?}");
    let m = &diags[0].message;
    let start = m.find("operand types as ").unwrap() + "operand types as ".len();
    let end = m.find(", which is not").unwrap();
    m[start..end].to_string()
}

#[test]
fn raise_fires_on_scalar_operands() {
    // Skip when the real Exception/String RBS is unavailable (stub fallback).
    if !CoreIndex::new().knows_class("Exception") {
        return;
    }
    assert_eq!(one_raise_operand(b"raise 42\n"), "42");
    assert_eq!(one_raise_operand(b"raise :sym\n"), ":sym");
    assert_eq!(one_raise_operand(b"raise nil\n"), "nil");
    assert_eq!(one_raise_operand(b"fail 3.14\n"), "3.14");
    // The message names the method verbatim.
    assert_eq!(raise_diags(b"fail 3.14\n")[0].method_name.as_deref(), Some("fail"));
    assert_eq!(raise_diags(b"raise 42\n")[0].method_name.as_deref(), Some("raise"));
    assert_eq!(raise_diags(b"raise 42\n")[0].severity, Severity::Error);
}

#[test]
fn raise_full_message_is_byte_exact() {
    if !CoreIndex::new().knows_class("Exception") {
        return;
    }
    let d = &raise_diags(b"raise 42\n")[0];
    assert_eq!(
        d.message,
        "`raise' operand types as 42, which is not an Exception class, \
         an Exception instance, a String, or an object defining `#exception' \u{2014} \
         this raises TypeError at runtime"
    );
    // Anchor is the `raise` keyword token.
    assert_eq!(&b"raise 42\n"[d.start_offset..d.end_offset], b"raise");
    // No receiver_type for this rule.
    assert!(d.receiver_type.is_none());
}

#[test]
fn raise_singleton_class_operands_fire_including_module_and_generic_carriers() {
    if !CoreIndex::new().knows_class("Exception") {
        return;
    }
    // A bare class object disjoint from Exception fires with `singleton(X)`.
    assert_eq!(one_raise_operand(b"raise Array\n"), "singleton(Array)");
    assert_eq!(one_raise_operand(b"raise Struct\n"), "singleton(Struct)");
    // The singleton path applies NO module / generic-carrier exclusion —
    // `raise Comparable` / `Class` / `Object` / `Module` / `BasicObject` fire.
    assert_eq!(one_raise_operand(b"raise Comparable\n"), "singleton(Comparable)");
    assert_eq!(one_raise_operand(b"raise Class\n"), "singleton(Class)");
    assert_eq!(one_raise_operand(b"raise Object\n"), "singleton(Object)");
    assert_eq!(one_raise_operand(b"raise Module\n"), "singleton(Module)");
    assert_eq!(one_raise_operand(b"raise BasicObject\n"), "singleton(BasicObject)");
    assert_eq!(one_raise_operand(b"raise Integer\n"), "singleton(Integer)");
}

#[test]
fn raise_instance_and_hash_operands_fire() {
    if !CoreIndex::new().knows_class("Exception") {
        return;
    }
    // A `Time.new` instance → `Time`.
    assert_eq!(one_raise_operand(b"raise Time.new\n"), "Time");
    // A positional (braced) hash literal → value-pinned `{ a: 1 }`.
    assert_eq!(one_raise_operand(b"raise({a: 1})\n"), "{ a: 1 }");
}

#[test]
fn raise_fires_inside_method_and_class_bodies() {
    if !CoreIndex::new().knows_class("Exception") {
        return;
    }
    // NOT toplevel-restricted.
    assert_eq!(raise_diags(b"def foo\n  raise 42\nend\n").len(), 1);
    assert_eq!(
        raise_diags(b"class W\n  def go\n    raise 7\n  end\nend\n").len(),
        1
    );
}

#[test]
fn raise_fires_on_third_positional_arg_form() {
    if !CoreIndex::new().knows_class("Exception") {
        return;
    }
    // Only the first positional argument is checked.
    assert_eq!(one_raise_operand(b"raise 42, \"msg\", caller\n"), "42");
}

#[test]
fn raise_stays_silent_on_legal_operands() {
    if !CoreIndex::new().knows_class("Exception") {
        return;
    }
    // Exception classes / instances / String are legal.
    assert!(raise_diags(b"raise StandardError\n").is_empty());
    assert!(raise_diags(b"raise RuntimeError\n").is_empty());
    assert!(raise_diags(b"raise KeyError\n").is_empty());
    assert!(raise_diags(b"raise StandardError, \"m\"\n").is_empty());
    assert!(raise_diags(b"raise ArgumentError.new\n").is_empty());
    assert!(raise_diags(b"raise \"plain message\"\n").is_empty());
    assert!(raise_diags(b"raise \"interp #{1}\"\n").is_empty());
}

#[test]
fn raise_stays_silent_on_envelope_bail_cases() {
    if !CoreIndex::new().knows_class("Exception") {
        return;
    }
    // Bare raise, explicit receiver, splat / bare-kwargs first arg.
    assert!(raise_diags(b"raise\n").is_empty());
    assert!(raise_diags(b"obj.raise(42)\n").is_empty());
    assert!(raise_diags(b"raise *some_ary\n").is_empty());
    assert!(raise_diags(b"raise(a: 1)\n").is_empty(), "bare keyword-hash bails");
    // Unresolved constant / dynamic operand.
    assert!(raise_diags(b"raise NotAThing\n").is_empty());
    assert!(raise_diags(b"raise err\n").is_empty());
    assert!(raise_diags(b"raise self.class\n").is_empty());
    // Qualified constant (unresolved in the source subset).
    assert!(raise_diags(b"raise Foo::Bar\n").is_empty());
}

#[test]
fn raise_stays_silent_on_project_classes_both_paths() {
    if !CoreIndex::new().knows_class("Exception") {
        return;
    }
    // A project class — even one whose written superclass is StandardError —
    // bails on BOTH the singleton and the instance path (the project gate).
    let src = b"class CustomError < StandardError; end\nraise CustomError\nraise CustomError.new\n";
    assert!(raise_diags(src).is_empty(), "{:?}", raise_diags(src));
}

#[test]
fn raise_stays_silent_when_redefined() {
    if !CoreIndex::new().knows_class("Exception") {
        return;
    }
    // Toplevel def.
    assert!(raise_diags(b"def raise(x); end\nraise 42\n").is_empty());
    // Object reopen.
    assert!(raise_diags(b"class Object\n  def raise(x); end\nend\nraise 42\n").is_empty());
    // Enclosing-class instance def.
    assert!(
        raise_diags(b"class Foo\n  def raise(x); end\n  def go\n    raise 99\n  end\nend\n")
            .is_empty()
    );
    // Enclosing-class singleton def (`def self.raise`).
    assert!(
        raise_diags(b"class Bar\n  def self.raise(x); end\n  def go\n    raise 99\n  end\nend\n")
            .is_empty()
    );
}

#[test]
fn raise_union_fires_only_when_every_arm_illegal() {
    // Constructed directly on the verdict function (rigor-rs types ternaries
    // Dynamic, so a source-level union operand does not arise through
    // inference — the verdict logic is what must be exact).
    let index = CoreIndex::new();
    if !index.knows_class("Exception") {
        return;
    }
    let source = rigor_infer::SourceIndex::build(&lower(&parse(b"\n")), &index);
    let mut i = Interner::new();
    let int = i.int(42);
    let sym = i.intern(Type::Constant(Scalar::Sym("s".into())));
    let string = i.intern(Type::Constant(Scalar::Str("x".into())));
    let all_illegal = rigor_types::Algebra::join(&mut i, int, sym);
    assert_eq!(
        raise_operand_verdict(&i, &index, &source, all_illegal),
        RaiseVerdict::Illegal
    );
    let mixed = rigor_types::Algebra::join(&mut i, int, string);
    assert_eq!(
        raise_operand_verdict(&i, &index, &source, mixed),
        RaiseVerdict::Unknown
    );
}

// -- def.ivar-write-mismatch ------------------------------------------

fn ivar_diags(src: &[u8]) -> Vec<Diagnostic> {
    run(src)
        .into_iter()
        .filter(|d| d.rule_id == DEF_IVAR_WRITE_MISMATCH)
        .collect()
}

#[test]
fn ivar_mismatch_string_then_integer_fires() {
    let src = b"class Foo\n  def m\n    @x = \"s\"\n    @x = 42\n  end\nend\n";
    let d = ivar_diags(src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].rule_id, DEF_IVAR_WRITE_MISMATCH);
    assert_eq!(d[0].severity, Severity::Warning);
    assert_eq!(
        d[0].message,
        "instance variable `@x' on Foo was previously assigned String; this write assigns Integer"
    );
    // Anchored on the `@x` name token of the OFFENDING (second) write.
    assert_eq!(&src[d[0].start_offset..d[0].end_offset], b"@x");
    assert_eq!(d[0].start_offset, src.windows(2).enumerate().filter(|(_, w)| *w == b"@x").nth(1).unwrap().0);
}

#[test]
fn ivar_bool_flag_idiom_silent() {
    // false then true — both fold to "bool", so no mismatch.
    let src = b"class Foo\n  def m\n    @on = false\n    @on = true\n  end\nend\n";
    assert!(ivar_diags(src).is_empty());
}

#[test]
fn ivar_bool_then_string_fires() {
    let src = b"class Foo\n  def m\n    @x = true\n    @x = \"s\"\n  end\nend\n";
    let d = ivar_diags(src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(
        d[0].message,
        "instance variable `@x' on Foo was previously assigned bool; this write assigns String"
    );
}

#[test]
fn ivar_op_writes_not_collected() {
    // `@x ||=` / `@x +=` are InstanceVariable{Or,Operator}WriteNodes, never
    // plain InstanceVariableWriteNode ⇒ never collected (probed silent).
    let src = b"class Foo\n  def m\n    @x = \"s\"\n    @x ||= 5\n    @x += 1\n  end\nend\n";
    assert!(ivar_diags(src).is_empty());
}

#[test]
fn ivar_self_setter_not_collected() {
    // `self.x =` is a `x=` method call, not an ivar write.
    let src = b"class Foo\n  def m\n    self.x = \"s\"\n    self.x = 5\n  end\nend\n";
    assert!(ivar_diags(src).is_empty());
}

#[test]
fn ivar_write_inside_block_with_literal_fires() {
    // A block is not a barrier; a literal write inside it is collected.
    let src = b"class Foo\n  def m\n    @x = \"s\"\n    [1].each do |i|\n      @x = 5\n    end\n  end\nend\n";
    let d = ivar_diags(src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(
        d[0].message,
        "instance variable `@x' on Foo was previously assigned String; this write assigns Integer"
    );
}

#[test]
fn ivar_module_instance_method_fires() {
    let src = b"module Foo\n  def m\n    @x = \"s\"\n    @x = 5\n  end\nend\n";
    let d = ivar_diags(src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].message, "instance variable `@x' on Foo was previously assigned String; this write assigns Integer");
}

#[test]
fn ivar_same_file_reopen_merges_group() {
    let src = b"class Foo\n  def a\n    @x = \"s\"\n  end\nend\nclass Foo\n  def b\n    @x = 5\n  end\nend\n";
    let d = ivar_diags(src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].message, "instance variable `@x' on Foo was previously assigned String; this write assigns Integer");
}

#[test]
fn ivar_nested_class_qualified_name() {
    let src = b"module A\n  class B\n    def m\n      @x = \"s\"\n      @x = 5\n    end\n  end\nend\n";
    let d = ivar_diags(src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].message, "instance variable `@x' on A::B was previously assigned String; this write assigns Integer");
}

#[test]
fn ivar_nested_def_is_barrier() {
    let src = b"class Foo\n  def m\n    @x = \"s\"\n    def inner\n      @x = 5\n    end\n  end\nend\n";
    assert!(ivar_diags(src).is_empty());
}

#[test]
fn ivar_singleton_def_skipped() {
    let src = b"class Foo\n  def self.m\n    @x = \"s\"\n    @x = 5\n  end\nend\n";
    assert!(ivar_diags(src).is_empty());
}

#[test]
fn ivar_top_level_def_skipped() {
    // A def outside any class ⇒ qualified prefix empty ⇒ never collected.
    let src = b"def m\n  @x = \"s\"\n  @x = 5\nend\n";
    assert!(ivar_diags(src).is_empty());
}

#[test]
fn ivar_leading_nil_then_single_typed_silent() {
    let src = b"class Foo\n  def m\n    @x = nil\n    @x = \"s\"\n  end\nend\n";
    assert!(ivar_diags(src).is_empty());
}

#[test]
fn ivar_leading_nil_fires_on_third_conflicting() {
    let src = b"class Foo\n  def m\n    @x = nil\n    @x = \"s\"\n    @x = 5\n  end\nend\n";
    let d = ivar_diags(src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].message, "instance variable `@x' on Foo was previously assigned String; this write assigns Integer");
}

#[test]
fn ivar_clear_to_nil_silent() {
    let src = b"class Foo\n  def m\n    @x = \"s\"\n    @x = nil\n  end\nend\n";
    assert!(ivar_diags(src).is_empty());
}

#[test]
fn ivar_none_canonical_kills_whole_group() {
    // First non-nil write reads an untyped param ⇒ canonical unresolvable ⇒
    // the WHOLE group is silent even though a later String vs Integer differs.
    let src = b"class Foo\n  def m(arg)\n    @x = arg\n    @x = \"s\"\n    @x = 5\n  end\nend\n";
    assert!(ivar_diags(src).is_empty());
}

#[test]
fn ivar_same_name_different_classes_no_fire() {
    let src = b"class A\n  def m\n    @x = \"s\"\n  end\nend\nclass B\n  def m\n    @x = 5\n  end\nend\n";
    assert!(ivar_diags(src).is_empty());
}

#[test]
fn ivar_rescue_single_class_bound_var_fires() {
    // Increment (a): `rescue StandardError => e` binds e to StandardError.
    let src = b"class Foo\n  def m\n    @e = \"s\"\n  rescue StandardError => error\n    @e = error\n  end\nend\n";
    let d = ivar_diags(src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].message, "instance variable `@e' on Foo was previously assigned String; this write assigns StandardError");
}

#[test]
fn ivar_rescue_bare_binds_standard_error() {
    let src = b"class Foo\n  def m\n    @e = \"s\"\n  rescue => error\n    @e = error\n  end\nend\n";
    let d = ivar_diags(src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].message, "instance variable `@e' on Foo was previously assigned String; this write assigns StandardError");
}

#[test]
fn ivar_rescue_multi_class_silent() {
    // Multi-class ⇒ union ⇒ not bound ⇒ the bound-var write is unresolvable.
    let src = b"class Foo\n  def m\n    @e = \"s\"\n  rescue TypeError, ArgumentError => error\n    @e = error\n  end\nend\n";
    assert!(ivar_diags(src).is_empty());
}

#[test]
fn ivar_rescue_project_exception_fires() {
    // Increment (a) resolves a discovered project exception class.
    let src = b"class MyError < StandardError\nend\nclass Foo\n  def m\n    @e = \"s\"\n  rescue MyError => error\n    @e = error\n  end\nend\n";
    let d = ivar_diags(src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].message, "instance variable `@e' on Foo was previously assigned String; this write assigns MyError");
}

#[test]
fn ivar_rescue_unknown_exception_silent() {
    let src = b"class Foo\n  def m\n    @e = \"s\"\n  rescue Nonexistent::Whatever => error\n    @e = error\n  end\nend\n";
    assert!(ivar_diags(src).is_empty());
}

/// Increment (b): a Kernel conversion types the first ivar write, so the
/// second write's class mismatch is witnessed — but ONLY when the argument
/// discriminates. Upstream #521 (`3d5dddbb`, ported at the
/// `v0.3.4 → v0.3.8` re-pin) stops pinning one overload for an UNTYPED
/// argument, and both engines then answer `Dynamic[union]`, on which no
/// negative rule fires. Fixture 60 line 59 (`Float(kwargs[:upload_duration])`
/// with the `rescue`-arm `= 0`) was one of the four re-pin false positives.
#[test]
fn ivar_kernel_conversion_of_untyped_argument_is_silent() {
    // A bare parameter: reference-SILENT at `ffb456b0` (both rows measured).
    let float_src = b"class Foo\n  def m(k)\n    @d = Float(k)\n  rescue ArgumentError, TypeError\n    @d = 0\n  end\nend\n";
    assert!(ivar_diags(float_src).is_empty(), "{:?}", ivar_diags(float_src));
    let int_src = b"class Foo\n  def m(a)\n    @n = Integer(a)\n    @n = \"x\"\n  end\nend\n";
    assert!(ivar_diags(int_src).is_empty(), "{:?}", ivar_diags(int_src));
}

/// …and the must-still-fire half of the same pair: a conversion whose
/// argument the reference can discriminate keeps its pinned return, so the
/// mismatch still fires. Both rows measured FIRING at `ffb456b0`.
#[test]
fn ivar_kernel_conversion_of_typed_argument_still_fires() {
    // A literal argument …
    let lit = b"class Foo\n  def m\n    @n = Integer(\"12\")\n    @n = \"x\"\n  end\nend\n";
    let d = ivar_diags(lit);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].message, "instance variable `@n' on Foo was previously assigned Integer; this write assigns String");
    // … and a parameter REBOUND to one, which the reference types and this
    // port's allow-list therefore refuses to declare untyped.
    let rebound = b"class Foo\n  def m(a)\n    a = \"12\"\n    @n = Integer(a)\n    @n = \"x\"\n  end\nend\n";
    let d = ivar_diags(rebound);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].message, "instance variable `@n' on Foo was previously assigned Integer; this write assigns String");
}

// --- call.argument-type-mismatch (ADR-64) --------------------------------
//
// The probe matrix, run against the LIVE reference oracle and pinned here.
// Every FIRE row asserts the (rule, anchor-span) parity the harness keys on;
// every SILENT row is a zero-FP guard the reference also stays silent on.

fn atm_diags(src: &[u8]) -> Vec<Diagnostic> {
    run(src)
        .into_iter()
        .filter(|d| d.rule_id == CALL_ARGUMENT_TYPE_MISMATCH)
        .collect()
}

#[test]
fn atm_nil_channel_single_overload_fires() {
    // `"a" + nil` — String#+ param `string` rejects nil (alias-aware nil
    // channel). Anchors on the `nil` argument node.
    let src = b"\"a\" + nil\n";
    let d = atm_diags(src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].severity, Severity::Error);
    assert_eq!(&src[d[0].start_offset..d[0].end_offset], b"nil");
    assert_eq!(d[0].receiver_type.as_deref(), Some("String"));
    assert_eq!(d[0].method_name.as_deref(), Some("+"));
    // Byte-parity with the oracle: single-overload names the parameter.
    assert_eq!(
        d[0].message,
        "argument type mismatch at parameter `other_string' of `+' on String: expected string, got nil"
    );
}

#[test]
fn atm_nil_channel_multi_overload_fires() {
    // `5 + nil` — Integer#+ has several numeric overloads, none admits nil.
    let src = b"5 + nil\n";
    let d = atm_diags(src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(&src[d[0].start_offset..d[0].end_offset], b"nil");
    assert_eq!(d[0].receiver_type.as_deref(), Some("Integer"));
    // Byte-parity with the oracle: multi-overload, NO parameter prefix; the
    // label joins per-overload written types first-seen (the bigdecimal
    // overloading reopen prepends BigDecimal onto core's four).
    assert_eq!(
        d[0].message,
        "argument type mismatch at `+' on Integer: expected BigDecimal | Integer | Float | Rational | Complex, got nil"
    );
}

#[test]
fn atm_nonnil_channel_multi_overload_fires_on_wrong_class() {
    // `[1, 2, 3].fetch("x")` — Array#fetch index params reject a concrete
    // String on every overload (non-coerce method). Since rbs 4.1 the block
    // overload spells its index as a BOUNDED type parameter (`[I < _ToInt,
    // T] (I index)`), so the label carries the bound alongside the plain
    // `int` of the other two — byte-parity with the oracle.
    let src = b"[1, 2, 3].fetch(\"x\")\n";
    let d = atm_diags(src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(&src[d[0].start_offset..d[0].end_offset], b"\"x\"");
    assert_eq!(d[0].receiver_type.as_deref(), Some("Array"));
    assert_eq!(
        d[0].message,
        "argument type mismatch at `fetch' on Array: expected int | _ToInt, got \"x\""
    );
}

#[test]
fn atm_nil_channel_int_alias_param_fires() {
    // `"abc".center(nil)` — the width param is the `int` alias; the nil
    // channel sees through the alias (NilClass has no `to_int`).
    let src = b"\"abc\".center(nil)\n";
    let d = atm_diags(src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(&src[d[0].start_offset..d[0].end_offset], b"nil");
    assert_eq!(
        d[0].message,
        "argument type mismatch at parameter `width' of `center' on String: expected int, got nil"
    );
}

#[test]
fn atm_fires_alongside_wrong_arity_at_one_site() {
    // `"abc".center(nil, "x", "y")` — the reference emits BOTH wrong-arity
    // (too many args) AND argument-type-mismatch (first arg nil vs `int`).
    let src = b"\"abc\".center(nil, \"x\", \"y\")\n";
    let all = run(src);
    assert!(
        all.iter().any(|d| d.rule_id == CALL_WRONG_ARITY),
        "expected wrong-arity: {all:?}"
    );
    let atm: Vec<_> = all
        .iter()
        .filter(|d| d.rule_id == CALL_ARGUMENT_TYPE_MISMATCH)
        .collect();
    assert_eq!(atm.len(), 1, "expected one ATM: {all:?}");
    assert_eq!(&src[atm[0].start_offset..atm[0].end_offset], b"nil");
}

#[test]
fn atm_hash_literal_miss_folds_to_nil_and_fires() {
    // `h["z"]` on a Hash literal folds to `nil`, so it takes the nil channel.
    let src = b"h = { \"a\" => 1 }\n\"p\".center(h[\"z\"])\n";
    let d = atm_diags(src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].receiver_type.as_deref(), Some("String"));
}

// --- SILENT rows (zero-FP guards) ----------------------------------------

#[test]
fn atm_silent_universal_equality() {
    // `== != eql? equal? <=>` accept any argument by contract.
    assert!(atm_diags(b"x = \"a\"\nx == nil\n").is_empty());
    assert!(atm_diags(b"x = \"a\"\nx.eql?(nil)\n").is_empty());
    assert!(atm_diags(b"x = \"a\"\nx <=> nil\n").is_empty());
}

#[test]
fn atm_silent_coerce_dispatch_operator() {
    // `5 + "s"` — a coerce-dispatch operator on a multi-overload method;
    // any user type may define `coerce`, so the non-nil channel excludes it.
    assert!(atm_diags(b"5 + \"s\"\n").is_empty());
}

#[test]
fn atm_silent_faithful_gate_on_interface_alias_param() {
    // `"abc".center("s")` — the `int` alias param degrades to gradual, so a
    // concrete non-nil argument the alias would reject stays silent (the
    // single-overload non-nil channel requires a FAITHFUL param).
    assert!(atm_diags(b"\"abc\".center(\"s\")\n").is_empty());
    // `"a" + 5` — String#+ param `string` (interface-alias) ⇒ silent.
    assert!(atm_diags(b"\"a\" + 5\n").is_empty());
}

#[test]
fn atm_silent_non_plain_positional_args() {
    // A splat / bare-keyword argument makes the call non-plain-positional;
    // the whole call is skipped (any non-plain arg, not just the first).
    assert!(atm_diags(b"def f(a)\n  \"abc\".center(*a)\nend\n").is_empty());
    assert!(atm_diags(b"def f(a)\n  \"abc\".center(nil, *a)\nend\n").is_empty());
}

#[test]
fn atm_silent_dynamic_argument() {
    // A method-parameter argument types Dynamic; the multi-overload non-nil
    // channel requires a single concrete RBS-known class ⇒ silent.
    assert!(atm_diags(b"def f(x)\n  [1, 2, 3].fetch(x)\nend\n").is_empty());
}

#[test]
fn atm_silent_project_class_argument() {
    // A non-RBS project-class argument: its duck-typed conversion protocol is
    // invisible, so the non-nil channel cannot refute acceptance ⇒ silent.
    assert!(atm_diags(b"class Foo\nend\n[1, 2, 3].fetch(Foo.new)\n").is_empty());
}

#[test]
fn atm_silent_correct_arguments() {
    // Well-typed arguments never fire.
    assert!(atm_diags(b"[1, 2, 3].fetch(0)\n").is_empty());
    assert!(atm_diags(b"\"abc\".center(5)\n").is_empty());
}

// ---------------------------------------------------------------------------
// rigor-rs#136 — later call arguments type from the call's ENTRY scope (the
// port of the reference's `OperandWalk` per-node scope index, upstream
// rigor#1310). Every row below is oracle-measured on the pinned reference
// (fresh cwd, `--no-cache`).
// ---------------------------------------------------------------------------

/// Headline row: the second argument is typed from the scope it was entered
/// from — which already holds the first argument's `unshift` mutation — so
/// `b` is the widened carrier there and `b.first.upcase` declines. Silent
/// on both engines at the e59b7b89 pin.
#[test]
fn arg_entry_scope_headline_row_is_silent() {
    let src = b"b = [1, 2, 3]\nputs(b.unshift(\"s\"), b.first.upcase)\n";
    let diags = run(src);
    assert!(
        diags.iter().all(|d| d.rule_id != CALL_UNDEFINED_METHOD),
        "expected silent (reference is silent at e59b7b89), got {diags:?}"
    );
}

/// Swapped control: `b.first` evaluates BEFORE the `unshift` mutation, so it
/// types from the pre-mutation binding and `upcase` on `1` fires — on both
/// engines, at `2:14`.
#[test]
fn arg_entry_scope_swapped_control_fires() {
    let src = b"b = [1, 2, 3]\nputs(b.first.upcase, b.unshift(\"s\"))\n";
    let diags = run(src);
    let d = diags
        .iter()
        .find(|d| d.rule_id == CALL_UNDEFINED_METHOD)
        .unwrap_or_else(|| panic!("expected undefined-method, got {diags:?}"));
    assert_eq!(d.message, "undefined method `upcase' for 1");
    assert_eq!(&src[d.start_offset..d.end_offset], b"upcase");
}

/// The same entry-scope replay, nested: the mutator call's own argument is
/// still typed from the scope that ran before it.
#[test]
fn arg_entry_scope_nested_call_arg_fires() {
    let src = b"b = [1, 2, 3]\nb.unshift(b.first.upcase)\n";
    let diags = run(src);
    let d = diags
        .iter()
        .find(|d| d.rule_id == CALL_UNDEFINED_METHOD)
        .unwrap_or_else(|| panic!("expected undefined-method, got {diags:?}"));
    assert_eq!(&src[d.start_offset..d.end_offset], b"upcase");
}

/// A straight rebind no longer leaks backwards across statements: the flat
/// env used to type `s.upcase` as `5` — a live false positive the oracle is
/// silent on (the statement-sequence analogue of the argument rule).
#[test]
fn entry_scope_rebind_does_not_reach_back() {
    let diags = run(b"s = \"x\"\ns.upcase\ns = 5\n");
    assert!(diags.is_empty(), "expected silent, got {diags:?}");
}

/// And in the other direction the write still reaches the later operand:
/// `n.even?` typed from the scope the FIRST arg was entered from fires
/// `for "x"` on both engines.
#[test]
fn arg_entry_scope_earlier_arg_keeps_entry_scope() {
    let src = b"n = \"x\"\nputs(n.even?, n = 5)\n";
    let diags = run(src);
    let d = diags
        .iter()
        .find(|d| d.rule_id == CALL_UNDEFINED_METHOD)
        .unwrap_or_else(|| panic!("expected undefined-method, got {diags:?}"));
    assert_eq!(d.message, "undefined method `even?' for \"x\"");
}

/// An `if` replays only the TAKEN branch's statements: the else body still
/// reads the pre-branch `b`, so `b.first.upcase` fires (the sibling branch's
/// `unshift` never reached the site's scope).
#[test]
fn arg_entry_scope_if_else_branch_is_exclusive() {
    let src =
        b"b = [1, 2, 3]\nc = true\nif c\n  b.unshift(\"s\")\nelse\n  b.first.upcase\nend\n";
    let diags = run(src);
    let d = diags
        .iter()
        .find(|d| d.rule_id == CALL_UNDEFINED_METHOD)
        .unwrap_or_else(|| panic!("expected undefined-method, got {diags:?}"));
    assert_eq!(&src[d.start_offset..d.end_offset], b"upcase");
}

// ---------------------------------------------------------------------------
// rigor-rs#306 — effect spans NOT linked in `flow_children` (a `for` index
// target, a `rescue =>` target, `Range` bounds) rode `path_unconditional`'s
// "inside `id` but inside no linked child" fallback, which answered TRUE and
// minted the unconditional mutator nominal where nothing was proven. Every
// row below is oracle-measured on the pinned reference (fresh cwd,
// `--no-cache`).
// ---------------------------------------------------------------------------

/// Headline row 1: the `[]=` store a `for h[:k]` index performs is per-
/// iteration and the loop may not run — the reference joins the
/// zero-iteration scope into `eval_for`'s continuation, so `h` keeps its
/// join, never the widened nominal the fallback minted.
#[test]
fn for_index_target_store_is_conditional() {
    let src = b"h = {}\nfor h[:k] in [[\"x\"]]; end\nh.frobnicate\n";
    let diags = run(src);
    assert!(
        diags.iter().all(|d| d.rule_id != CALL_UNDEFINED_METHOD),
        "expected silent (reference is silent at e59b7b89), got {diags:?}"
    );
}

/// The store stays conditional in the other `for` index shapes too — a
/// multi-target slot and a bare splat index (`for *h[:k] in xs`).
#[test]
fn for_index_target_multi_and_splat_are_conditional() {
    for src in [
        &b"h = {}\nfor w, h[:k] in [[1, 2]]; end\nh.frobnicate\n"[..],
        &b"h = {}\nfor *h[:k] in [[1]]; end\nh.frobnicate\n"[..],
    ] {
        let diags = run(src);
        assert!(
            diags.iter().all(|d| d.rule_id != CALL_UNDEFINED_METHOD),
            "expected silent (reference is silent at e59b7b89), got {diags:?}"
        );
    }
}

/// Headline row 2: `rescue => h[:k]` stores the exception through `[]=` only
/// when the clause fires (`bind_rescue_reference` binds inside the clause's
/// edge), so `h` widens Dynamic — never the unconditional nominal.
#[test]
fn rescue_reference_store_is_conditional() {
    for src in [
        &b"h = {}\nbegin\n  raise StandardError\nrescue => h[:k]\nend\nh.frobnicate\n"[..],
        &b"h = {}\nbegin\n  raise StandardError\nrescue StandardError, RuntimeError => h[:k]\nend\nh.frobnicate\n"[..],
    ] {
        let diags = run(src);
        assert!(
            diags.iter().all(|d| d.rule_id != CALL_UNDEFINED_METHOD),
            "expected silent (reference is silent at e59b7b89), got {diags:?}"
        );
    }
}

/// Headline row 3: a range evaluates its bounds unconditionally in order
/// (the reference's `OPERAND_CONTAINERS` lists `RangeNode`), so the LEFT
/// bound's `unshift` widens `b` before `b.first` types inside the RIGHT
/// bound — silent on both engines.
#[test]
fn range_left_bound_effect_reaches_right_bound() {
    let src = b"b = [1, 2, 3]\nx = (b.unshift(\"s\"))..b.first.upcase\n";
    let diags = run(src);
    assert!(
        diags.iter().all(|d| d.rule_id != CALL_UNDEFINED_METHOD),
        "expected silent (reference is silent at e59b7b89), got {diags:?}"
    );
}

/// Ordering control: the RIGHT bound's effect never reaches back — `b.first`
/// in the left bound still types the pre-mutation `Tuple`, so `upcase` on
/// `1` fires on both engines, and the post-statement `b` is the widened
/// carrier the unconditional bound mutation left (`frobnicate` fires too).
#[test]
fn range_right_bound_effect_does_not_reach_back() {
    let src = b"b = [1, 2, 3]\nx = b.first.upcase..(b.unshift(\"s\"))\n";
    let diags = run(src);
    let d = diags
        .iter()
        .find(|d| d.rule_id == CALL_UNDEFINED_METHOD)
        .unwrap_or_else(|| panic!("expected undefined-method, got {diags:?}"));
    assert_eq!(&src[d.start_offset..d.end_offset], b"upcase");

    let src = b"b = [1, 2, 3]\nx = b.first.upcase..(b.unshift(\"s\"))\nb.frobnicate\n";
    let diags = run(src);
    let names: Vec<&str> = diags
        .iter()
        .filter(|d| d.rule_id == CALL_UNDEFINED_METHOD)
        .map(|d| &src[d.start_offset..d.end_offset])
        .map(|s| std::str::from_utf8(s).unwrap())
        .collect();
    assert_eq!(names, ["upcase", "frobnicate"], "got {diags:?}");
}

/// Sibling exclusion preserved: a mutation inside a `for` BODY still widens
/// `Dynamic` (the loop may not run), so `b.frobnicate` stays silent — the
/// `Cond` edge, not the unconditional mint.
#[test]
fn loop_body_mutation_stays_conditional() {
    let src = b"b = [1, 2, 3]\nfor x in [1]; b.unshift(\"s\"); end\nb.frobnicate\n";
    let diags = run(src);
    assert!(
        diags.iter().all(|d| d.rule_id != CALL_UNDEFINED_METHOD),
        "expected silent (reference is silent at e59b7b89), got {diags:?}"
    );
}

/// Issue #146 — a `lambda`/`proc`/ordinary-block body sitting in a TYPED
/// (operand) position is never scope-entered on the reference:
/// `propagate`/`closure_scope` fills it with the parent scope and floors the
/// closure's own locals to `Dynamic[top]`, so `Float(q)` declines. The same
/// body at statement level IS entered and fires.
#[test]
fn issue_146_closure_in_operand_position_is_silent() {
    // The issue's row 1, verbatim.
    assert!(run(b"NL3 = { a: lambda { |q| q = 1; Float(q).w_nl3 } }\n").is_empty());
    // The same mechanism around it — array / call-argument operands and the
    // `->` spelling — plus an ordinary block's parameter and a
    // body-introduced local, which `closure_scope` floors the same way.
    assert!(run(b"x = [lambda { |q| q = 1; Float(q).wA }]\n").is_empty());
    assert!(run(b"puts(lambda { |q| q = 1; Float(q).wB })\n").is_empty());
    assert!(run(b"h = { a: ->(q) { q = 1; Float(q).wC } }\n").is_empty());
    assert!(run(b"h = { a: [1].each { |n| Float(n).wD } }\n").is_empty());
    assert!(run(b"h = { a: [1].each { |n| n = 2; Float(n).wE } }\n").is_empty());
    // A same-named write inside a DIFFERENT block must not leak in:
    // `n = 2` here is `each`'s block-local, invisible to the sibling body.
    assert!(run(
        b"h = { a: [1].each { |n| n = 2; Float(n).wF } }\nb = { c: [1].each { |n| Float(n).wG } }\n"
    )
    .is_empty());
    // CONTROLS — the entered spellings keep firing (the reference answers
    // `for 1.0`; the port's `for Float` carrier is the recorded gap, not an
    // FP), and a captured OUTER local keeps its enclosing binding inside an
    // operand closure (`x` is not a block local there).
    let diags = run(b"lambda { |q| q = 1; Float(q).wK }\n");
    assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
    assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
    let diags = run(b"[1].each { |n| Float(n).wH }\n");
    assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
    assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
    let diags = run(b"x = 0\nh = { a: lambda { x = 1; Float(x).wX } }\n");
    assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
    assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
}

/// Issue #146 — a `Constant`-receiver call whose argument reaches more than
/// one distinct precise value is folded member-wise on the reference
/// (`"abc"[v]` -> `"b" | "c"`), a union no negative rule fires on — never
/// the flat `method_return` class tier 3 minted.
#[test]
fn issue_146_multi_value_argument_stays_silent() {
    // The issue's row 2, verbatim.
    assert!(run(b"def g(c)\n  v = 1\n  v = 2 if c\n  \"abc\"[v].w_lit\nend\n").is_empty());
    // The same shape at top level, on another value-pinned receiver, and
    // with the join inlined into the argument.
    assert!(run(b"v = 1\nv = 2 if $c\n\"abc\"[v].wA\n").is_empty());
    assert!(run(b"def g(c)\n  v = 1\n  v = 2 if c\n  1.fdiv(v).wB\nend\n").is_empty());
    assert!(run(b"def g(c)\n  \"abc\"[c ? 1 : 2].wC\nend\n").is_empty());
    // CONTROLS — a literal argument still folds (`for "b"`), and a single
    // reaching value keeps its nominal pin (the reference's `for "b"` there
    // is the recorded precision gap, not an FP).
    let diags = run(b"\"abc\"[1].wK\n");
    assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
    assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
    assert_eq!(diags[0].message, "undefined method `wK' for \"b\"");
    let diags = run(b"def g\n  v = 1\n  \"abc\"[v].wL\nend\n");
    assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
    assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
}

/// rigor-rs#309, adversarial review — a union of DISTINCT literal
/// collection carriers stays a union after a store mutator. The
/// reference's `widen_union` widens each arm memberwise (`widen_tuple`
/// mints `Array[element_type]`; `widen_hash_shape` mints key/value args)
/// and `Combinator.union` dedups only structurally identical arms, so
/// `Tuple[1] | Tuple[2]` under `push(3)` is `Array[1 | top] |
/// Array[2 | top]` — a union receiver no negative rule fires on. The
/// port's flow-env widening minted a bare `Nominal[Array]` per literal
/// arm, collapsed the union, and fired `frobnicate` where the oracle is
/// silent: a new FP family introduced by the first union-normalization
/// fix.
#[test]
fn issue_309_union_literal_arms_stay_distinct_end_to_end() {
    for src in [
        // The reviewer's bisected row: two distinct literal Tuple arms.
        b"def f(c)\n  a = c ? [1] : [2]\n  a.push(3)\n  a.frobnicate_zzz\nend\n" as &[u8],
        // The same union via the rescue join — a rebind on the protected
        // path unions with the entry literal.
        b"def f\n  b = [1]\n  begin\n    b = [2]\n  rescue\n    nil\n  end\n  b.push(3)\n  b.frobnicate_zzz\nend\n",
        // … and via a plain `if`-modifier rebind.
        b"def f(c)\n  a = [1]\n  a = [2] if c\n  a.push(3)\n  a.frobnicate_zzz\nend\n",
        // Empty against non-empty, longer literals, string members.
        b"def f(c)\n  a = c ? [1] : []\n  a.push(3)\n  a.frobnicate_zzz\nend\n",
        b"def f(c)\n  a = c ? [1, 2] : [3, 4]\n  a << 5\n  a.frobnicate_zzz\nend\n",
        b"def f(c)\n  a = c ? [\"s\"] : [\"t\"]\n  a << \"u\"\n  a.frobnicate_zzz\nend\n",
        // The Hash twin: distinct HashShape arms under `[]=`.
        b"def f(c)\n  h = c ? {a: \"s\"} : {b: 1}\n  h[:k] = 9\n  h.frobnicate_zzz\nend\n",
        // The gitlab changes_access_logger shape — the conditional `[]=`
        // edge's grown member evidence must survive `compact!` or the
        // `Hash#stringify_keys!` FP family returns.
        b"def f(error)\n  h = {a: 1, p: @x}\n  h[:e] = error if error\n  h.compact!\n  h.frobnicate_zzz\nend\n",
    ] {
        let diags = run(src);
        assert!(
            diags.iter().all(|d| d.rule_id != CALL_UNDEFINED_METHOD),
            "expected silent (reference is silent at e59b7b89), got {diags:?} for {:?}",
            String::from_utf8_lossy(src),
        );
    }
    // The toplevel spellings of the two bisected rows — the flow-env
    // replay path that minted the collapsed carrier.
    for src in [
        b"a = $c ? [1] : [2]\na.push(3)\na.frobnicate_zzz\n" as &[u8],
        b"h = $c ? {a: \"s\"} : {b: 1}\nh[:k] = 9\nh.frobnicate_zzz\n",
    ] {
        let diags = run(src);
        assert!(
            diags.iter().all(|d| d.rule_id != CALL_UNDEFINED_METHOD),
            "expected silent, got {diags:?} for {:?}",
            String::from_utf8_lossy(src),
        );
    }
}

/// The dedup is structural, not positional: arms that widen to the SAME
/// carrier still collapse and fire — identical literal seeds, and the
/// rescue join that unions a `Tuple`-grown `Nominal[Array]` arm with the
/// entry `Nominal` arm (`coll_union_literal_arms_converge_fires`'s
/// end-to-end twin).
#[test]
fn issue_309_union_converging_arms_still_fire_end_to_end() {
    let diags = run(b"def f(c)\n  a = c ? [1] : [1]\n  a.push(3)\n  a.frobnicate_zzz\nend\n");
    assert_eq!(
        diags.iter().filter(|d| d.rule_id == CALL_UNDEFINED_METHOD).count(),
        1,
        "expected the converged carrier to fire, got {diags:?}"
    );
    let diags = run(
        b"def f\n  b = [1, 2, 3]\n  begin\n    b.unshift(5)\n  rescue\n    nil\n  end\n  b.push(6)\n  b.frobnicate_zzz\nend\n",
    );
    assert_eq!(
        diags.iter().filter(|d| d.rule_id == CALL_UNDEFINED_METHOD).count(),
        1,
        "expected the rescue-grown carrier to fire, got {diags:?}"
    );
}

/// rigor-rs#341 — a local write inside a `when` clause's CONDITIONS or an
/// `in` clause's pattern/guard never binds: the reference shape-reads
/// those extents (`Narrowing.case_when_scopes`,
/// `apply_in_pattern_bindings`) and sub-evals only the clause body
/// (`StatementEvaluator#eval_when_or_in` walks `node.statements` alone),
/// so `case v; when (q = 1; Integer) then Float(q).w; end` is silent on
/// the oracle while `local_reach`'s lexical span scan collected the
/// write and minted `Float` — the `edge_evaluates` `when`-exclusion's
/// (rigor-rs#334) sibling hole. Every row below is oracle-measured
/// SILENT for `call.undefined-method` at e59b7b89.
#[test]
fn issue_341_case_clause_writes_stay_silent_end_to_end() {
    for src in [
        // The issue row — a multi-statement condition.
        b"def f(v)\n  case v\n  when (q = 1; Integer) then\n    Float(q).w\n  end\nend\n" as &[u8],
        // The bare-write condition.
        b"def f(v)\n  case v\n  when q = 1 then\n    Float(q).w\n  end\nend\n",
        // `&&` / `||` condition shapes.
        b"def f(v)\n  case v\n  when Integer && (q = 1) then\n    Float(q).w\n  end\nend\n",
        b"def f(v)\n  case v\n  when (q = 1) || Integer then\n    Float(q).w\n  end\nend\n",
        // One condition of several.
        b"def f(v)\n  case v\n  when Integer, (q = 1; String) then\n    Float(q).w\n  end\nend\n",
        // With an `else`: the write reaches neither body.
        b"def f(v)\n  case v\n  when (q = 1; Integer) then\n    Float(q).w\n  else\n    Float(q).w\n  end\nend\n",
        // Nor the post-`case` read.
        b"def f(v)\n  case v\n  when (q = 1; Integer) then\n    1\n  end\n  Float(q).w\nend\n",
        // A nested `case`'s own `when` conditions are just as inert.
        b"def f(v)\n  case v\n  when (case 1\n        when (q = 1; Integer) then 0\n        else 1\n        end; Integer) then\n    Float(q).w\n  end\nend\n",
        // `in` patterns and `if`/`unless` guards ride the same
        // never-evaluated extent (Prism folds the guard into the
        // pattern's `IfNode`).
        b"def f(v)\n  case v\n  in ^(q = 1) then\n    Float(q).w\n  end\nend\n",
        b"def f(v)\n  case v\n  in Integer if (q = 1; true) then\n    Float(q).w\n  end\nend\n",
        b"def f(v)\n  case v\n  in Integer unless (q = 1; false) then\n    Float(q).w\n  end\nend\n",
        // The read can sit inside the shape-only extent too — `q`'s
        // write must not pin it there either.
        b"def f(v)\n  case v\n  when (q = 1; q.is_a?(Integer)) then\n    1\n  end\nend\n",
    ] {
        let diags = run(src);
        assert!(
            diags.iter().all(|d| d.rule_id != CALL_UNDEFINED_METHOD),
            "expected silent (reference is silent at e59b7b89), got {diags:?} for {:?}",
            String::from_utf8_lossy(src),
        );
    }
}

/// The exclusion is the clause's CONDITION/PATTERN extent, not the
/// `case`: a write before it, inside a branch body, or joining after it
/// still binds and still witnesses.
#[test]
fn issue_341_real_writes_still_witness_end_to_end() {
    for src in [
        // A pre-`case` write.
        b"def f(v)\n  q = 1\n  case v\n  when Integer then\n    Float(q).w\n  end\nend\n" as &[u8],
        // A `when`-BODY write reaches the body's own read…
        b"def f(v)\n  case v\n  when Integer then\n    q = 1\n    Float(q).w\n  end\nend\n",
        // …and the post-`case` read.
        b"def f(v)\n  case v\n  when Integer then\n    q = 1\n  end\n  Float(q).w\nend\n",
        // A real body write still lands beside an excluded condition
        // write for the post-`case` read.
        b"def f(v)\n  case v\n  when (q = 1; Integer) then\n    q = 2\n  end\n  Float(q).w\nend\n",
    ] {
        let diags = run(src);
        assert_eq!(
            diags.iter().filter(|d| d.rule_id == CALL_UNDEFINED_METHOD).count(),
            1,
            "expected one undefined-method, got {diags:?} for {:?}",
            String::from_utf8_lossy(src)
        );
    }
}

/// rigor-rs#357 — the same never-evaluated `when`-condition / `in`-pattern
/// write under a rescue MODIFIER: `x = (case v when (q = 1; Integer) then
/// 1 end) rescue nil` flattens the `case` into a `Statements{Recovered}`
/// carrier, so no `Node::Case`/`Node::When` survives for
/// `unevaluated_case_clause_spans` to exclude — the leak path
/// `rigor-rs#341`'s span filter cannot see. The `Recovered::blocked` mark
/// the recovery walk already stamps on those children is recorded on the
/// AST instead, and reach/flow collectors skip writes inside it. Every row
/// below is oracle-measured SILENT for `call.undefined-method` at
/// e59b7b89.
#[test]
fn issue_357_modifier_rescue_blocked_writes_stay_silent_end_to_end() {
    for src in [
        // The issue row, top level and inside a `def`.
        b"x = (case v\nwhen (q = 1; Integer) then 1\nend) rescue nil\nFloat(q).w\n" as &[u8],
        b"def f(v)\n  x = (case v\n  when (q = 1; Integer) then 1\n  end) rescue nil\n  Float(q).w\nend\n",
        // The no-parens spelling reaches the same carrier.
        b"x = case v\nwhen (q = 1; Integer) then 1\nend rescue nil\nFloat(q).w\n",
        // Nested rescue modifiers.
        b"x = ((case v\nwhen (q = 1; Integer) then 1\nend) rescue nil) rescue nil\nFloat(q).w\n",
        // The `case` wrapped in a `begin` under the modifier.
        b"x = begin\ncase v\nwhen (q = 1; Integer) then 1\nend\nend rescue nil\nFloat(q).w\n",
        // An `in` pattern guard under the modifier.
        b"x = (case v\nin Integer if (q = 1) then 1\nend) rescue nil\nFloat(q).w\n",
        // Every arm terminated — the all-dead join still never evaluates
        // the conditions.
        b"x = (case v\nwhen (q = 1; Integer) then raise\nelse raise\nend) rescue nil\nFloat(q).w\n",
        // Other blocked positions the same carrier replays: a `super`
        // operand and the dead right of a short-circuit.
        b"x = (super(q = 1)) rescue nil\nFloat(q).w\n",
        b"x = (c && (q = 1; raise)) rescue nil\nFloat(q).w\n",
    ] {
        let diags = run(src);
        assert!(
            diags.iter().all(|d| d.rule_id != CALL_UNDEFINED_METHOD),
            "expected silent (reference is silent at e59b7b89), got {diags:?} for {:?}",
            String::from_utf8_lossy(src),
        );
    }
}

/// The blocked exclusion is positional, not a rescue-modifier blanket: an
/// ordinary write under the modifier still binds, a `when`-BODY write still
/// lands, an `if`-arm write still lands, and a blocked write must not
/// overwrite an EARLIER binding either — `q = "s"` survives the discarded
/// `q = 1`, so `q.upcase.w` fires `for "S"` on the oracle. A blocked
/// content-mutation mark outside an iterated body never lands either, so
/// `h` keeps `[]` and `h.first` folds `nil` (`super(h.push(1))` —
/// rigor-rs#312's scan reaches only iterated bodies).
#[test]
fn issue_357_evaluated_writes_still_witness_end_to_end() {
    for src in [
        b"x = (q = 1) rescue nil\nFloat(q).w\n" as &[u8],
        b"x = (q = 1; q = \"s\") rescue nil\nFloat(q).w\n",
        // `when`-arm bodies still join into the post-scope.
        b"x = (case v\nwhen Integer then (q = 1)\nend) rescue nil\nFloat(q).w\n",
        b"x = (case v\nwhen Integer then (q = 1)\nelse (q = \"s\")\nend) rescue nil\nFloat(q).w\n",
        // An `if` arm under the modifier is an evaluated position.
        b"x = if c then (q = 1; 2) end rescue 1\nFloat(q).w\n",
        // A `begin` block under the modifier.
        b"x = (begin\nq = 1\nend) rescue nil\nFloat(q).w\n",
        // The prior binding survives the discarded write.
        b"q = \"s\"\nx = (case v\nwhen (q = 1; Integer) then 1\nend) rescue nil\nq.upcase.w\n",
        // The blocked mutation mark does not widen `h`.
        b"h = []\nx = (super(h.push(1))) rescue nil\nh.first.w\n",
    ] {
        let diags = run(src);
        assert_eq!(
            diags.iter().filter(|d| d.rule_id == CALL_UNDEFINED_METHOD).count(),
            1,
            "expected one undefined-method, got {diags:?} for {:?}",
            String::from_utf8_lossy(src)
        );
    }
}
