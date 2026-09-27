use super::*;
use rigor_parse::{lower, parse, LoweredAst};
use rigor_index::CoreIndex;

/// The `CoreIndex` is passed in, never built per project: it is the most
/// expensive thing in this module by an order of magnitude (RBS load), and
/// the graded property does not depend on it.
fn build(srcs: &[Vec<u8>], core: &CoreIndex) -> SourceIndex {
    let asts: Vec<LoweredAst> = srcs.iter().map(|s| lower(&parse(s))).collect();
    let refs: Vec<&LoweredAst> = asts.iter().collect();
    SourceIndex::build_project(&refs, core)
}

fn owned(srcs: Vec<&[u8]>) -> Vec<Vec<u8>> {
    srcs.into_iter().map(|s| s.to_vec()).collect()
}

/// Every name the graded pairs are drawn from: every class the override
/// index knows, plus names it does NOT know (an unrelated owner must stay
/// `false` through both paths).
fn universe(idx: &SourceIndex) -> Vec<String> {
    let mut names: Vec<String> = idx.override_classes.keys().cloned().collect();
    names.sort();
    names.push("NoSuchClass".to_string());
    names.push("".to_string());
    names
}

/// Grade `closure.contains(owner)` against the pre-#94 walk for EVERY
/// ordered pair over the universe, through both the memoized entry point
/// (one shared [`AncestorClosures`] map, so cache hits are exercised) and a
/// freshly built closure (so a cache hit can never be what makes it agree).
/// Returns `(pairs graded, pairs that were related)`.
fn grade(idx: &SourceIndex, label: &str) -> (usize, usize) {
    let names = universe(idx);
    let mut closures = AncestorClosures::new();
    let (mut graded, mut related) = (0usize, 0usize);
    for c in &names {
        let fresh = idx.build_ancestor_closure(c);
        for o in &names {
            let legacy = idx.related_to_owner(c, o);
            let cached = idx.ancestor_closure(c, &mut closures).contains(o);
            assert_eq!(legacy, cached, "{label}: cached closure disagrees at ({c:?}, {o:?})");
            let f = fresh.contains(o);
            assert_eq!(legacy, f, "{label}: fresh closure disagrees at ({c:?}, {o:?})");
            graded += 1;
            if legacy {
                related += 1;
            }
        }
    }
    (graded, related)
}

/// The override-graph shapes the probe corpora actually contain — the four
/// permuted probe-1 files, the probe-2 reopen trio, and the awkward shapes a
/// real corpus does contain but those two do not: cycles, a self-referential
/// class/module, a diamond, and lexical nesting where the SAME short name
/// resolves to a nested class in one scope and a toplevel one in another.
#[test]
fn closure_matches_legacy_on_probe_corpora() {
    let corpora: Vec<(&str, Vec<Vec<u8>>)> = vec![
        ("probe1", owned(super::probes_s92::probe1_sources())),
        (
            "probe2",
            owned(vec![
                b"class Base\n  def m\n    1\n  end\nend\nSHARED = 1\nmodule Wrap\n  DUP = 1\nend\n",
                b"class Base\n  private\n  def m\n    2\n  end\nend\nmodule Wrap\n  DUP = 2\nend\nclass Sub < Base\n  private\n  def m\n    3\n  end\nend\nSOLO = 7\n",
                b"class Solo\n  def q\n    1\n  end\nend\n",
            ]),
        ),
        (
            // A superclass cycle, a self-superclass, and a self-include:
            // the shapes the walk's `seen` guard exists for.
            "cycles",
            owned(vec![
                b"class A < B\nend\nclass B < A\nend\nclass S < S\nend\n",
                b"module Loop\n  include Loop\nend\nclass UsesLoop\n  include Loop\nend\n",
                b"class A\n  include Loop\nend\n",
            ]),
        ),
        (
            // A diamond plus a deep-ish chain, reopened across files so the
            // includes accumulate in source order (MRO-bearing).
            "diamond",
            owned(vec![
                b"module Top\nend\nmodule Left\n  include Top\nend\nmodule Right\n  include Top\nend\n",
                b"class Mid\n  include Left\nend\nclass Mid\n  include Right\nend\nclass Leaf < Mid\nend\nclass Leafer < Leaf\n  include Left\nend\n",
            ]),
        ),
        (
            // Lexical nesting: `include Shared` inside `Outer` resolves to
            // `Outer::Shared`, the same text at toplevel resolves to
            // `Shared` — the qualification keystone the walk must preserve.
            "nesting",
            owned(vec![
                b"module Shared\nend\nmodule Outer\n  module Shared\n  end\n  class Inner\n    include Shared\n  end\n  class Deep < Inner\n  end\nend\nclass Flat\n  include Shared\nend\n",
                b"module Outer\n  class Inner\n    include Outer::Shared\n  end\nend\nclass Other < Outer::Deep\nend\n",
            ]),
        ),
    ];
    let core = CoreIndex::new();
    let mut total_related = 0usize;
    for (label, srcs) in &corpora {
        let idx = build(srcs, &core);
        let (graded, related) = grade(&idx, label);
        println!("--- {label}: {graded} pairs graded, {related} related");
        total_related += related;
    }
    assert!(
        total_related >= 20,
        "the corpora must EXERCISE relatedness, not agree vacuously ({total_related})"
    );
}

/// A deterministic xorshift64 — the randomized hierarchies are reproducible
/// from the fixed seed, so a failure is replayable.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// A random project: `n` classes and modules spread over 3 files, with
/// reopens, random `include`s, random superclasses, lexically nested
/// namespaces, references to names that resolve nowhere, and no acyclicity
/// guarantee whatsoever (cycles are the point).
fn random_hierarchy(rng: &mut Rng, n: usize) -> Vec<Vec<u8>> {
    let pool: Vec<String> = (0..n)
        .map(|i| match i % 4 {
            0 => format!("C{i}"),
            1 => format!("M{i}"),
            2 => format!("Ns::C{i}"),
            _ => format!("M{i}"),
        })
        .collect();
    // As-written ancestor names: bare short names (so lexical resolution
    // does the work), fully qualified names, and a name that exists nowhere.
    let refname = |rng: &mut Rng| -> String {
        match rng.below(8) {
            0 => "Absent".to_string(),
            1..=2 => {
                let p = &pool[rng.below(pool.len())];
                p.rsplit("::").next().unwrap().to_string()
            }
            _ => pool[rng.below(pool.len())].clone(),
        }
    };
    let mut files: Vec<String> = vec![String::new(), String::new(), String::new()];
    for _ in 0..(n * 2) {
        let target = pool[rng.below(pool.len())].clone();
        let file = rng.below(files.len());
        let (nested, short) = match target.split_once("::") {
            Some((_, s)) => (true, s.to_string()),
            None => (false, target.clone()),
        };
        let is_module = short.starts_with('M');
        let mut body = String::new();
        for _ in 0..rng.below(3) {
            body.push_str(&format!("  include {}\n", refname(rng)));
        }
        let head = if is_module {
            format!("module {short}\n")
        } else if rng.below(3) == 0 {
            format!("class {short}\n")
        } else {
            format!("class {short} < {}\n", refname(rng))
        };
        let decl = format!("{head}{body}end\n");
        if nested {
            files[file].push_str(&format!("module Ns\n{decl}end\n"));
        } else {
            files[file].push_str(&decl);
        }
    }
    files.into_iter().map(|f| f.into_bytes()).collect()
}

/// Old-vs-new over randomized hierarchies. Every pair of every generated
/// project is graded against the pre-#94 walk.
#[test]
fn closure_matches_legacy_on_random_hierarchies() {
    let core = CoreIndex::new();
    let mut rng = Rng(0x0094_A9CE_5709_5EED);
    let (mut graded, mut related) = (0usize, 0usize);
    for round in 0..120 {
        let n = 4 + rng.below(14);
        let srcs = random_hierarchy(&mut rng, n);
        let idx = build(&srcs, &core);
        let (g, r) = grade(&idx, &format!("random#{round}"));
        graded += g;
        related += r;
    }
    println!("--- random: {graded} pairs graded, {related} related");
    assert!(
        related >= 200,
        "the generator must produce genuinely related pairs (got {related} of {graded})"
    );
}

/// THE CAP TEST — the boundary no corpus reaches. In the pre-#94 walk the
/// owner check ran on POP, before the `visited > OVERRIDE_ANCESTOR_WALK_LIMIT`
/// return, so the node that OVERFLOWS the cap still answers `true` while
/// everything past it answers `false`. Owner placed just inside the cap, AT
/// the overflow, and one step past it — old and new must agree at all three.
#[test]
fn closure_matches_legacy_at_the_walk_cap() {
    // C0 <- C1 <- ... <- C109; the walk starts at C109's ancestors, so C108
    // is visit 1 and C{109-i} is visit i.
    let mut src = String::from("class C0\nend\n");
    for i in 1..110 {
        src.push_str(&format!("class C{i} < C{}\nend\n", i - 1));
    }
    let idx = build(&[src.into_bytes()], &CoreIndex::new());
    let candidate = "C109";
    let limit = OVERRIDE_ANCESTOR_WALK_LIMIT; // 100
    let just_inside = format!("C{}", 109 - limit); // C9  — visit 100
    let at_boundary = format!("C{}", 108 - limit); // C8  — visit 101, overflows
    let just_past = format!("C{}", 107 - limit); // C7  — never popped

    let closure = idx.build_ancestor_closure(candidate);
    assert!(idx.related_to_owner(candidate, &just_inside), "legacy: inside the cap");
    assert!(idx.related_to_owner(candidate, &at_boundary), "legacy: the overflowing node");
    assert!(!idx.related_to_owner(candidate, &just_past), "legacy: past the cap");
    assert!(closure.contains(&just_inside), "closure: inside the cap");
    assert!(closure.contains(&at_boundary), "closure: the overflowing node");
    assert!(!closure.contains(&just_past), "closure: past the cap");
    // The cap really did fire: 100 visited nodes plus the overflowing one.
    assert_eq!(closure.len(), limit + 1);
    grade(&idx, "cap-chain");
}

/// The other half of the cap boundary: nodes still QUEUED when the cap fires
/// were never popped, so they were never owner-checkable. A 200-wide fan-out
/// leaves 99 modules in the queue behind the overflowing one.
#[test]
fn closure_matches_legacy_when_the_cap_abandons_a_queue() {
    let mut src = String::new();
    for i in 0..200 {
        src.push_str(&format!("module M{i}\nend\n"));
    }
    src.push_str("class R\n");
    for i in 0..200 {
        src.push_str(&format!("  include M{i}\n"));
    }
    src.push_str("end\n");
    let idx = build(&[src.into_bytes()], &CoreIndex::new());
    let limit = OVERRIDE_ANCESTOR_WALK_LIMIT; // 100
    let closure = idx.build_ancestor_closure("R");
    // M0..M99 are visits 1..100; M100 overflows the cap but is still popped
    // (⇒ owner-checkable); M101.. never leave the queue.
    assert!(idx.related_to_owner("R", &format!("M{}", limit - 1)));
    assert!(idx.related_to_owner("R", &format!("M{limit}")), "the overflowing node");
    assert!(!idx.related_to_owner("R", &format!("M{}", limit + 1)), "abandoned in the queue");
    assert!(closure.contains(&format!("M{}", limit - 1)));
    assert!(closure.contains(&format!("M{limit}")));
    assert!(!closure.contains(&format!("M{}", limit + 1)));
    assert_eq!(closure.len(), limit + 1);
    grade(&idx, "cap-fanout");
}
