use super::*;
use rigor_parse::{lower, parse, Node};

/// The rendered type of the LAST receiver-bearing call in `src`.
fn last_call_ty(src: &[u8]) -> String {
    let ast = lower(&parse(src));
    let index = CoreIndex::new();
    let source = SourceIndex::build(&ast, &index);
    let typer = Typer::with_source(&index, &source);
    let mut i = Interner::new();
    let env = TypeEnv::new();
    let call_id = ast
        .iter()
        .filter_map(|(id, n)| matches!(n, Node::Call { receiver: Some(_), .. }).then_some(id))
        .last()
        .unwrap();
    let ty = typer.type_of(&ast, call_id, &env, &mut i);
    rigor_types::describe(&i, ty)
}

/// MultiWrite substrate Slice 2: a SINGLETON RBS tuple return types
/// per-position (`Process.wait2 : [Integer, Process::Status]`) instead of
/// collapsing to `Dynamic[top]`. `Integer` is a core id (`Class<1>`); the
/// RBS-only `Process::Status` carries a source-registry id (`Class<1_000_xxx>`),
/// minted by `SourceIndex`'s tuple-element pre-registration — no source file
/// here names `Process::Status`.
#[test]
fn singleton_tuple_return_types_per_position() {
    let rendered = last_call_ty(b"_pid, status = Process.wait2\n");
    assert!(
        rendered.starts_with("Tuple[Class<1>, Class<"),
        "expected a 2-element tuple, got {rendered}"
    );
}

/// The instance twin (`String#partition -> [String, String, String]`), and
/// the element ids are the CORE String id.
#[test]
fn instance_tuple_return_types_per_position() {
    assert_eq!(
        last_call_ty(b"x = \"a-b\".partition(\"-\")\n"),
        "Tuple[Class<0>, Class<0>, Class<0>]"
    );
}

/// The Slice-1 binder distributes the tuple across the multi-write targets,
/// so `status` binds to the `Process::Status` nominal — the link fixture 68
/// needs. Compared against the SAME id the typer mints for the tuple slot.
#[test]
fn multi_write_binds_a_tuple_slot_to_its_rbs_class() {
    let ast = lower(&parse(b"_pid, status = Process.wait2\nstatus\n"));
    let index = CoreIndex::new();
    let source = SourceIndex::build(&ast, &index);
    let typer = Typer::with_source(&index, &source);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let status = *env.get("status").expect("status must be bound");
    let name = source.class_name_for_id_of(&i, status);
    assert_eq!(name, Some("Process::Status"), "got {:?}", i.get(status));
}

/// An RBS return this descriptor does not model stays exactly as before
/// (`Dynamic[top]`): `IO.pipe`'s overloads disagree (a block overload
/// returns the block's value), so the all-overloads-agree collapse declines.
#[test]
fn divergent_overloads_stay_dynamic() {
    assert_eq!(last_call_ty(b"r, w = IO.pipe\n"), "Dynamic[top]");
}

/// Upstream #121: an array of statically known values kept its precision
/// through concatenation and slicing but lost it at a set operation. Each
/// result below is the answer real Ruby gives for the same expression.
#[test]
fn tuple_set_operations_fold() {
    assert_eq!(last_call_ty(b"[1, 2] & [2]\n"), "Tuple[Constant[2]]");
    assert_eq!(
        last_call_ty(b"[1] | [2]\n"),
        "Tuple[Constant[1], Constant[2]]"
    );
    // `-` does NOT de-duplicate what survives: `[1, 1, 2] - [2] == [1, 1]`.
    assert_eq!(
        last_call_ty(b"[1, 1, 2] - [2]\n"),
        "Tuple[Constant[1], Constant[1]]"
    );
    // The named spellings take several arguments, reduced left to right.
    assert_eq!(
        last_call_ty(b"[1, 2].intersection([2, 3], [2])\n"),
        "Tuple[Constant[2]]"
    );
    assert_eq!(last_call_ty(b"[1, 2].intersect?([3])\n"), "Constant[false]");
    assert_eq!(last_call_ty(b"[1, 2].intersect?([2])\n"), "Constant[true]");
}

/// Membership is `eql?`, not `==` — `[1] & [1.0]` is EMPTY at runtime even
/// though `1 == 1.0`. Getting this wrong is the whole reason upstream ran
/// Ruby's own operator instead of reimplementing membership.
#[test]
fn tuple_set_operations_use_eql_not_equality() {
    assert_eq!(last_call_ty(b"[1] & [1.0]\n"), "Tuple[]");
    // Both survive the union because they are not `eql?` — the rendering
    // of `Constant[1.0]` as `1` is `describe`'s float formatting, not a
    // collapse (the intersection above proves they are distinct values).
    assert_eq!(
        last_call_ty(b"[1] | [1.0]\n"),
        "Tuple[Constant[1], Constant[1]]"
    );
}

/// `one?` (no block) counts TRUTHY elements, so `nil` / `false` do not
/// count; `at` folds an in-range constant index.
#[test]
fn tuple_one_and_at_fold() {
    assert_eq!(last_call_ty(b"[nil, false, 3].one?\n"), "Constant[true]");
    assert_eq!(last_call_ty(b"[nil, 2, 3].one?\n"), "Constant[false]");
    assert_eq!(last_call_ty(b"[1, 2, 3].at(1)\n"), "Constant[2]");
    assert_eq!(last_call_ty(b"[1, 2, 3].at(-1)\n"), "Constant[3]");
}

/// The declines. An out-of-range `at` does NOT fold to nil — proving nil on
/// a receiver the RBS tier calls optional would newly SURFACE diagnostics,
/// a different decision from removing a Dynamic. An argument that is not a
/// pinned Tuple, or an element that is not pinned, leaves the RBS tier to
/// widen.
#[test]
fn tuple_set_operations_decline_when_undecidable() {
    assert_eq!(last_call_ty(b"[1, 2, 3].at(9)\n"), "Dynamic[top]");
    // `Class<4>` is core `Array` — the RBS tier's widened answer.
    assert_eq!(last_call_ty(b"[1, 2] & unknown_thing\n"), "Class<4>");
    assert_eq!(last_call_ty(b"[1, unknown_thing] & [1]\n"), "Class<4>");
    // No argument at all is not a set operation; the RBS tier answers.
    assert_eq!(last_call_ty(b"[1, 2].intersection\n"), "Class<4>");
}
