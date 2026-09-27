use super::*;
use rigor_parse::{lower, parse};

fn new_ty(src: &[u8]) -> String {
    let ast = lower(&parse(src));
    let index = CoreIndex::new();
    let source = SourceIndex::build(&ast, &index);
    let typer = Typer::with_source(&index, &source);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let ty = *env.get("v").expect("v bound");
    rigor_types::describe(&i, ty)
}

/// The reference `meta_new` constant-constructor lifts (Pathname pinned-Str,
/// Date/DateTime all-pinned, Set empty/pinned-Tuple) produce pinned VALUE
/// carriers rigor-rs does not model — the mint declines (Dynamic). Every
/// other singleton `.new` mints a witnessable instance, matching the
/// reference's `nominal_of` fallback (probed live on all of these shapes).
#[test]
fn curated_constructor_lifts_decline_and_others_mint() {
    // Lift shapes -> decline (Dynamic).
    assert_eq!(new_ty(b"v = Pathname.new(\"x\")\n"), "Dynamic[top]");
    assert_eq!(new_ty(b"v = Date.new(2020)\n"), "Dynamic[top]");
    assert_eq!(new_ty(b"v = Set.new\n"), "Dynamic[top]");
    assert_eq!(new_ty(b"v = Set.new([1, 2])\n"), "Dynamic[top]");
    // Non-lift shapes -> minted instance (source-range Nominal renders
    // Class<1000000+>).
    assert!(new_ty(b"v = Pathname.new(:sym)\n").starts_with("Class<"));
    assert!(new_ty(b"def f(x)\n  $g = Pathname.new(x)\nend\nv = Time.new\n").starts_with("Class<"));
    assert!(new_ty(b"v = StringIO.new\n").starts_with("Class<"));
}
