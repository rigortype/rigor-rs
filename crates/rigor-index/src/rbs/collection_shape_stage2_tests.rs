use super::*;

/// Stage 2a/2c — the BLOCK-FREE return slot. `String#split` and `Dir.glob`
/// each declare a block overload whose return DIVERGES from the block-free
/// one (`-> self` / `-> nil` vs `-> Array[String]`), so the flat
/// all-overloads-agree slot collapses to `None`; the block-free slot answers
/// `Array` for exactly those.
#[test]
fn block_free_return_answers_where_the_flat_slot_collapses() {
    let idx = CoreData::load();
    if !idx.knows_class("String") || !idx.knows_class("Dir") {
        return;
    }
    // Instance side (probe c08b's chain root).
    assert_eq!(idx.method_return("String", "split"), None);
    assert_eq!(idx.method_return_block_free("String", "split"), Some("Array"));
    // Singleton side (probe c02's chain root).
    assert_eq!(idx.singleton_method_return("Dir", "glob"), None);
    assert_eq!(idx.singleton_method_return_block_free("Dir", "glob"), Some("Array"));
}

/// The slot is EMPTY for a method with no block overload — the flat return
/// is the only answer there, so the new path adds nothing and cannot change
/// an existing result. `Dir.[]` (probe c01's root, single overload) and
/// `String#upcase` are the controls.
#[test]
fn block_free_return_is_absent_without_a_block_overload() {
    let idx = CoreData::load();
    if !idx.knows_class("String") || !idx.knows_class("Dir") {
        return;
    }
    assert_eq!(idx.singleton_method_return("Dir", "[]"), Some("Array"));
    assert_eq!(idx.singleton_method_return_block_free("Dir", "[]"), None);
    assert_eq!(idx.method_return("String", "upcase"), Some("String"));
    assert_eq!(idx.method_return_block_free("String", "upcase"), None);
    // An unknown class / method declines on both sides.
    assert_eq!(idx.method_return_block_free("NoSuchClassZZZ", "split"), None);
    assert_eq!(idx.method_return_block_free("String", "spilt"), None);
    assert_eq!(idx.singleton_method_return_block_free("NoSuchClassZZZ", "glob"), None);
}

/// The DECLINE control: block-free overloads that DISAGREE on the return
/// leave the slot empty, exactly like the flat slot's all-overloads-agree
/// collapse — we never pick one of two divergent block-free returns. The
/// agreeing twin on the same class is the positive control.
#[test]
fn divergent_block_free_overloads_decline() {
    let dir = std::env::temp_dir().join("rigor_block_free_decline_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("z.rbs"),
        concat!(
            "class ZzBlockFree\n",
            // Two block-free overloads that DISAGREE (String vs Integer) plus
            // a block overload: must decline.
            "  def diverge: (String) -> String\n",
            "             | (Integer) -> Integer\n",
            "             | (String) { (String) -> void } -> nil\n",
            // Two block-free overloads that AGREE plus a block overload whose
            // return diverges: the slot answers, the flat slot does not.
            "  def agree: (String) -> Array[String]\n",
            "           | (Integer) -> Array[String]\n",
            "           | (String) { (String) -> void } -> nil\n",
            // A non-concrete block-free return declines.
            "  def void_ret: (String) -> void\n",
            "              | (String) { (String) -> void } -> nil\n",
            "  def self.sdiverge: (String) -> String\n",
            "                   | (Integer) -> Integer\n",
            "                   | (String) { (String) -> void } -> nil\n",
            "  def self.sagree: (String) -> Array[String]\n",
            "                 | (Integer) -> Array[String]\n",
            "                 | (String) { (String) -> void } -> nil\n",
            "end\n",
        ),
    )
    .unwrap();
    let idx = CoreData::load_for_project(&[], std::slice::from_ref(&dir));
    // Flat slot collapses for every one of them (a block overload diverges).
    assert_eq!(idx.method_return("ZzBlockFree", "diverge"), None);
    assert_eq!(idx.method_return("ZzBlockFree", "agree"), None);
    assert_eq!(idx.singleton_method_return("ZzBlockFree", "sdiverge"), None);
    assert_eq!(idx.singleton_method_return("ZzBlockFree", "sagree"), None);
    // The block-free slot answers ONLY where the block-free overloads agree.
    assert_eq!(idx.method_return_block_free("ZzBlockFree", "diverge"), None);
    assert_eq!(idx.method_return_block_free("ZzBlockFree", "agree"), Some("Array"));
    assert_eq!(idx.method_return_block_free("ZzBlockFree", "void_ret"), None);
    assert_eq!(idx.singleton_method_return_block_free("ZzBlockFree", "sdiverge"), None);
    assert_eq!(
        idx.singleton_method_return_block_free("ZzBlockFree", "sagree"),
        Some("Array")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Stage 2b — TOP-LEVEL RBS object-constant declarations are ingested, and
/// only the ones whose declared type is a bare class instance.
#[test]
fn rbs_object_constants_are_ingested() {
    let idx = CoreData::load();
    if !idx.knows_class("String") {
        return;
    }
    // `type_name_str` yields the LEAF name, the same short key every other
    // class table uses — `RBS::Unnamed::ENVClass` is registered as
    // `ENVClass` there, and that is what resolves `keys`.
    assert_eq!(idx.object_constant_class("ENV"), Some("ENVClass"));
    // A generic application keeps its constructor name (the same erasure
    // every other return slot performs).
    assert_eq!(idx.object_constant_class("ARGV"), Some("Array"));
    assert_eq!(idx.object_constant_class("STDOUT"), Some("IO"));
    // `CROSS_COMPILING: true?` is not a bare class instance ⇒ not recorded.
    assert_eq!(idx.object_constant_class("CROSS_COMPILING"), None);
    assert_eq!(idx.object_constant_class("NoSuchConstantZZZ"), None);
    // And the declared class resolves so `ENV.keys` can type (probe c03).
    assert_eq!(idx.method_return("ENVClass", "keys"), Some("Array"));
}
