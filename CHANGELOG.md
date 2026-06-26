# Changelog

All notable changes to rigor-rs are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/), and the project aims for
[Semantic Versioning](https://semver.org/) once it reaches parity (pre-1.0,
the surface may still shift as the port progresses).

## [Unreleased]

- (in progress) Distribution: musl + Windows release targets; quality
  management (clippy-clean tightening, snapshot-mode CI parity). See
  `docs/CURRENT_WORK.md` for the live roadmap.

## [0.0.1] — first release

The first tagged release of rigor-rs — the Rust reimplementation of **Rigor**,
the type-aware bug finder for Ruby. A standalone, single self-contained binary
that is a *sound subset* of the reference Ruby implementation: it only flags a
problem when it can prove one, and never emits a diagnostic the reference does
not. Validated at **0 false positives across 3,829 real Ruby files** (100%
precision).

### Analysis

- `rigor check` over the diagnostic-set-parity bar, with a real RBS-backed index
  (vendored + embedded at build time — no runtime Ruby or rbs-gem dependency)
  and Rust-native constant folding.
- Seven diagnostic rules across three families:
  - `call.undefined-method`, `call.wrong-arity`, `call.possible-nil-receiver`
  - `flow.dead-assignment`, `flow.always-raises`, `flow.unreachable-branch`
  - `def.override-visibility-reduced`
- Cross-file in-source method return-type inference (tier-4 minimal slices);
  block-call return typing from RBS block overloads; class-method (singleton)
  witnessing; interpolated-string typing.
- First plugin: `activesupport-core-ext` (pure-RBS, config-gated) — reopens core
  classes with the most-flagged ActiveSupport selectors.

### CLI & output

- Commands: `check`, `type-of`, `explain`, `docs`, `baseline`, `init`, `doctor`,
  `plugins`, `version`.
- Output formats: `text`, `json`, `github`, `sarif`, `gitlab`, `checkstyle`,
  `junit`, `teamcity`; CI auto-detection.
- Configuration: `.rigor.yml` (`disable`/`exclude`/`plugins`/`baseline`), inline
  `# rigor:disable` suppression, and a byte-compatible `.rigor-baseline.yml`.

### Distribution

- Release pipeline (`.github/workflows/release.yml`): tag-triggered cross-compile
  matrix → GitHub Release assets.
- Channels: prebuilt binaries (GitHub Releases), `cargo binstall`, the
  `rigortype-rs` precompiled-binary gem, and a Homebrew formula.
- Prebuilt targets: macOS (arm64, x86_64), Linux gnu (x86_64, aarch64). Other
  platforms build from source.

[Unreleased]: https://github.com/rigortype/rigor-rs/compare/v0.0.1...HEAD
[0.0.1]: https://github.com/rigortype/rigor-rs/releases/tag/v0.0.1
