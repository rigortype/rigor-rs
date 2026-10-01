//! The shape-mutator name tables (`MutationWidening::SHAPE_MUTATORS`,
//! `reference/rigor/lib/rigor/inference/mutation_widening.rb:85-110` plus
//! `hash_lookup_mutation.rb:22` and `string_mutation.rb:25`), kept verbatim.
//!
//! They live in `rigor-parse` — not `rigor-infer` — because the
//! `OperandEffects.any?` gate (`ast::operand_effects`, rigor-rs#361) asks
//! "is this call a shape mutator on an outliving receiver?" at LOWERING
//! time, and `rigor-parse` cannot depend on `rigor-infer`.

/// `reference/rigor/lib/rigor/inference/mutation_widening.rb:85` verbatim.
pub const ARRAY_MUTATORS: &[&str] = &[
    "<<", "push", "append", "prepend", "unshift", "concat", "insert", "pop", "shift", "delete",
    "delete_at", "delete_if", "reject!", "clear", "compact!", "replace", "fill", "[]=", "map!",
    "collect!", "select!", "filter!", "keep_if", "uniq!", "flatten!", "sort!", "sort_by!",
    "reverse!", "rotate!", "shuffle!", "slice!",
];

/// `reference/rigor/lib/rigor/inference/mutation_widening.rb:99` verbatim.
///
/// `shift` since upstream `495a7458` (pin `e59b7b89`): a name both classes
/// define is listed in both tables, and its absence here let `k = { a: 1 };
/// k.shift` keep the literal shape of a hash that is empty at runtime.
pub const HASH_MUTATORS: &[&str] = &[
    "[]=", "store", "shift", "delete", "delete_if", "reject!", "select!", "filter!", "keep_if",
    "clear", "compact!", "merge!", "update", "transform_keys!", "transform_values!", "replace",
];

/// `reference/rigor/lib/rigor/inference/hash_lookup_mutation.rb:22` verbatim —
/// the Hash methods that change what a READ of the pairs answers without
/// changing the pair set. Upstream keeps them off [`HASH_MUTATORS`] on purpose
/// (`HashLookupMutation.widen_shape` opens a shape instead of widening it to a
/// nominal — issue #1280, not ported: a local's shape is left as-is here). Read
/// only through [`is_shape_mutator`], the constant-mutation census.
pub const HASH_LOOKUP_MUTATORS: &[&str] = &["default=", "default_proc=", "compare_by_identity"];

/// `reference/rigor/lib/rigor/inference/string_mutation.rb:25` verbatim — the
/// one String table (upstream `4a6b43f6`, pin `e59b7b89`, which grew it to 35
/// and retired the effect classifier's drifted copy). Upstream's
/// `StringMutation.widen_constant` widens a `Constant["ab"]` local to the bare
/// `String` nominal under any of these; the port widens the binding to
/// `Dynamic` instead, through `MUTATOR_METHODS` — strictly fewer
/// diagnostics, never more.
pub const STRING_MUTATORS: &[&str] = &[
    "<<", "concat", "insert", "prepend", "replace", "clear", "[]=", "slice!", "setbyte",
    "bytesplice", "append_as_bytes", "force_encoding", "sub!", "gsub!", "tr!", "tr_s!",
    "delete!", "squeeze!", "succ!", "next!", "upcase!", "downcase!", "capitalize!", "swapcase!",
    "reverse!", "strip!", "lstrip!", "rstrip!", "chomp!", "chop!", "delete_prefix!",
    "delete_suffix!", "encode!", "scrub!", "unicode_normalize!",
];

/// Upstream's `MutationWidening::SHAPE_MUTATORS` (`mutation_widening.rb:110`):
/// `ARRAY_MUTATORS | HASH_MUTATORS | HashLookupMutation::MUTATORS |
/// StringMutation::MUTATORS` — "could an in-place call on this name change its
/// binding?". The constant-mutation census (`scope_indexer.rb`
/// `mutating_receiver_of`) reads it since pin `e59b7b89`; before that it read
/// the Array and Hash tables alone, so `FOO = "ab"; FOO.upcase!` kept folding
/// `FOO == "ab"` on both sides.
pub fn is_shape_mutator(method: &str) -> bool {
    ARRAY_MUTATORS.contains(&method)
        || HASH_MUTATORS.contains(&method)
        || HASH_LOOKUP_MUTATORS.contains(&method)
        || STRING_MUTATORS.contains(&method)
}
