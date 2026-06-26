# frozen_string_literal: true

# Unit test for RigortypeRs::Binary resolution + the exe/rigor shim's ARGV
# passthrough. Plain minitest (stdlib) — `gem/` has no rspec/bundler set up and
# the gem ships zero deps, so we avoid pulling rspec in. Run with:
#   ruby -Ilib spec/binary_resolution_spec.rb     (from the gem/ dir)

require "minitest/autorun"
require "fileutils"
require "tmpdir"
require "shellwords"
require "rigortype_rs/binary"

GEM_ROOT = File.expand_path("..", __dir__)
LIBEXEC_BIN = File.join(GEM_ROOT, "libexec", "rigor")
EXE_SHIM = File.join(GEM_ROOT, "exe", "rigor")

class BinaryResolutionTest < Minitest::Test
  def setup
    @staged = false
    return if File.exist?(LIBEXEC_BIN)

    # Stage a tiny stub binary that echoes its args so the present-case and the
    # ARGV-passthrough test work without a real cargo build.
    FileUtils.mkdir_p(File.dirname(LIBEXEC_BIN))
    File.write(LIBEXEC_BIN, <<~SH)
      #!/bin/sh
      echo "args:$*"
    SH
    File.chmod(0o755, LIBEXEC_BIN)
    @staged = true
  end

  def teardown
    FileUtils.rm_f(LIBEXEC_BIN) if @staged
  end

  def test_resolves_path_when_binary_present
    path = RigortypeRs::Binary.path
    assert_equal LIBEXEC_BIN, path
    assert File.exist?(path), "resolved path should exist"
    assert File.executable?(path), "resolved path should be executable"
  end

  def test_raises_not_found_with_guidance_when_absent
    # Temporarily move the binary aside so resolution fails.
    backup = "#{LIBEXEC_BIN}.bak"
    FileUtils.mv(LIBEXEC_BIN, backup) if File.exist?(LIBEXEC_BIN)
    err = assert_raises(RigortypeRs::Binary::NotFound) do
      RigortypeRs::Binary.path
    end
    msg = err.message
    assert_match(/no native binary/, msg)
    assert_match(/arm64-darwin/, msg)
    assert_match(/x86_64-darwin/, msg)
    assert_match(/x86_64-linux/, msg)
    assert_match(/aarch64-linux/, msg)
    assert_match(/cargo binstall rigor/, msg)
    assert_match(/brew/, msg)
  ensure
    FileUtils.mv(backup, LIBEXEC_BIN) if File.exist?(backup)
  end

  def test_exe_shim_passes_argv_through
    # exec the shim with args; the stub binary echoes them. Proves the shim is
    # transparent: same argv reaches the native binary.
    out = `ruby #{Shellwords.escape(EXE_SHIM)} check --format json some/file.rb 2>&1`
    assert_equal 0, $?.exitstatus, "shim should exit 0 via the stub"
    assert_equal "args:check --format json some/file.rb", out.strip
  end

  def test_exe_shim_raises_not_found_when_absent
    backup = "#{LIBEXEC_BIN}.bak"
    FileUtils.mv(LIBEXEC_BIN, backup) if File.exist?(LIBEXEC_BIN)
    out = `ruby #{Shellwords.escape(EXE_SHIM)} --version 2>&1`
    refute_equal 0, $?.exitstatus
    assert_match(/no native binary/, out)
  ensure
    FileUtils.mv(backup, LIBEXEC_BIN) if File.exist?(backup)
  end
end
