# The effects differential was a vacuous gate — a real project and the `resolved` probes

2026-08-26. Instrument work, no `crates/` behaviour change. Everything measured
against the PINNED submodule at `v0.3.4` (`b10bd5df`), populated into this
worktree from the parent checkout's tree (never the network, never
`REFERENCE_RIGOR_DIR`), with `.rigor/cache` cleared either side of every oracle
run.

Subject: the highest-value finding of
[the slice-4 probe](20260826-effects-s4-probe.md) § 6 — `harness/effects_diff.py`'s
standing corpus is seven synthetic projects the port's own authors wrote, and it
cannot distinguish a correct implementation from a no-op one. Independent of
whether slice 4 ever ships.

**Headline: the vacuity is confirmed on the REAL BINARY, not only in simulation,
and it is now closed.** A `rigor` built with the `unresolved-self-call` taint
deleted outright scores `MATCH=320 UNDER=248 OVER=0` on the seven fixture
projects — **every number identical to master's, to the digit** — and PASSES.
The same binary scores **76 OVER** and FAILS on the improved standing set.

- **Deliverable 1** — `mastodon/app` (1,236 files, 6,948 methods, ~10 s) joins
  the DEFAULT run; `gitlab-foss/lib` (28,607 methods, ~40 s) is behind `--scale`.
  Each is COPIED into a temp project with a synthesised `.rigor.yml`; the
  checkout is only ever read (§ 1).
- **Deliverable 2** — `harness/effects-corpus/08_resolved`, 44 methods promoted
  from the probe's scratch projects, each commented with the property it
  discriminates (§ 2). Writing it **corrected three rows of the probe note's
  § 4b blind-position table** (§ 2b).
- **Deliverable 3** — the bite proof above, per project (§ 3).
- Zero OVER on the shipped binary everywhere, including both real projects, so
  the "STOP and report a live defect" condition did not trigger.

---

## 1. The mechanism, and why this one

`rigor effects` needs a project directory with a `.rigor.yml`; the sweep corpora
are plain checkouts without one. Two shapes were prototyped and **measured to
produce bit-identical results** on `mastodon/app` (both arms, both engines,
`ref link==copy: True`, `rs link==copy: True`):

| | build cost | |
|---|---|---|
| temp cwd + `.rigor.yml` beside a **symlink** to the real tree | 0.00 s | the probe's `p_mast` shape |
| temp cwd + `.rigor.yml` beside a **copy** of the real tree | 0.61 s (38 MB) / 1.44 s (25 MB) | **chosen** |

The copy wins on the constraint that outranks run time.

**Residue is structural, not conditional.** The task's hard constraint is that
the user's checkouts cannot receive residue — a previous agent had to clean stray
`.rigor.yml` files out of them by hand. A copy makes the checkout a read-only
input by construction: every write either engine performs (`.rigor/cache`,
`.rigor-effects.yml`, and whatever a future subcommand adds) lands in the temp
project. A symlink leaves a live writable path into the user's tree for the
duration of the run, and today's argument for its safety is an argument about
what the engines happen to write.

**And it closes a config-discovery ambush.** The temp project has no ancestor
directory to walk into. `gitlab-foss/lib` really does sit one level under a real
`/Users/…/gitlab-foss/.rigor.yml` carrying its own `paths:`, `plugins:` and
`severity_profile: lenient`; `mastodon/` carries `.rigor.dist.yml` and a 132 KB
`.rigor-baseline.yml`. Neither engine walks up into them today — that is what
"link == copy" proves — but "measured equal today" is exactly what a pin bump is
allowed to change, and a confound in the instrument costs more than 0.6 s.

The synthesised config is deliberately minimal — `paths:` and nothing else, no
`plugins:`, no `severity_profile:` — so the two arms differ in nothing but the
engine under test.

### 1a. Membership, and the SKIPPED contract

Paths are NOT duplicated. `effects_diff.py`'s `REAL_PROJECTS` names corpora by
**label** and resolves each path from `harness/sweep-corpora.yml`, the repo's
single membership list for external checkouts, already shared by `run_corpus.rb`
and `fp_audit.py --sweep`. What lives in the tool is the selection and the
effects-specific rationale; what lives in the manifest is the path.

- a label `REAL_PROJECTS` names that the manifest does not carry is a **repo
  bug**: hard error, nothing measured;
- a corpus the manifest carries that is **not on this machine** is SKIPPED
  loudly, matching `fp_audit.py`.

One deliberate strengthening over `fp_audit.py`: there, the skip banner is the
only record, and `TOTAL FP candidates: 0` still prints clean. Here the
incompleteness also rides on the `RESULT:` line, because that is the line that
gets grepped and quoted into a note:

```
=== mastodon/app — SKIPPED: /nonexistent/mastodon/app is not on this machine (the standing set is INCOMPLETE for this run) ===

RESULT: PASS — no over-claims. (UNDER is the arc's odometer, not a failure.)
        INCOMPLETE — 2 standing corpora SKIPPED: mastodon/app, gitlab-foss/lib
```

The exit code is unchanged by a skip (a machine-local membership list may
legitimately be short a member), which is `fp_audit.py`'s rule exactly.

---

## 2. `08_resolved` — 44 methods, four sections

Hand-written, unlike `05_posture` and `07_mutators`: there is no vendored table
to generate from. Every method carries a `DISCRIMINATES:` comment naming the
property, because a fixture nobody can explain is a fixture nobody will maintain.

| section | what it pins |
|---|---|
| **1. the binder admission test** | `Binder`, 7 call sites. `resolved` is not "the project defines the method": the callee must have REQUIREDS-ONLY parameters and the positional arity must match exactly. `calls_required` is the control (oracle: exhaustive); a keyword, optional, rest or block parameter on the callee, or either arity mismatch, makes the site `unresolved-self-call` **while the edge still resolves and the label still propagates**. Each callee proves a different label, so a mis-propagation names itself. |
| **2. selectors the closed world does not answer** | `Stranger` ×3 — **the bite**: a receiver-less call to a selector no unit defines and no row claims, the one shape whose oracle bit is false for a single reason with no project edge to reach it by. Plus `Gamma#calls_it`, a selector naming a unit on an unrelated class (the port's stand-in reaches the same FALSE by a different road; a real closure without a `resolved` reconstruction goes silent). |
| **3. traversal blind spots** | `Visited` ×8 and `Blind` ×10, the same call in different syntax, sharing one leaf through a superclass. |
| **4. two smaller traps** | `Selfy#from_define_method` — a `define_method` body's `self` is the CLASS object, so the edge is `Selfy.dm_helper` (singleton) and resolves to nothing. `<toplevel>#top_caller` — a toplevel self-call records `receiver_class: "Object"` while the key is `<toplevel>#top_helper`, so the label does not propagate; the call IS resolved, so the oracle calls it exhaustive. |

### 2a. What it grades today, and what it grades later

Sections 1, 2 and 4 bite **now** — `Stranger` is measurably the shape the
existing corpus lacked. Section 3 is a trap laid for the transitive lane: the
shipped port propagates nothing, so those methods are UNDER on the label lane;
they exist so the first slice that joins labels along a syntactic edge set fails
in the corpus rather than on a real project three months later.

### 2b. It corrects the probe note's § 4b table

The probe lists **modifier `unless` body**, **block-form `unless` body** and
**`elsif` arm** as positions the typer does not visit. Measured at the same pin,
with an ivar or parameter condition, all three are **VISITED** — the oracle
propagates `io.output.stdout` and calls the caller exhaustive:

```
C#live_unless    ref ex=True  eff=['io.output.stdout']
C#live_elsif     ref ex=True  eff=['io.output.stdout']
C#dead_unless_local  ref ex=False eff=[] causes=['unresolved-self-call']   # `truthy = 1; leaky unless truthy`
C#dead_unless_call   ref ex=False eff=[] causes=['unresolved-self-call']   # cond is a project method returning `true`
C#dead_elsif         ref ex=False eff=[] causes=['unresolved-self-call']   # the `if` condition folds true
C#dead_if            ref ex=False eff=[] causes=['unresolved-self-call']   # `if false`
```

They go blind only when the condition **folds** — a local holding a literal, or a
project method whose body is one — because the arm is then dead code the typer
never evaluates. That is the same property as `if false`, not a fact about
`unless`. The probe's three rows were an artifact of its own condition
expression. `Visited#in_modifier_unless` / `Blind#in_dead_unless` and
`Visited#in_elsif_arm` / `Blind#in_dead_elsif` are the paired reproducers, and
the second pair folds **through a call**, which is why the blind set is not
portable at parity: mirroring it would need constant folding across the call
graph.

The rows that survive unchanged, measured: `return` values (bare and guarded),
regexp and symbol interpolation, `next` / `break` values, the receiver of a
compound write, any dead branch — blind; string interpolation, `if` arms, simple
block bodies, non-tail statements — visited. Two bonus facts the file pins: an
inherited plain `def` RESOLVES (contrast an inherited `attr_*` unit, which does
not), and a parameter default is invisible to the effect scan on BOTH sides.

---

## 3. The verdict table, and the bite proof

Both columns are the same instrument, one commit apart in `crates/` only. The
**wrong variant** is `target/release/rigor` built with one line deleted —
`self.taint(UNRESOLVED_SELF_CALL, Some(&selector))` at `collect.rs:697`, the
receiver-less uncatalogued arm — i.e. the probe's `S4_ARM=v0`: no
`unresolved-self-call` taint whatsoever. Built into a separate `CARGO_TARGET_DIR`
and measured through `RIGOR_RS_BIN`; the working tree was reverted before any
gate ran, and the shipped binary is byte-identical to the master baseline
(`sha256 280db68a…` both).

| project | oracle methods | SHIPPED binary | wrong variant |
|---|---|---|---|
| `01_core_origins` | 16 | 16 / 0 / **0** / 0 | 16 / 0 / **0** / 0 |
| `02_propagation` | 15 | 10 / 5 / **0** / 0 | 10 / 5 / **0** / 0 |
| `03_taint` | 11 | 9 / 2 / **0** / 0 | 9 / 2 / **0** / 0 |
| `04_declared` | 4 | 0 / 4 / **0** / 0 | 0 / 4 / **0** / 0 |
| `05_posture` | 133 | 61 / 72 / **0** / 0 | 61 / 72 / **0** / 0 |
| `06_edge` | 6 | 5 / 1 / **0** / 0 | 5 / 1 / **0** / 0 |
| `07_mutators` | 383 | 219 / 164 / **0** / 0 | 219 / 164 / **0** / 0 |
| *(the seven, total)* | *568* | *320 / 248 / **0** / 0 — **PASS*** | *320 / 248 / **0** / 0 — **PASS*** |
| **`08_resolved`** *(new)* | 44 | 28 / 16 / **0** / 0 | 25 / 16 / **3** / 0 |
| **`mastodon/app`** *(new)* | 6,948 | 5,217 / 1,731 / **0** / 0 | 5,147 / 1,728 / **73** / 0 |
| **TOTAL (default run)** | 7,560 | **5,565 / 1,995 / 0 / 0 — PASS** | **5,492 / 1,992 / 76 / 0 — FAIL** |
| `gitlab-foss/lib` *(`--scale`)* | 28,607 | 20,990 / 7,617 / **0** / 0 | — |
| **TOTAL (`--scale`)** | 36,167 | **26,555 / 9,612 / 0 / 0 — PASS** | — |

Cells are `MATCH / UNDER / OVER / DECLARED-MISMATCH`. The four PINNED projects
hold their recorded rows exactly (`01` 16/0/0/0, `02` 10/5/0/0, `03` 9/2/0/0,
`04` 0/4/0/0). 0 OVER everywhere on the shipped binary, on both real projects
included — no live defect to stop for.

**The vacuity, stated once.** The seven-project column is not merely "PASS on
both": it is *the same numbers*, every project, every kind — `UNDER by kind`
included. A gate that cannot see a deleted line of the thing it gates is not a
weak gate, it is not a gate.

The three new fixture OVERs are exactly the shapes § 2 predicts:

```
OVER: Stranger#calls_nothing_at_all      — claims exhaustiveness the oracle does not
OVER: Stranger#calls_nothing_with_args   — claims exhaustiveness the oracle does not
OVER: Stranger#proves_and_calls_nothing  — claims exhaustiveness the oracle does not
```

and mastodon's 73 are ordinary application methods —
`OAuthMetadataPresenter#issuer`, `MediaAttachment#audio_or_video?`,
`Expireable#expire!`, `Setting#to_param` — which is the point of a real project:
the fixture proves the mechanism, the corpus proves the frequency.

Why only `Stranger` and not the other 41 fixture methods: the port's slice-3
stand-in makes a unit non-exhaustive when any edge selector names a project unit
at all (`collect.rs:257`), so every *other* non-exhaustive method in the corpus
is reachable by a second road. `Stranger` calls a selector nothing in the project
defines, so the taint is the only road. That is precisely the shape 01-07 lacked.

---

## 4. Residue verification

`.rigor*` under both corpus checkouts, before the first run and after every run
in this session including two `--scale` runs and the whole bite proof:

```
/Users/megurine/repo/ruby/gitlab-foss/.rigor       1780558638  96
/Users/megurine/repo/ruby/gitlab-foss/.rigor.yml   1779951658  516
/Users/megurine/repo/ruby/mastodon/.rigor          1779869612  96
/Users/megurine/repo/ruby/mastodon/.rigor-baseline.yml 1779870526 132573
/Users/megurine/repo/ruby/mastodon/.rigor.dist.yml 1779870535  626
```

Identical name, mtime and size on both sides — these five are the user's own
pre-existing artifacts and none was touched. Additionally: no `.rigor*` of any
kind exists anywhere under `mastodon/app` or `gitlab-foss/lib`,
`git status --short` is empty for both subtrees, and no `rigor-effects-*` temp
project survives in `$TMPDIR`.

---

## 5. Gates

Run bare, from the worktree.

| gate | verdict |
|---|---|
| `cargo test --workspace` | PASS |
| `CARGO_TARGET_DIR=$(mktemp -d) cargo clippy --workspace --all-targets -- -D warnings` | PASS |
| `effects_diff.py --self-test` (8 fixtures + mastodon/app) | PASS — 7,560 methods, all MATCH |
| `effects_diff.py` (default) | PASS — 5,565 / 1,995 / 0 / 0 |
| `effects_diff.py --scale` | PASS — 26,555 / 9,612 / 0 / 0 |
| `rigor check` vs the master baseline, mastodon/app (1,236 files) | **BYTE-IDENTICAL** (`sha256 eb3b63dc…`) |
| `harness/run_snapshot.rb` | PASS |
| `harness/docs_check.py` | PASS |
| `gen_effects_posture_corpus.py --check` | PASS |
| `gen_effects_mutator_corpus.py --check` | PASS |

---

## 6. Deviations

1. **`docs/CURRENT_WORK.md` is not touched.** It sits at 24,549 bytes against a
   24,576-byte budget — 27 bytes of headroom — so a ledger line does not fit
   without a fold, and folding is the orchestrator's call
   (`harness/docs_check.py` passes as-is).
2. **The `RESULT:` line carries the incompleteness**, which is one notch
   stronger than `fp_audit.py`'s skip contract (§ 1a). Deliberate, and the exit
   code still matches.
3. **`--fixtures-only` was not added.** Positional arguments already replace the
   whole standing set, so the fast development loop is
   `effects_diff.py harness/effects-corpus/*` and the flag would be a second way
   to say it.
4. **The wrong variant was not run on `gitlab-foss/lib`.** The default set
   already fails it by 76; a fourth measurement of the same deletion buys
   nothing but 40 s.
