#!/usr/bin/env ruby
# frozen_string_literal: true

# harness/run.rb — Differential parity harness (ADR-0002 + ADR-0011)
#
# Compares rigor-rs diagnostic output against the reference Ruby Rigor
# implementation over a corpus of fixture files.
#
# Parity discipline:
#   - rigor-rs must NEVER emit an unregistered diagnostic the reference
#     doesn't emit ("extra" = false positive → hard failure).
#   - Diagnostics the reference emits but rigor-rs misses ("missing" =
#     coverage gap) are expected during the port and never fail the gate.
#   - Registered divergences (harness/divergence-registry.yml) excuse
#     specific "extra" entries per ADR-0011.
#
# Usage: ruby harness/run.rb   (from repo root)
#
# Env vars:
#   REFERENCE_RIGOR_DIR  path to the Ruby rigor checkout
#                        (default: /Users/megurine/repo/ruby/rigor)
#   RIGOR_RS_BIN         path to the rigor-rs binary
#                        (default: target/debug/rigor, rebuilt if absent)
#   CORPUS_DIR           path to corpus directory
#                        (default: harness/corpus)
#   DIVERGENCE_REGISTRY  path to divergence registry YAML
#                        (default: harness/divergence-registry.yml)

require "json"
require "yaml"
require "open3"
require "tmpdir"
require "fileutils"
require "pathname"

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

REPO_ROOT = File.expand_path("..", __dir__)

REFERENCE_RIGOR_DIR = ENV.fetch(
  "REFERENCE_RIGOR_DIR",
  "/Users/megurine/repo/ruby/rigor"
)

RIGOR_RS_BIN = File.expand_path(
  ENV.fetch("RIGOR_RS_BIN", "target/debug/rigor"),
  REPO_ROOT
)

CORPUS_DIR = File.expand_path(
  ENV.fetch("CORPUS_DIR", "harness/corpus"),
  REPO_ROOT
)

DIVERGENCE_REGISTRY_PATH = File.expand_path(
  ENV.fetch("DIVERGENCE_REGISTRY", "harness/divergence-registry.yml"),
  REPO_ROOT
)

REFERENCE_LIB = File.join(REFERENCE_RIGOR_DIR, "lib")
REFERENCE_EXE = File.join(REFERENCE_RIGOR_DIR, "exe", "rigor")

# Severity levels we care about for parity. Info/hint are excluded.
PARITY_SEVERITIES = %w[error warning].freeze

# ---------------------------------------------------------------------------
# Plugin-enabled fixtures (ADR-25)
# ---------------------------------------------------------------------------
#
# A fixture `corpus/NN_name.rb` may declare a SIDECAR config
# `corpus/NN_name.rigor.yml`. When present, the harness runs BOTH tools with
# that config (`--config <sidecar>`) so a config-gated plugin is exercised on
# BOTH sides — the reference loads the plugin from the sidecar's `plugins:`
# list, and rigor-rs ingests the matching bundled RBS. The reference also needs
# the plugin gem's `lib/` on its `-I` load path (it `require`s the gem); the
# rigor-rs binary has the plugin RBS vendored, so it needs only `--config`.
#
# Fixtures WITHOUT a sidecar (the existing 16) are unchanged: no `--config`, no
# extra `-I` — byte-identical to before this slice.
#
# `PLUGIN_LIB_DIRS` maps a plugin id (as it appears in a sidecar's `plugins:`)
# to the gem `lib/` the reference must load. NOTE: the reference `require`s the
# id verbatim, so a sidecar MUST use the GEM-NAME spelling
# (`rigor-activesupport-core-ext`) — the manifest-id spelling
# (`activesupport-core-ext`) is not require-able by the reference. rigor-rs
# normalises both, so the gem-name works for both tools. Add an entry here when
# vendoring a new plugin's parity fixture.
PLUGIN_LIB_DIRS = {
  "rigor-activesupport-core-ext" =>
    File.join(REFERENCE_RIGOR_DIR, "plugins", "rigor-activesupport-core-ext", "lib")
}.freeze

# The sidecar config path for a fixture, or `nil` if it has none. Convention:
# `corpus/NN_name.rb` ⇒ `corpus/NN_name.rigor.yml`.
def sidecar_config(fixture_path)
  cfg = fixture_path.sub(/\.rb\z/, ".rigor.yml")
  File.exist?(cfg) ? cfg : nil
end

# The `-I <lib>` flags the reference needs to `require` every plugin named in a
# sidecar config's `plugins:` list. Returns a flat array (possibly empty).
def reference_plugin_includes(sidecar)
  return [] if sidecar.nil?

  data = begin
    YAML.safe_load_file(sidecar) || {}
  rescue StandardError
    {}
  end
  Array(data["plugins"]).flat_map do |id|
    lib = PLUGIN_LIB_DIRS[id.to_s]
    lib ? ["-I", lib] : []
  end
end

# ---------------------------------------------------------------------------
# Build rigor-rs if binary is absent
# ---------------------------------------------------------------------------

def ensure_rigor_rs_binary!
  return if File.executable?(RIGOR_RS_BIN)

  puts "rigor-rs binary not found at #{RIGOR_RS_BIN}; building..."
  Dir.chdir(REPO_ROOT) do
    system("cargo build --offline -p rigor-cli") or
      abort("ERROR: cargo build failed — cannot continue")
  end

  unless File.executable?(RIGOR_RS_BIN)
    abort("ERROR: binary still missing after build: #{RIGOR_RS_BIN}")
  end
  puts "Build OK: #{RIGOR_RS_BIN}"
end

# ---------------------------------------------------------------------------
# Run each implementation and parse JSON output
# ---------------------------------------------------------------------------

# Run the reference Ruby Rigor on a fixture, isolated from any .rigor.yml in
# parent directories by using a clean temp dir as cwd.
#
# Returns an array of diagnostic hashes, filtered to:
#   - severity in PARITY_SEVERITIES
#   - path == the fixture file path
def run_reference(fixture_path)
  Dir.mktmpdir("rigor-harness-ref") do |tmpdir|
    # Absolute path for unambiguous matching
    abs_fixture = File.expand_path(fixture_path)

    # A plugin-enabled fixture carries a sidecar `.rigor.yml`; pass it via
    # `--config` and add each named plugin's gem `lib/` to the reference's `-I`
    # load path so it can `require` and load the plugin. No sidecar ⇒ no extra
    # flags ⇒ the existing fixtures run exactly as before.
    sidecar = sidecar_config(fixture_path)
    config_flags = sidecar ? ["--config", File.expand_path(sidecar)] : []

    cmd = [
      "ruby",
      "-I", REFERENCE_LIB,
      *reference_plugin_includes(sidecar),
      REFERENCE_EXE,
      "check",
      abs_fixture,
      "--format", "json",
      *config_flags
    ]

    stdout, _stderr, _status = Open3.capture3(*cmd, chdir: tmpdir)

    # Strip any human preamble before the first `{` (defensive; in practice
    # the reference writes preamble to stderr, but guard anyway).
    json_start = stdout.index("{")
    return [] if json_start.nil?
    json_str = stdout[json_start..]

    parsed = JSON.parse(json_str)
    diags = parsed.fetch("diagnostics", [])

    diags.select do |d|
      PARITY_SEVERITIES.include?(d["severity"]) &&
        File.expand_path(d["path"].to_s) == abs_fixture
    end
  end
rescue JSON::ParserError => e
  warn "  WARNING: reference produced invalid JSON for #{fixture_path}: #{e.message}"
  []
end

# Run rigor-rs on a fixture.
# Returns an array of diagnostic hashes in a normalized format compatible with
# the reference output (adds `severity: "error"` since rigor-rs omits it).
def run_rigor_rs(fixture_path)
  abs_fixture = File.expand_path(fixture_path)

  # Mirror the reference: a sidecar `.rigor.yml` is passed via `--config` so a
  # config-gated plugin is exercised. rigor-rs has the plugin RBS vendored, so it
  # needs only the config (no `-I`). No sidecar ⇒ no `--config` ⇒ unchanged.
  sidecar = sidecar_config(fixture_path)
  config_flags = sidecar ? ["--config", File.expand_path(sidecar)] : []

  stdout, stderr, status = Open3.capture3(
    RIGOR_RS_BIN, "check", abs_fixture, "--format", "json", *config_flags
  )

  # rigor-rs outputs the JSON array to stderr (exit 1) or stdout
  output = stdout.strip.empty? ? stderr.strip : stdout.strip

  return [] if output.strip.empty?

  parsed = JSON.parse(output)
  parsed.map do |d|
    # rigor-rs doesn't include severity; treat all emitted diagnostics as errors
    d["severity"] ||= "error"
    d
  end.select do |d|
    PARITY_SEVERITIES.include?(d["severity"]) &&
      File.expand_path(d["path"].to_s) == abs_fixture
  end
rescue JSON::ParserError => e
  warn "  WARNING: rigor-rs produced invalid JSON for #{fixture_path}: #{e.message}"
  []
end

# ---------------------------------------------------------------------------
# Diagnostic key — parity is defined over (rule, line, column)
# ---------------------------------------------------------------------------

DiagKey = Struct.new(:rule, :line, :column) do
  def to_s = "#{rule} @ line #{line}, col #{column}"
end

def diag_key(d)
  DiagKey.new(d["rule"], d["line"].to_i, d["column"].to_i)
end

# ---------------------------------------------------------------------------
# Load divergence registry
# ---------------------------------------------------------------------------

def load_registry(path)
  return [] unless File.exist?(path)

  data = YAML.safe_load_file(path)
  Array(data&.fetch("divergences", []))
rescue => e
  warn "WARNING: could not load divergence registry #{path}: #{e.message}"
  []
end

# Build a set of excused DiagKeys keyed by fixture basename for O(1) lookup.
def build_excused_set(registry, fixture_path)
  fixture_rel = Pathname.new(fixture_path).relative_path_from(Pathname.new(REPO_ROOT)).to_s
  excused = Set.new

  registry.each do |entry|
    next unless entry["fixture"] == fixture_rel

    excused << DiagKey.new(entry["rule"], entry["line"].to_i, entry["column"].to_i)
  end

  excused
end

# ---------------------------------------------------------------------------
# Per-fixture comparison
# ---------------------------------------------------------------------------

FixtureResult = Struct.new(
  :fixture,     # path
  :ref_diags,   # array of raw hashes from reference
  :rs_diags,    # array of raw hashes from rigor-rs
  :matched,     # Set<DiagKey> in both
  :missing,     # Set<DiagKey> in reference but not rigor-rs (coverage gap)
  :extra_unregistered, # Set<DiagKey> in rigor-rs but not reference, unexcused
  :extra_registered    # Set<DiagKey> in rigor-rs but not reference, excused
)

def compare_fixture(fixture_path, ref_diags, rs_diags, excused)
  ref_keys = Set.new(ref_diags.map { diag_key(_1) })
  rs_keys  = Set.new(rs_diags.map  { diag_key(_1) })

  matched   = ref_keys & rs_keys
  missing   = ref_keys - rs_keys
  extra_all = rs_keys - ref_keys

  extra_registered   = extra_all & excused
  extra_unregistered = extra_all - excused

  FixtureResult.new(
    fixture_path,
    ref_diags,
    rs_diags,
    matched,
    missing,
    extra_unregistered,
    extra_registered
  )
end

# ---------------------------------------------------------------------------
# Output helpers
# ---------------------------------------------------------------------------

TICK  = "✓"
CROSS = "✗"
WARN  = "⚠"

def print_fixture_report(result)
  rel = Pathname.new(result.fixture).relative_path_from(Pathname.new(REPO_ROOT)).to_s
  puts "\n#{"=" * 60}"
  puts "Fixture: #{rel}"
  puts "=" * 60

  if result.matched.empty? && result.missing.empty? &&
      result.extra_unregistered.empty? && result.extra_registered.empty?
    puts "  #{TICK} No diagnostics expected, none emitted — clean"
    return
  end

  unless result.matched.empty?
    puts "  #{TICK} Matched (#{result.matched.size}):"
    result.matched.each { |k| puts "      #{k}" }
  end

  unless result.missing.empty?
    puts "  #{WARN} Coverage gaps / missing (#{result.missing.size}) — expected, not a failure:"
    result.missing.each { |k| puts "      #{k}" }
  end

  unless result.extra_registered.empty?
    puts "  #{TICK} Extra (registered divergence, excused) (#{result.extra_registered.size}):"
    result.extra_registered.each { |k| puts "      #{k}" }
  end

  unless result.extra_unregistered.empty?
    puts "  #{CROSS} UNREGISTERED EXTRA — FALSE POSITIVE — REGRESSION (#{result.extra_unregistered.size}):"
    result.extra_unregistered.each { |k| puts "      #{k}" }
  end
end

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main
  ensure_rigor_rs_binary!

  registry = load_registry(DIVERGENCE_REGISTRY_PATH)

  fixtures = Dir[File.join(CORPUS_DIR, "*.rb")].sort
  if fixtures.empty?
    abort("ERROR: no *.rb fixtures found in #{CORPUS_DIR}")
  end

  results = []

  fixtures.each do |fixture_path|
    print "Running fixture: #{File.basename(fixture_path)} ... "
    ref_diags = run_reference(fixture_path)
    rs_diags  = run_rigor_rs(fixture_path)
    excused   = build_excused_set(registry, fixture_path)
    result    = compare_fixture(fixture_path, ref_diags, rs_diags, excused)
    results << result
    puts "done"
  end

  # Per-fixture detailed report
  results.each { |r| print_fixture_report(r) }

  # ---------------------------------------------------------------------------
  # Summary
  # ---------------------------------------------------------------------------
  total_matched          = results.sum { |r| r.matched.size }
  total_missing          = results.sum { |r| r.missing.size }
  total_extra_registered = results.sum { |r| r.extra_registered.size }
  total_extra_unreg      = results.sum { |r| r.extra_unregistered.size }
  total_ref_diags        = results.sum { |r| r.ref_diags.size }

  coverage_pct = if total_ref_diags.zero?
    total_matched.zero? ? 100.0 : 0.0
  else
    (total_matched.to_f / total_ref_diags * 100).round(1)
  end

  puts "\n#{"=" * 60}"
  puts "SUMMARY"
  puts "=" * 60
  puts "  Fixtures run:             #{results.size}"
  puts "  Reference diagnostics:    #{total_ref_diags}"
  puts "  rigor-rs diagnostics:     #{results.sum { |r| r.rs_diags.size }}"
  puts ""
  puts "  #{TICK} Matched:                  #{total_matched}"
  puts "  #{WARN} Coverage gaps (missing):  #{total_missing}  <-- expected; not a failure"
  puts "  #{TICK} Extra (registered):       #{total_extra_registered}  <-- excused divergences"
  puts "  #{CROSS} Extra (unregistered):     #{total_extra_unreg}  <-- FALSE POSITIVES / REGRESSIONS"
  puts ""
  puts "  Coverage: #{total_matched}/#{total_ref_diags} = #{coverage_pct}%"
  puts ""

  if total_extra_unreg > 0
    puts "RESULT: FAIL — #{total_extra_unreg} unregistered false positive(s) detected."
    puts "  rigor-rs must never emit diagnostics the reference does not."
    puts "  Either fix the rigor-rs rule or add a registry entry with an upstream link."
    exit 1
  else
    puts "RESULT: PASS — no unregistered false positives."
    puts "  (Coverage gaps are expected and will shrink as the port progresses.)"
    exit 0
  end
end

main
