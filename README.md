# rigor-rs

> [!NOTE]
> **About this project.** rigor-rs is a **performance-oriented prototype** that
> reimplements the Ruby **Rigor** analyzer in Rust. **The Ruby implementation is
> the mainstream and always leads development** — replacing it with Rust is **not
> planned**. **Full compatibility with the Ruby version is not yet verified.**
> rigor-rs may eventually be kept in sync as a Rust implementation that preserves
> the same behavior, but that remains a possibility, not a commitment.

A Rust reimplementation of **Rigor**, the type-aware bug finder for Ruby. It
parses Ruby with Prism, infers types from the values expressions produce, reads
RBS as authoritative, and reports diagnostics under a **zero-false-positive
bar** — it only flags a problem when it can prove one.

rigor-rs ships as a **single self-contained binary**: the RBS signature set is
vendored and embedded at build time, so the core analyzer has no runtime Ruby or
rbs-gem dependency. It is a *sound subset* of the reference Ruby implementation —
where it cannot prove a finding it stays silent, and it never emits a diagnostic
the reference does not.

> [!IMPORTANT]
> **Status.** An early prototype under active development; **full compatibility
> with the Ruby version is not yet verified**. On the rules it implements it is a
> *sound subset* — across 3,829 real Ruby files (mastodon, gitlab-foss,
> conference-app, and Rigor's own source) it emits **0 false positives** at 100%
> precision, but it does not yet reproduce every diagnostic the Ruby version does.
> The port is incremental; see [`docs/CURRENT_WORK.md`](docs/CURRENT_WORK.md) for
> what is done and what remains.

## Install

The core binary is standalone — drop it on your `PATH` and run it; no Ruby
needed. Distribution channels (built by `.github/workflows/release.yml`) become
available with the first tagged release:

```sh
# Prebuilt binary via cargo-binstall (no compile)
cargo binstall rigor

# Homebrew (once the tap is published)
brew install rigortype/tap/rigor

# RubyGems — drop-in for the Ruby toolchain (precompiled native binary)
gem install rigortype-rs

# Or download a release archive directly:
#   https://github.com/rigortype/rigor-rs/releases  →  rigor-<version>-<target>.tar.gz
```

Supported prebuilt targets: `aarch64`/`x86_64` macOS, `x86_64`/`aarch64` Linux
(gnu). Other platforms (musl, Windows) build from source.

### From source

```sh
cargo build --release            # produces target/release/rigor (standalone)
./target/release/rigor --version
```

Requires a recent Rust toolchain (edition 2024, MSRV 1.88 — the `ruby-rbs`
dependency uses let-chains). `ruby-prism` and
`ruby-rbs` compile their vendored C at build time, so a C toolchain + libclang
are needed to build (not to run).

## Usage

```sh
rigor check path/to/file.rb              # analyze a file or directory
rigor check app/ --format json           # machine-readable output
rigor check app/ --format github         # CI annotations
```

`check` reports diagnostics under the zero-false-positive bar. Output formats:
`text` (default), `json`, `github`, `sarif`, `gitlab`, `checkstyle`, `junit`,
`teamcity`. CI is auto-detected (GitHub Actions, GitLab CI, …) so the right
format is emitted without configuration.

Other commands:

| Command | Purpose |
| --- | --- |
| `rigor check` | Run analysis (the primary command) |
| `rigor type-of FILE:LINE:COL` | Show the inferred type of an expression |
| `rigor explain <rule>` | Describe a diagnostic rule |
| `rigor docs [<rule>]` | List / print rule documentation |
| `rigor baseline generate` | Record current diagnostics to suppress them |
| `rigor init` | Write a starter `.rigor.dist.yml` |
| `rigor doctor` | Report the RBS source, plugins, and rule set |
| `rigor plugins` | List the bundled plugins |
| `rigor --version` | Print the version |

## What it checks

The implemented rules (a sound subset of the reference's set):

- **`call.undefined-method`** — a method called on a receiver whose class
  provably lacks it (witnessed only on core/RBS-known receivers).
- **`call.wrong-arity`** — a call with the wrong number of arguments.
- **`call.possible-nil-receiver`** — a method called on a value that may be `nil`
  with no narrowing guard.
- **`flow.dead-assignment`** — a local assigned but never read.
- **`flow.always-raises`** — Integer division/modulo by a constant zero.
- **`flow.unreachable-branch`** — an `if`/`unless` with a literal predicate whose
  dead branch can never run.
- **`def.override-visibility-reduced`** — an override that narrows a method's
  visibility, breaking substitutability.

## Configuration

A `.rigor.yml` in the working directory (or `--config <path>`) is optional:

```yaml
disable:                       # silence rules project-wide (ids or family tokens)
  - flow.dead-assignment
exclude:                       # skip files by glob
  - "db/schema.rb"
plugins:                       # opt-in, config-gated
  - activesupport-core-ext     # reopens core classes with ActiveSupport selectors
baseline: .rigor-baseline.yml  # suppress a recorded set of pre-existing findings
```

Inline suppression is also honored: `# rigor:disable <rule>` at the end of a
line, or `# rigor:disable-file <rule>` / `all` for a whole file.

Run `rigor doctor` to see the active RBS source, the bundled plugins, and the
implemented rule set.

## How it relates to Ruby Rigor

rigor-rs is a faithful port whose correctness bar is **diagnostic-set parity**
with the reference Ruby implementation: for a given input, the `(rule id,
location)` pairs it emits should match the reference on the rules it implements
(message wording may improve; the set must match). A differential harness runs
both tools over a shared corpus and fails on any divergence. **The Ruby
implementation remains the mainstream and leads development** — rigor-rs tracks it
for performance. Reaching full compatibility, and one day syncing as a
behavior-preserving Rust implementation, is a possibility rather than a committed
goal; replacing the Ruby mainstream is not planned.

The standalone binary covers the sound subset that needs no Ruby. The full
plugin long-tail (the Rails family and beyond) is delivered via opt-in bundled
plugins today and, in future, an optional Ruby sidecar that uses the *project's*
Ruby — never a bundled one.

## Contributing & design

- [`CONTRIBUTING.md`](CONTRIBUTING.md) — how to build, test, and run the parity harness.
- [`CONTEXT.md`](CONTEXT.md) — the project's ubiquitous language.
- [`docs/adr/`](docs/adr/) — architecture decision records.
- [`docs/CURRENT_WORK.md`](docs/CURRENT_WORK.md) — the live port roadmap.

## License

[AGPL-3.0](LICENSE). (Note: this differs from the reference Ruby implementation,
which is MPL-2.0.)
