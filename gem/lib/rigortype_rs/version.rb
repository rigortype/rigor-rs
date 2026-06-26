# frozen_string_literal: true

module RigortypeRs
  # Single source of truth is `[workspace.package] version` in the repo-root
  # Cargo.toml (currently 0.1.0). `rake version:check` asserts this constant
  # equals it and fails loudly on drift.
  VERSION = "0.1.0"
end
