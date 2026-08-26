//! The three by-reference mutator sets a catalogued class may name
//! (`mutators: array | hash | string`) — ADR-0043 slice 2.
//!
//! Upstream's `Catalog::MUTATOR_SETS` (`catalog.rb:43`) resolves the NAME to a
//! Ruby `%i[…]` literal it keeps beside the widening rules, so the effect model
//! and `Inference::MutationWidening` can never disagree about what mutates an
//! `Array`. `core.yml` deliberately does not re-spell the lists — upstream's
//! internal spec makes that normative — so the port cannot get them by copying
//! a data file. `harness/vendor_effects.py` EXTRACTS the three literals from the
//! pinned Ruby into `vendor/effects/mutators.yml`, and that file is graded by
//! the same `--check` drift gate as the two verbatim copies.
//!
//! Slice 1 carried the set NAME only, so a posture-path answer under-claimed
//! `mutates_receiver`. Slice 2 expands the name, because the expansion decides
//! a PROVEN `mutate.*` label on two paths: an instance ROW whose selector is in
//! its class's set (`catalog.rb:253`'s `in_mutator_set`), and the posture entry
//! for a selector in the set (`catalog.rb:194`).

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::Error;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMutators {
    #[serde(default)]
    schema: u32,
    #[serde(default)]
    sets: BTreeMap<String, RawSet>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSet {
    /// Where the literal was lifted from, for a reader who wonders why this
    /// file is derived rather than copied. Carried, never consulted.
    #[serde(default)]
    from: String,
    #[serde(default)]
    selectors: Vec<String>,
}

/// The resolved sets, by the name `core.yml`'s `mutators:` spells.
#[derive(Debug)]
pub struct Mutators {
    schema: u32,
    sets: BTreeMap<String, BTreeSet<String>>,
}

/// The empty set — what a class with no `mutators:` key answers, so the lookup
/// never needs an `Option` on the hot path.
static NO_MUTATORS: BTreeSet<String> = BTreeSet::new();

impl Mutators {
    /// Build from mutator-set-shaped YAML.
    ///
    /// # Errors
    ///
    /// A malformed document, or a set the catalogue has no name for.
    pub fn from_yaml_str(source: &str) -> Result<Self, Error> {
        let raw: RawMutators = serde_yaml::from_str(source)?;
        let mut sets = BTreeMap::new();
        for (name, body) in raw.sets {
            if !crate::catalog::MUTATOR_SETS.contains(&name.as_str()) {
                return Err(Error::UnknownMutatorSet {
                    class: "vendor/effects/mutators.yml".to_string(),
                    set: name,
                });
            }
            let _ = body.from;
            sets.insert(name, body.selectors.into_iter().collect());
        }
        Ok(Self { schema: raw.schema, sets })
    }

    /// The data file's `schema:`.
    #[must_use]
    pub fn schema(&self) -> u32 {
        self.schema
    }

    /// The selectors of the named set, or the empty set for a name nothing
    /// declares. A class with no `mutators:` key is the empty-set case.
    #[must_use]
    pub fn set(&self, name: Option<&str>) -> &BTreeSet<String> {
        name.and_then(|name| self.sets.get(name)).unwrap_or(&NO_MUTATORS)
    }

    /// Every declared set name, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.sets.keys().map(String::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::mutators;

    #[test]
    fn the_three_sets_are_the_sizes_the_probe_measured() {
        // Measured through the PINNED Ruby loader
        // (`docs/notes/20260826-effects-s2-probe.md` § 8):
        //   ARRAY=31 HASH=15 STRING=26
        // A set that changes size under a re-pin is a semantic change to what
        // counts as a receiver mutation; `harness/vendor_effects.py` refuses to
        // write one silently and this fails without the submodule populated.
        assert_eq!(mutators().set(Some("array")).len(), 31);
        assert_eq!(mutators().set(Some("hash")).len(), 15);
        assert_eq!(mutators().set(Some("string")).len(), 26);
        assert_eq!(mutators().names(), ["array", "hash", "string"]);
        assert_eq!(mutators().schema(), 1);
    }

    #[test]
    fn an_absent_or_unknown_name_answers_the_empty_set() {
        assert!(mutators().set(None).is_empty());
        assert!(mutators().set(Some("nope")).is_empty());
    }

    #[test]
    fn the_selectors_that_decide_a_mutation_are_present() {
        // The three the probe's § 4e names, and the universal one that is a
        // member of ALL of them — `[]=` spells a balanced `[` `]` inside the
        // Ruby literal, so an extractor that stopped at the first `]` would
        // truncate every set.
        assert!(mutators().set(Some("array")).contains("push"));
        assert!(mutators().set(Some("array")).contains("<<"));
        assert!(mutators().set(Some("hash")).contains("store"));
        assert!(mutators().set(Some("string")).contains("upcase!"));
        for name in ["array", "hash", "string"] {
            assert!(mutators().set(Some(name)).contains("[]="), "{name} lost `[]=`");
        }
        // …and the non-mutating siblings stay out (`map` vs `map!`).
        assert!(!mutators().set(Some("array")).contains("map"));
        assert!(!mutators().set(Some("string")).contains("upcase"));
    }
}
