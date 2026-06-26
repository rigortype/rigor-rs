# Homebrew formula (rigor-rs)

This directory holds the **reviewable template** for the Homebrew distribution
channel (§13, ADR-0010 — co-equal with the GitHub Releases / `cargo binstall`
channel and the precompiled `rigortype-rs` gem).

## `rigor.rb` is a template — the sha256s are placeholders

The four `sha256` values in [`rigor.rb`](./rigor.rb) are deliberate, obvious
**placeholders** (`0` × 64). They are NOT real hashes; a `brew install` against
them will fail integrity verification. **Do not ship the placeholders as-is.**

On a tagged release, the `homebrew-formula` job in
[`.github/workflows/release.yml`](../.github/workflows/release.yml) regenerates
`rigor.rb` with:

- the real version (`${GITHUB_REF_NAME#v}`), and
- the four real per-target `sha256`s, read from the
  `rigor-<version>-<target>.tar.gz.sha256` sidecars the `build` job uploaded to
  the GitHub Release.

The filled formula is uploaded as a release/workflow **artifact** so it is
available for review and for the (deferred) tap push.

## Per-OS / per-arch → Rust target mapping

| Homebrew block            | Rust target                  | Release asset                                      |
|---------------------------|------------------------------|----------------------------------------------------|
| `on_macos` + `on_arm`     | `aarch64-apple-darwin`       | `rigor-<v>-aarch64-apple-darwin.tar.gz`            |
| `on_macos` + `on_intel`   | `x86_64-apple-darwin`        | `rigor-<v>-x86_64-apple-darwin.tar.gz`             |
| `on_linux` + `on_arm`     | `aarch64-unknown-linux-gnu`  | `rigor-<v>-aarch64-unknown-linux-gnu.tar.gz`       |
| `on_linux` + `on_intel`   | `x86_64-unknown-linux-gnu`   | `rigor-<v>-x86_64-unknown-linux-gnu.tar.gz`        |

The archive holds the bare `rigor` binary at its root, so `def install` is just
`bin.install "rigor"`. The asset naming matches the `cargo-binstall`
`pkg-url` (`rigor-{ version }-{ target }{ archive-suffix }`) — the channels are
consistent.

## Canonical install path (DEFERRED)

The eventual home is a **tap repo** `rigortype/homebrew-tap`:

```sh
brew install rigortype/tap/rigor
```

Pushing the generated formula to that tap needs the tap repo to exist **and** a
token, so the tap push is **gated/deferred** — exactly like the gem's
`gem push` (behind a secret + a manual `release` environment). Until the tap
repo + token exist, CI only *produces* the filled formula as an artifact; it
never pushes.

> **NOTE:** `https://github.com/rigortype/rigor-rs` is a placeholder URL — no
> public repo is configured yet. Confirm when the project is published.
