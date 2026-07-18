# frozen_string_literal: true

# harness/lib.rb — Shared parity-harness logic (ADR-0002 + ADR-0011)
#
# This module holds the code shared by the differential parity harnesses so the
# live gate (`run.rb`), the snapshot generator (`snapshot.rb`), and the
# snapshot-mode gate (`run_snapshot.rb`) all use ONE definition of:
#
#   - how the reference Ruby Rigor is invoked (`run_reference`)
#   - how rigor-rs is invoked (`run_rigor_rs`)
#   - the sidecar / plugin `-I` wiring (ADR-25)
#   - the comparison semantics: `(rule, line, column)` keys over error/warning
#     severities, with the divergence registry excusing specific extras
#     (`compare_fixture`, `DiagKey`, `build_excused_set`)
#
# IMPORTANT: the comparison semantics here are the single source of truth. The
# snapshot-mode gate (`run_snapshot.rb`) swaps the live `run_reference` for a
# read of a committed `harness/snapshots/NN.json` but reuses the SAME
# comparison code so the gate semantics cannot drift. Do not fork the gate.

require "json"
require "yaml"
require "open3"
require "tmpdir"
require "fileutils"
require "pathname"
require "set"

module RigorHarness
  # -------------------------------------------------------------------------
  # Configuration
  # -------------------------------------------------------------------------

  REPO_ROOT = File.expand_path("..", __dir__)

  # The reference oracle is PINNED as a git submodule at `reference/rigor`,
  # checked out at the upstream `v0.2.6` tag (see UPSTREAM.md). Running the
  # differential against the pinned tag — not a drifting local checkout — makes
  # parity reproducible. `REFERENCE_RIGOR_DIR` still overrides for ad-hoc runs
  # against another checkout. Init the submodule with:
  #   git submodule update --init reference/rigor
  REFERENCE_RIGOR_DIR = File.expand_path(
    ENV.fetch("REFERENCE_RIGOR_DIR", "reference/rigor"),
    REPO_ROOT
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

  SNAPSHOT_DIR = File.expand_path(
    ENV.fetch("SNAPSHOT_DIR", "harness/snapshots"),
    REPO_ROOT
  )

  REFERENCE_LIB = File.join(REFERENCE_RIGOR_DIR, "lib")
  REFERENCE_EXE = File.join(REFERENCE_RIGOR_DIR, "exe", "rigor")

  # Severity levels we care about for parity. Info/hint are excluded.
  PARITY_SEVERITIES = %w[error warning].freeze

  # -------------------------------------------------------------------------
  # Plugin-enabled fixtures (ADR-25)
  # -------------------------------------------------------------------------
  #
  # A fixture `corpus/NN_name.rb` may declare a SIDECAR config
  # `corpus/NN_name.rigor.yml`. When present, the harness runs BOTH tools with
  # that config (`--config <sidecar>`). The reference also needs the plugin
  # gem's `lib/` on its `-I` load path (it `require`s the gem); the rigor-rs
  # binary has the plugin RBS vendored, so it needs only `--config`.
  PLUGIN_LIB_DIRS = {
    "rigor-activesupport-core-ext" =>
      File.join(REFERENCE_RIGOR_DIR, "plugins", "rigor-activesupport-core-ext", "lib")
  }.freeze

  module_function

  # The sidecar config path for a fixture, or `nil` if it has none. Convention:
  # `corpus/NN_name.rb` ⇒ `corpus/NN_name.rigor.yml`.
  def sidecar_config(fixture_path)
    cfg = fixture_path.sub(/\.rb\z/, ".rigor.yml")
    File.exist?(cfg) ? cfg : nil
  end

  # The per-fixture project `sig/` directory, or `nil` if it has none (ADR-0033).
  # Convention: `corpus/NN_name.rb` ⇒ `corpus/NN_name.sig/` (a dir of `.rbs`).
  # When present, BOTH tools run with a cwd whose `sig/` is a copy of it, so the
  # DEFAULT `signature_paths: ["sig"]` picks it up — exercising the real project-
  # signature ingestion path with no per-tool config divergence.
  def sig_dir(fixture_path)
    dir = fixture_path.sub(/\.rb\z/, ".sig")
    File.directory?(dir) ? dir : nil
  end

  # The per-fixture `rbs collection` env dir, or `nil` (ADR-0034). Convention:
  # `corpus/NN_name.rb` ⇒ `corpus/NN_name.collection/`, whose CONTENTS (an
  # `rbs_collection.lock.yaml` + a `.gem_rbs_collection/` tree) are copied into
  # the tool's cwd so the default `rbs_collection.auto_detect` discovers them.
  def collection_dir(fixture_path)
    dir = fixture_path.sub(/\.rb\z/, ".collection")
    File.directory?(dir) ? dir : nil
  end

  # Whether a fixture needs a staged cwd (it ships a project sig/ or an rbs
  # collection). Both tools then run with `chdir: <staged tmpdir>`.
  def staged_fixture?(fixture_path)
    !sig_dir(fixture_path).nil? || !collection_dir(fixture_path).nil?
  end

  # Stage a fixture's project-signature env into `tmpdir` so a tool run with
  # `chdir: tmpdir` and default config picks it up: the `sig/` dir (ADR-0033,
  # staged as `sig/`) and the rbs-collection dir (ADR-0034, its contents copied
  # to the cwd root, dotfiles included). No-op for a fixture that ships neither.
  def stage_fixture_env(fixture_path, tmpdir)
    if (sig = sig_dir(fixture_path))
      FileUtils.cp_r(sig, File.join(tmpdir, "sig"))
    end
    if (coll = collection_dir(fixture_path))
      Dir.each_child(coll) do |child|
        FileUtils.cp_r(File.join(coll, child), File.join(tmpdir, child))
      end
    end
  end

  # The `-I <lib>` flags the reference needs to `require` every plugin named in
  # a sidecar config's `plugins:` list. Returns a flat array (possibly empty).
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

  # -------------------------------------------------------------------------
  # Build rigor-rs if binary is absent
  # -------------------------------------------------------------------------

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

  # -------------------------------------------------------------------------
  # Run each implementation and parse JSON output
  # -------------------------------------------------------------------------

  # Run the reference Ruby Rigor on a fixture, isolated from any .rigor.yml in
  # parent directories by using a clean temp dir as cwd.
  #
  # Returns an array of diagnostic hashes, filtered to:
  #   - severity in PARITY_SEVERITIES
  #   - path == the fixture file path
  def run_reference(fixture_path)
    Dir.mktmpdir("rigor-harness-ref") do |tmpdir|
      abs_fixture = File.expand_path(fixture_path)

      # ADR-0033/0034: stage the fixture's project sig/ and/or rbs collection so
      # the reference's defaults (resolved against cwd = tmpdir) ingest them.
      stage_fixture_env(fixture_path, tmpdir)

      sidecar = sidecar_config(fixture_path)
      config_flags = sidecar ? ["--config", File.expand_path(sidecar)] : []

      cmd = [
        "ruby",
        "-I", REFERENCE_LIB,
        # Pin the CHECKOUT's bundled rigor-rbs-inline onto the load path
        # UNCONDITIONALLY (upstream issue #194): the ADR-93 auto-wire
        # `require "rigor-rbs-inline"` otherwise resolves a stale installed
        # rigortype gem's pre-annotation-gate plugin copy, which synthesizes
        # untyped skeletons for every file and poisons the oracle. Harmless at
        # pre-auto-wire pins (nothing requires it); load-bearing after.
        "-I", File.join(REFERENCE_RIGOR_DIR, "plugins", "rigor-rbs-inline", "lib"),
        *reference_plugin_includes(sidecar),
        REFERENCE_EXE,
        "check",
        abs_fixture,
        "--format", "json",
        *config_flags
      ]

      stdout, _stderr, _status = Open3.capture3(*cmd, chdir: tmpdir)
      # Diagnostic messages can carry non-ASCII (e.g. the em-dash in
      # `suppression.unknown-rule`); Open3 hands back ASCII-8BIT, so tag UTF-8
      # before slicing/parsing or `JSON.parse` raises on the raw bytes.
      stdout = stdout.dup.force_encoding("UTF-8")

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
  # Returns an array of diagnostic hashes in a normalized format compatible
  # with the reference output (adds `severity: "error"` since rigor-rs omits it).
  def run_rigor_rs(fixture_path)
    abs_fixture = File.expand_path(fixture_path)

    sidecar = sidecar_config(fixture_path)
    config_flags = sidecar ? ["--config", File.expand_path(sidecar)] : []
    cmd = [RIGOR_RS_BIN, "check", abs_fixture, "--format", "json", *config_flags]

    # ADR-0033/0034: a fixture shipping a project sig/ or an rbs collection runs
    # in a clean tmpdir staged with a copy of that env, so rigor-rs's defaults
    # (`signature_paths: ["sig"]`, `rbs_collection.auto_detect`) ingest it — the
    # SAME staging the reference gets, keeping the two implementations symmetric.
    # A fixture without either runs as before (no chdir).
    if staged_fixture?(fixture_path)
      Dir.mktmpdir("rigor-harness-rs") do |tmpdir|
        stage_fixture_env(fixture_path, tmpdir)
        stdout, stderr, _status = Open3.capture3(*cmd, chdir: tmpdir)
        parse_rigor_rs_diags(stdout, stderr, abs_fixture, fixture_path)
      end
    else
      stdout, stderr, _status = Open3.capture3(*cmd)
      parse_rigor_rs_diags(stdout, stderr, abs_fixture, fixture_path)
    end
  end

  # Parse rigor-rs's JSON stdout (falling back to stderr) into the normalized
  # diagnostic array shared by both run paths. rigor-rs omits `severity`, so it
  # defaults to `"error"`; diagnostics are filtered to parity severities and the
  # fixture file.
  def parse_rigor_rs_diags(stdout, stderr, abs_fixture, fixture_path)
    # Tag UTF-8 (Open3 returns ASCII-8BIT) so a non-ASCII message byte such as
    # the em-dash in `suppression.unknown-rule` does not break `JSON.parse`.
    stdout = stdout.dup.force_encoding("UTF-8")
    stderr = stderr.dup.force_encoding("UTF-8")
    output = stdout.strip.empty? ? stderr.strip : stdout.strip
    return [] if output.strip.empty?

    parsed = JSON.parse(output)
    parsed.map do |d|
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

  # -------------------------------------------------------------------------
  # Diagnostic key — parity is defined over (rule, line, column)
  # -------------------------------------------------------------------------

  DiagKey = Struct.new(:rule, :line, :column) do
    def to_s = "#{rule} @ line #{line}, col #{column}"
  end

  def diag_key(d)
    DiagKey.new(d["rule"], d["line"].to_i, d["column"].to_i)
  end

  # -------------------------------------------------------------------------
  # Load divergence registry
  # -------------------------------------------------------------------------

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

  # -------------------------------------------------------------------------
  # Per-fixture comparison
  # -------------------------------------------------------------------------

  FixtureResult = Struct.new(
    :fixture,            # path
    :ref_diags,          # array of raw hashes from reference (or snapshot)
    :rs_diags,           # array of raw hashes from rigor-rs
    :matched,            # Set<DiagKey> in both
    :missing,            # Set<DiagKey> in reference but not rigor-rs (gap)
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

  # -------------------------------------------------------------------------
  # Snapshot serialization (ADR-0002, §14 quality-management track c)
  # -------------------------------------------------------------------------
  #
  # A snapshot pins what `run_reference` returns for a fixture: the reference's
  # expected diagnostic set in a stable, sorted, pretty-printed form so that
  # re-generation is a no-op when nothing changed.
  #
  # The COMPARISON keys on (rule, line, column) like the live gate; `message`
  # and `severity` are stored too so the snapshot is human-reviewable.

  # Path to a fixture's committed snapshot: corpus/NN_name.rb ⇒
  # snapshots/NN_name.json.
  def snapshot_path(fixture_path)
    base = File.basename(fixture_path, ".rb")
    File.join(SNAPSHOT_DIR, "#{base}.json")
  end

  # Project a raw reference diagnostic to the stable snapshot shape. Only the
  # fields the gate needs (rule/line/column) plus human-review aids
  # (message/severity). `path` is deliberately omitted: it is absolute and
  # machine-specific, and `run_reference` already filtered to the fixture.
  def snapshot_diag(d)
    {
      "rule"     => d["rule"],
      "line"     => d["line"].to_i,
      "column"   => d["column"].to_i,
      "severity" => d["severity"],
      "message"  => d["message"]
    }
  end

  # Deterministic ordering: (rule, line, column) then message as a tiebreaker.
  def sort_snapshot_diags(diags)
    diags.sort_by do |d|
      [d["rule"].to_s, d["line"].to_i, d["column"].to_i, d["message"].to_s]
    end
  end

  # Serialize a fixture's reference diagnostics to the canonical snapshot JSON
  # string (sorted, pretty, trailing newline) — deterministic.
  def snapshot_json(fixture_path, ref_diags)
    payload = {
      "fixture"     => Pathname.new(File.expand_path(fixture_path))
                         .relative_path_from(Pathname.new(REPO_ROOT)).to_s,
      "diagnostics" => sort_snapshot_diags(ref_diags.map { snapshot_diag(_1) })
    }
    "#{JSON.pretty_generate(payload)}\n"
  end

  # Load a committed snapshot and return its diagnostics array (raw hashes with
  # rule/line/column/severity/message). Aborts if the snapshot is missing — a
  # snapshot-mode run requires every fixture to be pinned.
  def load_snapshot(fixture_path)
    path = snapshot_path(fixture_path)
    unless File.exist?(path)
      abort("ERROR: missing snapshot #{path}\n" \
            "  Regenerate with: ruby harness/snapshot.rb (needs the reference checkout).")
    end
    data = JSON.parse(File.read(path, encoding: "UTF-8"))
    Array(data["diagnostics"])
  end

  # -------------------------------------------------------------------------
  # Output helpers
  # -------------------------------------------------------------------------

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

  # Print the shared SUMMARY block and return the process exit code (0 PASS /
  # 1 FAIL). `mode_label` distinguishes the live gate from snapshot mode in the
  # header.
  def print_summary_and_exit_code(results, mode_label: "live reference")
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
    puts "SUMMARY (#{mode_label})"
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
      1
    else
      puts "RESULT: PASS — no unregistered false positives."
      puts "  (Coverage gaps are expected and will shrink as the port progresses.)"
      0
    end
  end

  # Enumerate corpus fixtures (sorted). Aborts if none found.
  def fixtures
    found = Dir[File.join(CORPUS_DIR, "*.rb")].sort
    abort("ERROR: no *.rb fixtures found in #{CORPUS_DIR}") if found.empty?
    found
  end
end
