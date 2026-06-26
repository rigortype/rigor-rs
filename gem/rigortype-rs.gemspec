# frozen_string_literal: true

require_relative "lib/rigortype_rs/version"

Gem::Specification.new do |spec|
  spec.name = "rigortype-rs"
  spec.version = RigortypeRs::VERSION
  spec.authors = ["Rigor contributors"]
  spec.email = ["maintainers@example.invalid"]

  spec.summary = "Precompiled native binary distribution of rigor-rs (the Rust port of Rigor)."
  spec.description = "rigortype-rs ships the standalone, self-contained Rust `rigor` static analyzer " \
                     "as a precompiled platform-specific gem. Installing it puts a native `rigor` " \
                     "executable on your PATH with zero runtime dependencies (no prism/rbs/lsp gems) — " \
                     "the analysis engine is compiled in. This is an opt-in, early-adopter channel for " \
                     "the Rust port; the `rigortype` name remains the reference Ruby implementation."
  # NOTE: placeholder — no git remote is configured yet for rigor-rs. Confirm
  # when the repository is published (mirrors the Cargo.toml `repository` note).
  spec.homepage = "https://github.com/rigortype/rigor-rs"
  spec.license = "AGPL-3.0"
  spec.required_ruby_version = ">= 3.0"

  spec.metadata = {
    "bug_tracker_uri" => "#{spec.homepage}/issues",
    "source_code_uri" => spec.homepage,
    "rubygems_mfa_required" => "true"
  }

  # NOTE: `spec.platform` is intentionally NOT set here. The Rakefile assigns it
  # per build (`build:platform[arm64-darwin]` etc., or the host string for
  # `build:local`); the gemspec stays platform-neutral so a single spec drives
  # all variants.
  #
  # `libexec/rigor` is staged at build time (NOT committed — only `.keep` is).
  # The `ruby` fallback gem is built with `libexec/rigor` absent.
  spec.files = Dir.glob(
    %w[
      README.md
      LICENSE
      exe/*
      lib/**/*.rb
      libexec/rigor
      sig/**/*.rbs
    ]
  )
  spec.bindir = "exe"
  spec.executables = ["rigor"]
  spec.require_paths = ["lib"]

  # No runtime dependencies: the payload is the native binary, not Ruby. This
  # deliberately DROPS the reference gemspec's prism/rbs/language_server-protocol
  # deps (ADR-0007 — the engine is statically compiled in).
end
