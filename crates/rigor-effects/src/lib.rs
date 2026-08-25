//! The vendored effect catalogue — ADR-0043 slice 1.
//!
//! Upstream separates the **label taxonomy** from the **per-method
//! catalogue**, and they are two hand-written data files with two loaders. Both
//! are vendored VERBATIM under `vendor/effects/` and embedded here with
//! [`include_str!`], the `vendor/plugins/` precedent — no `build.rs`, no
//! codegen step, and the runtime bytes stay byte-comparable to upstream's.
//!
//! # This crate has no consumer, on purpose
//!
//! ADR-0043 § 1 binds the effects arc to "the effects work may not change
//! `crates/rigor-infer`'s answers". Nothing in `rigor-infer` / `rigor-rules` /
//! `rigor-cli` depends on this crate, so that constraint is a
//! dependency-graph FACT rather than a review promise. When a later slice
//! genuinely needs it, that edge becomes a visible, arguable manifest change.
//!
//! Both files are parsed LAZILY, on first use ([`registry`] / [`catalog`]),
//! exactly as upstream memoises `Registry.default` / `Catalog.default`. Nothing
//! pays for the parse until an effects surface exists.
//!
//! # What is here, and what is deliberately not
//!
//! Shipped: [`label`]'s grammar and segment-aware subsumption,
//! [`Registry`]'s declared-∪-ancestors `known?`, and [`Catalog`]'s parse plus
//! the row → universal → posture lookup PRECEDENCE.
//!
//! Carved out — these are slice-2 CODE, not data, and the data file
//! deliberately does not re-spell them (see `vendor/effects/PROVENANCE.md`):
//!
//! 1. **Mutator sets.** A class names `mutators: array | hash | string` and
//!    upstream resolves the name to `MutationWidening::ARRAY_MUTATORS` (31) /
//!    `HASH_MUTATORS` (15) / `MutationClassifier::STRING_MUTATORS` (26). The
//!    name stays OPAQUE here ([`ClassEntry::mutator_set`]), so a posture-path
//!    answer reports `mutates_receiver == false` where upstream would consult
//!    the set. That is an UNDER-claim, the safe direction under ADR-0043 § 2,
//!    and [`catalog::tests::posture_path_does_not_expand_the_mutator_set`]
//!    pins it so slice 2 has to change it deliberately.
//! 2. **Narrowing handler BODIES.** A row's `narrow:` names one of the seven
//!    handlers in upstream's `Effects::Narrowing::HANDLERS`; the name is
//!    validated against [`catalog::NARROWING_HANDLERS`] and otherwise stays
//!    opaque. [`Catalog::lookup`] therefore answers a narrowed row's
//!    **unnarrowed** `effects:` — which is exactly upstream's own answer when
//!    it is handed no call node, and is the sound upper bound the handler
//!    degrades to.
//! 3. **The plugin effect layer**, which ADR-0043 puts out of scope entirely.
//!
//! Also deferred: `LabelSet`'s lattice (`TOP`, `join`, `admits?`,
//! `excluding_subsumed_by`) and `Registry#with`'s root ownership. Both are
//! summary-lane machinery for slices 2-6; slice 1 carries only the
//! normalisation `LabelSet.new` performs, as a sorted, de-duplicated, grammar-
//! checked `Vec<String>`.

use std::fmt;
use std::sync::LazyLock;

pub mod catalog;
mod digest;
pub mod label;
pub mod registry;

pub use catalog::{Catalog, ClassEntry, Entry, Row};
pub use registry::Registry;

// ---------------------------------------------------------------------------
// The vendored bytes, and their provenance anchors.
// ---------------------------------------------------------------------------

/// The reference pin these bytes were taken at. Moves only with
/// `UPSTREAM.md`'s pin, via `harness/vendor_effects.py`.
pub const PIN: &str = "v0.3.4 (b10bd5df)";

/// `data/effects/registry.yml`, verbatim.
pub const REGISTRY_YML: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/vendor/effects/registry.yml"));

/// `data/effects/core.yml`, verbatim.
pub const CORE_YML: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/vendor/effects/core.yml"));

/// `sha256(registry.yml)` at [`PIN`], as recorded in `PROVENANCE.md`.
pub const REGISTRY_SHA256: &str =
    "bb0eb3f08568bc52c47ce3caa75d22d359b0455b3182825906884797289d7104";

/// `sha256(core.yml)` at [`PIN`], as recorded in `PROVENANCE.md`.
///
/// This is the same number upstream uses as the catalogue's own cache identity
/// (`Catalog#identity` is `schema:sha256(core.yml)`, `catalog.rb:158`), so the
/// provenance anchor and upstream's invalidation key are one value — see
/// [`Catalog::identity`].
pub const CORE_SHA256: &str =
    "85778dd3433fcb5561a933c9b2b22fb07048af980e35f93091f545655bda9c31";

static REGISTRY: LazyLock<Registry> = LazyLock::new(|| {
    Registry::from_yaml_str(REGISTRY_YML).expect("vendored registry.yml must parse")
});

static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    Catalog::from_yaml_str(CORE_YML).expect("vendored core.yml must parse")
});

/// The shipped label vocabulary. Parsed on first use.
#[must_use]
pub fn registry() -> &'static Registry {
    &REGISTRY
}

/// The shipped effect catalogue. Parsed on first use.
#[must_use]
pub fn catalog() -> &'static Catalog {
    &CATALOG
}

/// SHA-256 of `data`, lowercase hex — how `PROVENANCE.md`'s anchors and
/// [`Catalog::identity`] are spelt.
#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    digest::sha256_hex(data)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a catalogue-shaped or registry-shaped document did not load.
///
/// Upstream raises for each of these at load time rather than at call time
/// (`Catalog::Error`, `registry.rb`'s `InvalidLabelError`): a data file that IS
/// present but malformed is a packaging bug, not a choice.
#[derive(Debug)]
pub enum Error {
    /// The document is not parseable as the shape the loader expects.
    Yaml(serde_yaml::Error),
    /// A label that is not well-formed under [`label::valid`]. Upstream's
    /// `LabelSet.new` raises `ArgumentError` here.
    InvalidLabel { context: String, label: String },
    /// A class names a `posture:` the `defaults:` table does not declare.
    UnknownPosture { class: String, posture: String },
    /// A class names a `mutators:` set the loader has no name for.
    UnknownMutatorSet { class: String, set: String },
    /// A row names a `narrow:` handler upstream's `Narrowing` does not
    /// implement.
    UnknownNarrowingHandler { key: String, handler: String },
    /// A row with no `why:` justification. Every row of the data file is an
    /// audit decision, and upstream refuses one that does not say why.
    MissingWhy { key: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Yaml(error) => write!(f, "effect data does not parse: {error}"),
            Self::InvalidLabel { context, label } => {
                write!(f, "{context}: not a well-formed effect label: {label:?}")
            }
            Self::UnknownPosture { class, posture } => write!(
                f,
                "{class}: unknown posture {posture:?}; add it to the catalogue's defaults:"
            ),
            Self::UnknownMutatorSet { class, set } => {
                write!(f, "{class}: unknown mutator set {set:?}")
            }
            Self::UnknownNarrowingHandler { key, handler } => {
                write!(f, "{key}: unknown narrowing handler {handler:?}")
            }
            Self::MissingWhy { key } => {
                write!(f, "{key}: every catalogue row needs a `why:` justification")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Yaml(error) => Some(error),
            _ => None,
        }
    }
}

impl From<serde_yaml::Error> for Error {
    fn from(error: serde_yaml::Error) -> Self {
        Self::Yaml(error)
    }
}

// ---------------------------------------------------------------------------
// The third drift layer (the mini-spec's gate 3): a hand-edit of the vendored
// bytes fails `cargo test` alone, without the submodule populated. The other
// two layers are `harness/vendor_effects.py --check` (which needs the pin) and
// the ported upstream data specs (`tests/upstream_data_specs.rs`).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn the_embedded_bytes_match_the_provenance_digests() {
        assert_eq!(
            sha256_hex(REGISTRY_YML.as_bytes()),
            REGISTRY_SHA256,
            "vendor/effects/registry.yml has been edited — it is a VERBATIM copy of \
             the pinned reference's data/effects/registry.yml; re-run \
             `python3 harness/vendor_effects.py` instead of hand-editing it"
        );
        assert_eq!(
            sha256_hex(CORE_YML.as_bytes()),
            CORE_SHA256,
            "vendor/effects/core.yml has been edited — it is a VERBATIM copy of the \
             pinned reference's data/effects/core.yml; re-run \
             `python3 harness/vendor_effects.py` instead of hand-editing it"
        );
    }

    #[test]
    fn the_catalogue_identity_is_upstreams_own() {
        // `Catalog.default.identity` at the pin, measured against the pinned
        // Ruby loader in the slice-1 probe § 1c.
        assert_eq!(
            catalog().identity(),
            "1:85778dd3433fcb5561a933c9b2b22fb07048af980e35f93091f545655bda9c31"
        );
    }
}
