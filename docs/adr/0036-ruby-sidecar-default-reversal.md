# Ruby sidecar is the default; Ruby-free is opt-in — reverse ADR-0008's polarity

Status: accepted

Amends [ADR-0008](0008-real-ruby-sidecar.md): reverses its **default-degrade**
polarity and its "standalone is the default posture" positioning. ADR-0008's
sidecar architecture, caching, and decline-to-silence discipline all stand — only
the *default coverage posture* and its user-facing controls change.

## Context

ADR-0008 made the Ruby sidecar **optional**: a run without it degrades to the
[sound subset](../../CONTEXT.md) (sound, incomplete) with a one-time notice, and
ADR-0008 explicitly rejected "require the sidecar always." The problem: absent an
explicit choice, a run silently delivers **less than the machine can achieve** —
if the project's Ruby is present and would raise coverage to [full fidelity](../../CONTEXT.md),
defaulting to the subset without a hard signal is *avoidable, undisclosed
incompleteness*. For a bug finder that is a CI gate, a silent false negative
("green, but the bug shipped") is the quiet, dangerous failure — a one-time stderr
notice is effectively invisible in CI and LSP.

Rigor's core value is precise, *rigorous* inference — including literal/refined
types — which is exactly what the sidecar unlocks. **Nothing is yet announced as
production-ready**, so the default can be reversed now at zero backward-compat
cost. Crucially, "single-binary" ([ADR-0001](0001-rust-reimplementation-strategy.md))
is two values: **distribution** (one artifact, no install) and **self-contained
execution** (no external runtime). Reversing the default changes only the latter's
*default*; the artifact is unchanged, and `--ruby=off` restores self-contained
execution fully. So the reversal costs nothing an opt-out doesn't return.

## The decision

**Full fidelity is the default and the product identity; the Ruby-free sound
subset is an explicit opt-in.**

### 1. Coverage-posture axis and its controls

A single axis selects the posture, settable at four layers (highest wins):

```
CLI:      --ruby=require|auto|off|<path>     --no-ruby (= --ruby=off)
env:      RIGOR_RUBY=require|auto|off|<path>  RIGOR_NO_RUBY=1 (= off)
config:   .rigor.yml  →  rigor_rs: { ruby: require|auto|off|<path> }
default:  require (one-shot commands) · auto (rigor lsp)
precedence: CLI > env > .rigor.yml > default
```

| value | posture | sidecar unavailable → |
| --- | --- | --- |
| `require` (default for `check` etc.) | full fidelity | **hard error**, exit `69` (EX_UNAVAILABLE), one-step remedy hint |
| `auto` (default for `rigor lsp`) | full if usable, else sound subset | proceed as subset, **posture surfaced**, exit `0` |
| `off` / `--no-ruby` / `RIGOR_NO_RUBY=1` | sound subset | (no probe) exit `0`, posture reported |
| `<path>` (e.g. `/opt/ruby/bin/ruby`) | **`require`** using that binary | **hard error** if that binary is absent / not executable / handshake fails |

### 2. Overload semantics (`--ruby` takes a keyword OR a path)

- A value equal to a reserved keyword (`require`/`auto`/`off`) is that keyword; any
  other value is a **ruby binary path**. Use a path-form (`./off`) to name a ruby
  that collides with a keyword.
- A path **implies `require`** and **hard-errors when unusable** — an explicitly
  named ruby that doesn't exist is never the user's intent, so it never falls back
  to the subset (fail-loud, per the reversal's spirit).
- This makes the contradictory "off + a specific ruby" combination **unexpressible**
  (one value can't be both), so no error guard is needed for it. A separate
  `--ruby-bin` is deliberately NOT shipped now; the only capability lost is
  "`auto` with a specific path" (low demand), addable non-BC later.

### 3. Mutual exclusion

Specifying the same axis twice **within one layer** is a usage error (exit `64`),
redundant or not: `--no-ruby --ruby=off` is rejected; `RIGOR_RUBY` + `RIGOR_NO_RUBY`
both set is rejected. Across layers is override, not conflict (CLI beats env beats
file), never an error.

### 4. "Ruby available" = a sidecar handshake probe

Availability is decided by spawning the (auto-detected or `<path>`) Ruby, loading
the sidecar, and completing a ready/version handshake — not by a bare `ruby` on
PATH. A present-but-broken Ruby (version skew, missing stdlib, bundler mismatch)
counts as **unavailable** (else `require` would proceed and die mid-analysis).
Plugin target-gem gaps are NOT a global error: they keep ADR-0008's per-plugin
decline-to-silence, but the declined plugin's reduced posture is **surfaced**
(loud per-plugin, not a whole-run failure).

### 5. Coverage posture is never silent

Whatever the posture, it is surfaced: `rigor doctor` reports it as a first-class
check; one-shot runs emit a one-time posture notice; `rigor lsp` reports it via a
client notification (`window/showMessage`) and never crashes on unavailability.
(A machine-readable posture field in `--format json` is a noted follow-up — the
current JSON is a bare diagnostics array with no envelope for it.)

### 6. `rigor lsp` default = `auto`

The one context-dependent default. `rigor lsp` is long-lived and editor Ruby
detection is structurally fragile (GUI apps don't source shell rc, so
rbenv/asdf shims are often off PATH); a `require` default would break the
integration on first launch. So LSP defaults to `auto` and always surfaces
posture — full fidelity when the sidecar is reachable, the sound subset otherwise,
visible either way. Editor users force `require`/`off` via `rigor_rs.ruby` if they
want.

### 7. `rigor_rs:` config namespace

rigor-rs-specific config keys (those with no reference-schema equivalent — the
sidecar mode is one; the pure-Ruby reference has no such concept) live under a
`rigor_rs:` group. Reference-schema keys (`disable`, `exclude`, `plugins`,
`baseline`, `signature_paths`, `rbs_collection`) stay top-level for parity. The
reference ignores unknown keys, so `rigor_rs:` is transparent to it — the same
`.rigor.yml` feeds both implementations.

### 8. Phasing (the sidecar is not yet implemented)

- **Now** — record this ADR, ship the flag/env/config **surface**, and emit an
  interim posture notice: with no sidecar, `require`/`auto`/`<path>`/default all run
  the sound subset and print a one-time "full-fidelity Ruby sidecar not yet
  implemented — running the sound subset" notice + a `doctor` posture line. This
  converts today's *truly silent* subset into a *disclosed* posture immediately and
  freezes the vocabulary before announcement. No hard error yet (erroring every run
  pre-sidecar would make the tool unusable).
- **With the sidecar** — `require`/`<path>` gain teeth: the handshake probe runs and
  unavailability becomes the exit-`69` hard error; `auto` gains real full-fidelity;
  the interim notice is retired.

## Relationship to other ADRs

- **Amends ADR-0008**: reverses its default-degrade polarity (§Degradation) and its
  "standalone is the default posture" product positioning. The sidecar
  architecture, two-level cache, and decline-to-silence discipline are unchanged.
- **Preserves [ADR-0001](0001-rust-reimplementation-strategy.md)**: the single
  distribution artifact is unchanged; only the *default* self-contained-execution
  behavior moves, fully restored by `--ruby=off`.
- **Extends [ADR-0031](0031-config-and-command-semantics.md)**: adds the
  `rigor_rs:` config namespace and the coverage-posture axis to the config/command
  surface; the `rigor_rs:` namespacing is the standing convention for all future
  rigor-rs-specific config.

## Considered options

- **Keep default-degrade + opt-in strictness (`--require-ruby`)** — rejected: the
  loud notice still buries in CI/LSP (the original complaint), and reversing later
  (post-announcement) would be a BC break. Pre-announcement is the free window.
- **Require the sidecar always, no opt-out** — rejected: kills the portable/hermetic
  and self-contained-execution use cases; ADR-0008 already rejected this. `off`
  preserves them.
- **Context-dependent default everywhere** — rejected except the single, justified
  `rigor lsp` exception; uniform `require` is the model for one-shot commands.
- **Separate `--ruby` (mode) + `--ruby-bin` (path)** — rejected for now: overloading
  `--ruby` makes the contradictory off+path combo unexpressible (fewer error cases)
  and matches the user's `--ruby=<ruby>` intuition; `--ruby-bin` is addable non-BC
  if "auto + explicit path" demand appears.
- **Fall back to the subset on a bad `<path>`** — rejected: an explicitly named,
  unusable ruby is never intended; it hard-errors.

## Revisiting

Activate the hard-error teeth when the sidecar lands (phase 2). Add `--ruby-bin`
if "auto + explicit path" demand appears. Add a `--format json` posture field when
the JSON output grows an envelope.
