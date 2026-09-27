use super::*;
use rigor_parse::{lower, parse};
use rigor_types::Interner;
use rigor_index::CoreIndex;

fn run(src: &[u8]) -> Vec<Diagnostic> {
    let ast = lower(&parse(src));
    let mut interner = Interner::new();
    let index = CoreIndex::new();
    analyze(&ast, &mut interner, &index)
}

/// MultiWrite substrate Slice 2 — the fixture-68 chain end to end:
/// `Process.wait2`'s RBS tuple return types per position, the Slice-1 binder
/// distributes it across the multi-write targets, and the ADR-0042 QUALIFIED
/// surface witnesses the typo on the RBS-only `Process::Status` class. Message
/// and position are byte-identical to the oracle
/// (`harness/snapshots/68_nested_stdlib_singleton.json`, 40:8).
#[test]
fn multi_write_tuple_slot_witnesses_on_a_nested_rbs_class() {
    let src = b"_pid, status = Process.wait2\nstatus.frobnicate\n";
    let diags = run(src);
    assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
    let d = &diags[0];
    assert_eq!(d.rule_id, CALL_UNDEFINED_METHOD);
    assert_eq!(d.message, "undefined method `frobnicate' for Process::Status");
    assert_eq!(d.receiver_type.as_deref(), Some("Process::Status"));
    assert_eq!(&src[d.start_offset..d.end_offset], b"frobnicate");
}

/// The other half of the witness: a REAL method on the same class is silent
/// (the qualified surface resolves `exited?`, and its ancestry is complete).
#[test]
fn a_real_method_on_the_tuple_slot_is_silent() {
    let diags = run(b"_pid, status = Process.wait2\nstatus.exited?\n");
    assert!(diags.is_empty(), "expected silence, got {diags:?}");
    // The FIRST slot is a plain core Integer and witnesses/stays silent the
    // usual way.
    assert!(run(b"pid, _s = Process.wait2\npid.succ\n").is_empty());
    assert_eq!(run(b"pid, _s = Process.wait2\npid.frobnicate\n").len(), 1);
}

/// FP REGRESSION (measured 2026-07-25, PROBE not corpus). The qualified
/// witness arm must NOT fire on a namespaced GEM class the SOURCE names:
/// rigor-rs's surface for those is knowingly weaker than the oracle's (the
/// reference's `data/vendored_gem_sigs/`, unvendored here). `segments` is a
/// real `Gem::Version` method declared only in `rubygems_extras.rbs`, so the
/// ORACLE IS SILENT on both lines below; an unrestricted arm fired on the
/// first. See `SourceIndex::is_declaration_only_class`.
#[test]
fn a_source_named_qualified_gem_class_stays_lenient() {
    assert!(
        run(b"g = Gem::Version.new(\"1.0\")\ng.segments\n").is_empty(),
        "an extras-only method on a source-named nested gem class must stay silent"
    );
    assert!(
        run(b"Gem::Version.new(\"1.0\").frobnicate\n").is_empty(),
        "and so must a typo on it (the FP-safe direction: the oracle fires, we do not)"
    );
    // The declaration-only twin still witnesses — the restriction is on
    // PROVENANCE, not on the qualified surface itself.
    assert_eq!(run(b"_pid, status = Process.wait2\nstatus.frobnicate\n").len(), 1);
}

/// An INSTANCE tuple return witnesses through the same mint
/// (`String#partition -> [String, String, String]`). Oracle-checked: the
/// reference fires at each of these positions too (it renders the pinned
/// values, rigor-rs the class — same rule/line/column).
#[test]
fn instance_tuple_return_slots_witness() {
    let diags = run(b"a, b, c = \"x-y\".partition(\"-\")\na.frobnicate\nb.upcase\nc.frobnicate\n");
    assert_eq!(diags.len(), 2, "expected two typo diagnostics, got {diags:?}");
    assert!(diags.iter().all(|d| d.message.contains("for String")));
}
