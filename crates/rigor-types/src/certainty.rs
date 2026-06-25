//! Trinary certainty and the relational result type.
//!
//! Grounded in `docs/type-specification/relations-and-certainty.md`. The core
//! discipline is that `Maybe` is sticky: it MUST NOT be promoted to `Yes` by
//! repetition or count, and a `Maybe` result MUST NOT be treated as `Yes` for
//! narrowing nor as `No` for the complementary false-edge fact.

/// Trinary certainty result for type, reflection, role-conformance, and
/// member-availability queries.
///
/// - `Yes`   — proven under the current evidence base.
/// - `No`    — disproven under the same evidence base.
/// - `Maybe` — every other case (cannot prove either side, dynamic behavior,
///   uncertain plugin fact, or an exhausted inference budget).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Certainty {
    Yes,
    No,
    Maybe,
}

impl Certainty {
    /// Logical conjunction with the `Maybe`-sticky discipline.
    ///
    /// `No` dominates (a single disproof disproves the conjunction); `Maybe`
    /// absorbs `Yes` (uncertainty is never promoted away by a proven sibling).
    /// Only `Yes & Yes` is `Yes`.
    pub fn and(self, other: Certainty) -> Certainty {
        match (self, other) {
            (Certainty::No, _) | (_, Certainty::No) => Certainty::No,
            (Certainty::Maybe, _) | (_, Certainty::Maybe) => Certainty::Maybe,
            (Certainty::Yes, Certainty::Yes) => Certainty::Yes,
        }
    }

    /// Logical disjunction with the `Maybe`-sticky discipline.
    ///
    /// `Yes` dominates; `Maybe` absorbs `No`. Note that two `Maybe`s remain
    /// `Maybe`: uncertainty is NOT promoted to `Yes` by accumulation, per
    /// relations-and-certainty.md ("Repeated `maybe` evidence remains `maybe`").
    pub fn or(self, other: Certainty) -> Certainty {
        match (self, other) {
            (Certainty::Yes, _) | (_, Certainty::Yes) => Certainty::Yes,
            (Certainty::Maybe, _) | (_, Certainty::Maybe) => Certainty::Maybe,
            (Certainty::No, Certainty::No) => Certainty::No,
        }
    }

    /// Negate a certainty. `Yes`/`No` flip; `Maybe` is its own negation.
    ///
    /// NOTE: this is the relational negation of a *proven* answer. It MUST NOT
    /// be used to mint the complementary false-edge fact from a `Maybe`.
    pub fn negate(self) -> Certainty {
        match self {
            Certainty::Yes => Certainty::No,
            Certainty::No => Certainty::Yes,
            Certainty::Maybe => Certainty::Maybe,
        }
    }

    /// True only when the answer is proven (`Yes`). A `Maybe` is never proven.
    pub fn is_proven(self) -> bool {
        matches!(self, Certainty::Yes)
    }
}

/// Why a [`Relation`] carries the certainty it does. Keeps the relational
/// `Maybe` (genuinely cannot prove either side) distinct from a budget cutoff
/// (analyzer stopped early), per relations-and-certainty.md §"`maybe` is
/// distinct from incomplete inference".
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Evidence {
    /// Decided by value-set inclusion (subtyping).
    Subtype,
    /// Decided by gradual consistency across a dynamic boundary.
    Consistency,
    /// An inference budget (recursion depth, call-graph width, ...) was
    /// exhausted; the `Maybe` is a cutoff artifact, not a true relational
    /// `Maybe`. The diagnostic MUST surface the cutoff rather than hide it.
    BudgetCutoff,
    /// No evidence basis recorded yet (skeleton default).
    Unknown,
}

/// A relational query result: a [`Certainty`] paired with the [`Evidence`] that
/// produced it. Keeping the evidence attached is what lets callers tell a
/// relational `Maybe` apart from a `BudgetCutoff`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Relation {
    pub certainty: Certainty,
    pub evidence: Evidence,
}

impl Relation {
    pub fn new(certainty: Certainty, evidence: Evidence) -> Self {
        Relation { certainty, evidence }
    }

    /// Convenience: a proven `Yes` with the given evidence.
    pub fn yes(evidence: Evidence) -> Self {
        Relation::new(Certainty::Yes, evidence)
    }

    /// Convenience: a proven `No` with the given evidence.
    pub fn no(evidence: Evidence) -> Self {
        Relation::new(Certainty::No, evidence)
    }

    /// Convenience: a relational `Maybe` with the given evidence.
    pub fn maybe(evidence: Evidence) -> Self {
        Relation::new(Certainty::Maybe, evidence)
    }

    /// Convenience: a `Maybe` caused by an exhausted inference budget. Distinct
    /// from a relational `Maybe` by its [`Evidence::BudgetCutoff`].
    pub fn budget_cutoff() -> Self {
        Relation::new(Certainty::Maybe, Evidence::BudgetCutoff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maybe_never_promotes_to_yes_via_or() {
        // Repeated `maybe` evidence remains `maybe`; uncertainty is not
        // promoted to `yes` by count (relations-and-certainty.md).
        let acc = Certainty::Maybe
            .or(Certainty::Maybe)
            .or(Certainty::Maybe)
            .or(Certainty::No);
        assert_eq!(acc, Certainty::Maybe);
    }

    #[test]
    fn yes_dominates_or_but_maybe_absorbs_yes_in_and() {
        assert_eq!(Certainty::Maybe.or(Certainty::Yes), Certainty::Yes);
        assert_eq!(Certainty::Maybe.and(Certainty::Yes), Certainty::Maybe);
    }

    #[test]
    fn no_dominates_and() {
        assert_eq!(Certainty::No.and(Certainty::Yes), Certainty::No);
        assert_eq!(Certainty::Yes.and(Certainty::No), Certainty::No);
    }

    #[test]
    fn maybe_is_self_negation() {
        assert_eq!(Certainty::Maybe.negate(), Certainty::Maybe);
        assert_eq!(Certainty::Yes.negate(), Certainty::No);
        assert_eq!(Certainty::No.negate(), Certainty::Yes);
    }

    #[test]
    fn budget_cutoff_is_distinct_from_relational_maybe() {
        let relational = Relation::maybe(Evidence::Subtype);
        let cutoff = Relation::budget_cutoff();
        assert_eq!(relational.certainty, cutoff.certainty); // both Maybe
        assert_ne!(relational.evidence, cutoff.evidence); // but distinguishable
        assert_eq!(cutoff.evidence, Evidence::BudgetCutoff);
    }
}
