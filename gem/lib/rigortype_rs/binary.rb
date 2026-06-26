# frozen_string_literal: true

module RigortypeRs
  # Resolves the native `rigor` binary bundled in a platform-specific gem at
  # `<gem_root>/libexec/rigor`. Platform gems (arm64-darwin / x86_64-darwin /
  # x86_64-linux / aarch64-linux) ship the matching binary; the `ruby`-platform
  # fallback gem ships none, so `path` raises `NotFound` with guidance toward
  # the other install channels.
  module Binary
    # Raised when no native binary is bundled (the `ruby` fallback gem, or a
    # platform this project does not yet precompile). The message lists the
    # supported precompiled platforms and points at the fallback channels.
    class NotFound < StandardError; end

    SUPPORTED_PLATFORMS = %w[arm64-darwin x86_64-darwin x86_64-linux aarch64-linux].freeze

    # Absolute path to the bundled native binary, e.g.
    #   <gem_root>/libexec/rigor
    # `__dir__` is `<gem_root>/lib/rigortype_rs`, so two levels up is the root.
    def self.path
      candidate = File.expand_path(File.join(__dir__, "..", "..", "libexec", "rigor"))

      unless File.exist?(candidate)
        raise NotFound, <<~MSG
          rigortype-rs: no native binary bundled for this platform (looked for #{candidate}).

          The `rigortype-rs` gem ships a precompiled `rigor` only for:
            #{SUPPORTED_PLATFORMS.join(", ")}

          On those platforms RubyGems installs the matching platform gem. You may
          have landed on the `ruby` fallback gem (no binary), or you are on a
          platform we do not precompile yet (e.g. musl Linux, Windows).

          Install the binary another way instead:
            cargo binstall rigor      # prebuilt binary via cargo-binstall
            brew install rigor        # Homebrew (when the formula is published)

          Then ensure `rigor` is on your PATH.
        MSG
      end

      # Defensive: a binary staged without the exec bit (e.g. via some unpack
      # paths) would make `exec` fail with EACCES. Make it runnable.
      File.chmod(0o755, candidate) unless File.executable?(candidate)

      candidate
    end
  end
end
