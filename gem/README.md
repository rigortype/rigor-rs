# rigortype-rs

Precompiled-binary distribution of **rigor-rs**, the Rust port of [Rigor](https://github.com/rigortype/rigor) — an inference-first static analyzer for Ruby.

This gem is a thin shim around a **native, self-contained `rigor` binary**: installing it puts a `rigor` executable on your PATH with **zero runtime dependencies** (no `prism`/`rbs`/`language_server-protocol` gems — the analysis engine is statically compiled into the binary, ADR-0007).

> **NOTE:** `https://github.com/rigortype/rigor-rs` is a placeholder URL — no public repo is configured yet. It will be confirmed when the project is published.

## Install

```sh
gem install rigortype-rs
```

RubyGems automatically selects the precompiled platform gem matching your machine. Supported precompiled platforms:

| Gem::Platform | Machine |
|---|---|
| `arm64-darwin` | Apple Silicon macOS |
| `x86_64-darwin` | Intel macOS |
| `x86_64-linux` | x86_64 Linux (glibc) |
| `aarch64-linux` | aarch64 Linux (glibc) |

Then:

```sh
rigor --version      # rigor 0.0.1
rigor check path/to/file.rb
```

## `rigortype` vs `rigortype-rs`

Both gems install a `rigor` executable. **Do not install both in the same environment** — they will collide on the `rigor` name.

- **`rigortype`** — the reference Ruby implementation (the canonical, full-featured analyzer).
- **`rigortype-rs`** (this gem) — the Rust port, a performance-oriented prototype and an **opt-in** channel. It implements a *sound subset* of the reference's rules (see the project docs). Per ADR-0001 the Rust port **coexists** with the Ruby mainstream — there is **no planned `rigortype` name takeover**, so `rigortype-rs` keeps a distinct name permanently and never silently downgrades a `rigortype` install.

## Unsupported platforms (musl Linux, Windows, …)

The `ruby`-platform fallback gem carries **no binary**; on an unsupported platform `rigor` raises a clear error pointing you here. Install the native binary another way:

```sh
cargo binstall rigor      # prebuilt binary via cargo-binstall
brew install rigor        # Homebrew (when the formula is published)
```

### Bundler on a precompiled gem

When you commit `Gemfile.lock` for a precompiled gem, add the platforms you deploy to so Bundler resolves the right variant in CI/production:

```sh
bundle lock --add-platform arm64-darwin x86_64-linux aarch64-linux x86_64-darwin
```

## License

AGPL-3.0. See [LICENSE](LICENSE).
