# Distribution: a precompiled-binary gem as the primary channel, plus standalone binaries

Status: accepted

rigor-rs ships through multiple channels, with a **gem carrying a precompiled native binary** (per platform) as the **primary** channel — matching how the reference is installed (`gem:rigortype`) and how comparable Rust-for-Ruby tools distribute (rubydex, pzoom). The gem is only a delivery vehicle: the payload is the native rigor-rs binary, which does not run on Ruby. This gives the smoothest drop-in replacement for the reference's Ruby-developer audience, who already have `gem`. Standalone channels — GitHub Releases binaries, `cargo-binstall`, Homebrew — serve non-Ruby CI and editor extensions as first-class co-equal paths.

libprism ([ADR-0003](0003-prism-rust-bindings.md)) is built from vendored C source and **statically linked** per target; Linux uses musl for a fully static binary. Targets: linux x86_64/aarch64 (gnu + musl), macOS x86_64/arm64, Windows x86_64 — cross-compiled in CI.

The [Ruby sidecar](0008-real-ruby-sidecar.md) runs in the **project's** Ruby environment, auto-detected from `.ruby-version` / `.tool-versions` / an active `bundle` / `PATH`, with a `ruby_path` config override; absence degrades per ADR-0008. Distribution of rigor-rs's own binary is independent of any Ruby — the sidecar uses the project's Ruby only when full-fidelity folding/plugins are exercised.

## Considered options

- **Standalone binary as the primary channel (gem secondary)** — kept as a co-equal channel, but gem-primary better matches the reference's install path and the drop-in-replacement goal.
- **A single channel only** — rejected: editor extensions, non-Ruby CI, and Homebrew users are poorly served by a gem-only release, and Ruby developers by a binary-only release.
