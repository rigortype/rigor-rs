//! The user-facing type-display layer lives in `rigor_types::describe_named`
//! (the shared dependency, so the `check` receiver path in rigor-rules can use it
//! too). This module re-exports it under the crate's `type_display::describe`
//! name that `type-of` / `annotate` / `triage` call, and owns a resolver builder.

use rigor_index::CoreIndex;
use rigor_infer::SourceIndex;
use rigor_types::{ClassId, Interner, TypeId};

/// Render `ty` as the reference's `Type#describe(:short)` would, resolving class
/// ids through the core RBS index then the project `sig/` registry.
pub fn describe(interner: &Interner, index: &CoreIndex, source: &SourceIndex, ty: TypeId) -> String {
    let resolve = |class: ClassId| -> Option<String> {
        index
            .class_name_for_id(class)
            .map(str::to_string)
            .or_else(|| source.class_name_for_id(class).map(str::to_string))
    };
    rigor_types::describe_named(interner, ty, &resolve)
}
