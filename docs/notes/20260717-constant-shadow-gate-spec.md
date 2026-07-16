# Binding spec — precise constant-shadow gate (C1) + default-arg checking (C2) + const-literal harvest (C5)

From the 2026-07-17 gitlab-foss lib UM/PN gap classification (525 gaps sampled
50+40, full-pool discriminator run). UM pool: C1 132 MEASURED / C2 ~25 / C5
~24 bounded; C3a (~42, Module#name→String + optional-unwrap tier-3) follow-on.
PN pool ~75% Tier B/C substrate-blocked as predicted (P2 straight-line
optional-local slice ~15-20 borderline, deferred).

## C1 — precise constant-shadow gate (close ≈ 125-132)

Cause: `ConstantRead` suppression (rigor-infer lib.rs:263-273) uses
`!source.knows_class(bare_name)` where SourceIndex::add_source registers the
BARE written name of every class/module project-wide (source_index.rs:237-247,
356). A nested `module Time` (Gitlab::Database::Partitioning::Time),
`DateTime`, `File` (...::External::File) in the corpus kills ALL
Time/DateTime/File singleton witnessing batch-wide. Verified: same file fires
alone, silent in batch. Reference resolves lexically.

Fix: suppress iff project defines the name AT TOPLEVEL, or a nested
`...::name` is visible from the use site's lexical prefix chain (or harvested
ancestor namespace). Ambiguous → Dynamic (today's behavior). Pass-1b already
computes qualified names for the override index; thread the use-site lexical
prefix to the typer (precompute per-node prefix at lowering or via span
containment as Pass 1b does).

FP risk: none by construction — strict relaxation of an over-broad
suppression; every new firing is reference-confirmed. Genuinely-shadowed uses
(inside Gitlab::Database::Partitioning::*) must STAY silent via the
visibility check — test exactly that.

## C2 — parameter default-value checking (close ≈ 25; 7 overlap C1)

`def f(t = Time.current)` / kwarg defaults are never walked by the rules
passes (probe p4). Include default-value expressions in the lowered body
statement walk (reference checks them). Same rule machinery, new positions.

## C5 — const-literal harvest (close ≈ 24)

`R = 1..1024; R.exclude?` etc: harvest `CONST = <literal>` (scalar / Tuple /
HashShape / range) in build_project; ConstantRead consults it BEFORE the
singleton gate. Gate: single assignment project-wide, fully-literal RHS, name
not also a class/module. Decline on ambiguity. Bonus: feeds always-truthy.

## Gates

Standard battery + the measured deltas: gitlab-foss lib undefined-method
gaps 356 → ~220±15 expected after C1+C2+C5 (report honest numbers), 0 FP on
gitlab lib + app/models + mastodon app (matched ≥ 397; mastodon may also gain
matched). Fixture(s) must include the shadow-visibility negative (nested
module Time consumer stays silent) and the batch-vs-alone reproduction shape.
