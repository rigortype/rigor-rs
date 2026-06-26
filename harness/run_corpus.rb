#!/usr/bin/env ruby
# frozen_string_literal: true

# harness/run_corpus.rb — Scaled differential corpus harness (Audit R4)
#
# Runs the reference Ruby Rigor and rigor-rs over REAL Ruby OSS corpora to
# surface false positives (diagnostics rigor-rs emits that the reference does
# NOT emit on the same file).
#
# Usage:
#   ruby harness/run_corpus.rb                 # uses built-in corpus list
#   ruby harness/run_corpus.rb /path/to/dir …  # custom corpus directories
#
# Env vars:
#   CORPUS_LIMIT          max .rb files to sample per corpus dir (default: 80)
#   REFERENCE_RIGOR_DIR   path to Ruby rigor checkout
#                         (default: /Users/megurine/repo/ruby/rigor)
#   RIGOR_RS_BIN          path to rigor-rs binary
#                         (default: target/debug/rigor in repo root)
#
# Exit codes:
#   0  — no false positives found
#   1  — one or more false positives detected (architect must fix crate)
#
# ─────────────────────────────────────────────────────────────────────────────

require "json"
require "open3"
require "tmpdir"
require "fileutils"
require "pathname"
require "set"

# ─── Configuration ───────────────────────────────────────────────────────────

REPO_ROOT = File.expand_path("..", __dir__)

REFERENCE_RIGOR_DIR = ENV.fetch(
  "REFERENCE_RIGOR_DIR",
  "/Users/megurine/repo/ruby/rigor"
)
REFERENCE_LIB = File.join(REFERENCE_RIGOR_DIR, "lib")
REFERENCE_EXE = File.join(REFERENCE_RIGOR_DIR, "exe", "rigor")

RIGOR_RS_BIN = File.expand_path(
  ENV.fetch("RIGOR_RS_BIN", "target/debug/rigor"),
  REPO_ROOT
)

# Max .rb files sampled per corpus directory (configurable via env)
CORPUS_LIMIT = (ENV["CORPUS_LIMIT"] || "80").to_i

# Severity levels that count for parity
PARITY_SEVERITIES = %w[error warning].freeze

# Built-in corpus list (order = run order)
DEFAULT_CORPORA = [
  {
    label:   "rigor/examples",
    dir:     File.join(REFERENCE_RIGOR_DIR, "examples"),
    limit:   CORPUS_LIMIT
  },
  {
    label:   "rigor/lib/rigor/type",
    dir:     File.join(REFERENCE_RIGOR_DIR, "lib", "rigor", "type"),
    limit:   CORPUS_LIMIT
  },
  {
    label:   "mastodon/app/models",
    dir:     "/Users/megurine/repo/ruby/mastodon/app/models",
    limit:   60   # explicit cap for the Rails corpus
  }
].freeze

# ─── Build rigor-rs if needed ─────────────────────────────────────────────────

def ensure_binary!
  return if File.executable?(RIGOR_RS_BIN)

  warn "rigor-rs binary not found at #{RIGOR_RS_BIN}; building..."
  Dir.chdir(REPO_ROOT) do
    system("cargo build --offline -p rigor-cli") or
      abort("ERROR: cargo build failed")
  end
  abort("ERROR: binary still missing after build: #{RIGOR_RS_BIN}") unless
    File.executable?(RIGOR_RS_BIN)
  warn "Build OK: #{RIGOR_RS_BIN}"
end

# ─── File collection ──────────────────────────────────────────────────────────

# Collect up to +limit+ .rb files from +dir+ (recursive, sorted for
# reproducibility).
def collect_files(dir, limit)
  all = Dir[File.join(dir, "**", "*.rb")].sort
  all.first(limit)
end

# ─── Reference runner ─────────────────────────────────────────────────────────

# Run the reference over a TEMP DIRECTORY (which contains the sampled files)
# using a separate, clean tmpdir as cwd so no .rigor.yml is discovered.
#
# Returns a Hash { canonical_path => [diag, …] } where canonical_path is the
# ORIGINAL file path (not the temp copy).
def run_reference_batch(file_map, tmpdir_with_files)
  # cwd is a separate throw-away dir to isolate config discovery
  Dir.mktmpdir("rigor-corpus-ref-cwd") do |cwd|
    # Pass the staging directory — reference accepts a directory and analyzes
    # all .rb files recursively.
    cmd = [
      "ruby",
      "-I", REFERENCE_LIB,
      REFERENCE_EXE,
      "check",
      tmpdir_with_files,
      "--format", "json"
    ]

    stdout, stderr, _status = Open3.capture3(*cmd, chdir: cwd)

    # Preamble (if any) goes to stderr; strip anything before first `{` in stdout
    json_start = stdout.index("{")
    if json_start.nil?
      warn "  [reference] No JSON in stdout. stderr snippet: #{stderr[0, 300]}"
      return {}
    end

    begin
      parsed = JSON.parse(stdout[json_start..])
    rescue JSON::ParserError => e
      warn "  [reference] JSON parse error: #{e.message}"
      return {}
    end

    result = Hash.new { |h, k| h[k] = [] }
    Array(parsed["diagnostics"]).each do |d|
      next unless PARITY_SEVERITIES.include?(d["severity"])

      # The diagnostic path points at the TEMP copy — translate back to
      # original path via file_map (temp_path => original_path).
      temp_path = File.expand_path(d["path"].to_s)
      orig_path = file_map[temp_path]
      next if orig_path.nil?   # unexpected path — skip

      result[orig_path] << d
    end
    result
  end
rescue => e
  warn "  [reference] Unexpected error: #{e.message}"
  {}
end

# ─── rigor-rs runner ─────────────────────────────────────────────────────────

# Run rigor-rs over all temp copies in one process (RBS loaded once).
# Returns Hash { original_path => [diag, …] }.
def run_rigorrs_batch(file_map)
  # file_map is temp_path => orig_path; we pass temp paths to rigor-rs
  temp_files = file_map.keys

  return {} if temp_files.empty?

  stdout, stderr, _status = Open3.capture3(RIGOR_RS_BIN, "check", *temp_files, "--format", "json")

  output = stdout.strip.empty? ? stderr.strip : stdout.strip
  return {} if output.empty?

  # Find JSON array or object start
  json_start = [output.index("["), output.index("{")].compact.min
  return {} if json_start.nil?

  begin
    parsed = JSON.parse(output[json_start..])
  rescue JSON::ParserError => e
    warn "  [rigor-rs] JSON parse error: #{e.message}"
    return {}
  end

  result = Hash.new { |h, k| h[k] = [] }
  Array(parsed).each do |d|
    next unless PARITY_SEVERITIES.include?(d["severity"].to_s)

    temp_path = File.expand_path(d["path"].to_s)
    orig_path = file_map[temp_path]
    next if orig_path.nil?

    result[orig_path] << d
  end
  result
end

# ─── Diagnostic key ──────────────────────────────────────────────────────────

DiagKey = Struct.new(:rule, :line, :col) do
  def to_s
    "#{rule} @ line #{line}, col #{col}"
  end
end

def diag_key(d)
  DiagKey.new(d["rule"].to_s, d["line"].to_i, d["column"].to_i)
end

# ─── Corpus runner ───────────────────────────────────────────────────────────

CorpusResult = Struct.new(
  :label,
  :files_scanned,
  :ref_total,       # total reference diag count (error/warning)
  :rs_total,        # total rigor-rs diag count
  :matched,         # count of (rule,line,col) in both
  :missing,         # count in reference but not rigor-rs
  :false_positives, # Array of {file:, key:, diag:} — rigor-rs ONLY
  keyword_init: true
)

def run_corpus(label:, dir:, limit:)
  unless Dir.exist?(dir)
    warn "  [SKIP] Directory not found: #{dir}"
    return CorpusResult.new(
      label: label, files_scanned: 0,
      ref_total: 0, rs_total: 0,
      matched: 0, missing: 0, false_positives: []
    )
  end

  files = collect_files(dir, limit)
  if files.empty?
    warn "  [SKIP] No .rb files found in #{dir}"
    return CorpusResult.new(
      label: label, files_scanned: 0,
      ref_total: 0, rs_total: 0,
      matched: 0, missing: 0, false_positives: []
    )
  end

  # Stage files into a temp dir (flat mirror using escaped paths to avoid
  # collisions). Use a deterministic name based on original path.
  Dir.mktmpdir("rigor-corpus-stage") do |stagedir|
    # file_map: temp_path => orig_path
    file_map = {}
    files.each_with_index do |orig, idx|
      # Use a numeric prefix + original basename to keep names readable
      temp_name = format("%04d_%s", idx, File.basename(orig))
      temp_path = File.join(stagedir, temp_name)
      FileUtils.cp(orig, temp_path)
      file_map[temp_path] = orig
    end

    # Run both tools
    ref_map = run_reference_batch(file_map, stagedir)
    rs_map  = run_rigorrs_batch(file_map)

    # Aggregate
    ref_total = 0
    rs_total  = 0
    matched   = 0
    missing   = 0
    fps       = []

    files.each do |orig|
      ref_diags = ref_map[orig] || []
      rs_diags  = rs_map[orig]  || []

      ref_total += ref_diags.size
      rs_total  += rs_diags.size

      ref_keys = Set.new(ref_diags.map { diag_key(_1) })
      rs_keys  = Set.new(rs_diags.map  { diag_key(_1) })

      matched  += (ref_keys & rs_keys).size
      missing  += (ref_keys - rs_keys).size

      extra = rs_keys - ref_keys
      extra.each do |key|
        # Find the corresponding raw diag for message/details
        raw = rs_diags.find { |d| diag_key(d) == key }
        fps << { file: orig, key: key, diag: raw }
      end
    end

    CorpusResult.new(
      label:          label,
      files_scanned:  files.size,
      ref_total:      ref_total,
      rs_total:       rs_total,
      matched:        matched,
      missing:        missing,
      false_positives: fps
    )
  end
end

# ─── Reporting helpers ────────────────────────────────────────────────────────

SEP = ("─" * 70).freeze
BOLD_SEP = ("═" * 70).freeze

def print_corpus_report(result)
  puts
  puts BOLD_SEP
  puts "CORPUS: #{result.label}"
  puts BOLD_SEP
  puts "  Files scanned:           #{result.files_scanned}"
  puts "  Reference diagnostics:   #{result.ref_total}"
  puts "  rigor-rs diagnostics:    #{result.rs_total}"
  puts "  Matched (both agree):    #{result.matched}"
  coverage_pct = if result.ref_total.zero?
    result.matched.zero? ? 100.0 : 0.0
  else
    (result.matched.to_f / result.ref_total * 100).round(1)
  end
  puts "  Coverage gaps (missing): #{result.missing}  [expected — only 3 rules implemented]"
  puts "  Coverage %:              #{coverage_pct}%"
  puts

  fp_count = result.false_positives.size
  if fp_count.zero?
    puts "  FALSE POSITIVES: 0  ✓  (no false positives in this corpus)"
  else
    puts "  FALSE POSITIVES: #{fp_count}  ✗  ← rigor-rs emits these; reference does NOT"
    puts
    puts "  Full false-positive list:"
    puts "  " + SEP[0..65]
    result.false_positives.each_with_index do |fp, i|
      rel = begin
        Pathname.new(fp[:file]).relative_path_from(Pathname.new("/Users/megurine/repo")).to_s
      rescue
        fp[:file]
      end
      d = fp[:diag] || {}
      puts "  [FP #{i + 1}] #{rel}:#{fp[:key].line}:#{fp[:key].col}"
      puts "         rule:    #{fp[:key].rule}"
      puts "         message: #{d["message"]}"
      method_name    = d["method_name"]
      receiver_type  = d["receiver_type"]
      puts "         method:  #{method_name}" if method_name
      puts "         receiver:#{receiver_type}" if receiver_type
      puts
    end
  end
end

# ─── Main ─────────────────────────────────────────────────────────────────────

def main
  ensure_binary!

  # Determine corpora: from ARGV or built-in list
  corpora = if ARGV.any?
    ARGV.map.with_index do |dir, i|
      { label: "argv[#{i}]: #{dir}", dir: dir, limit: CORPUS_LIMIT }
    end
  else
    DEFAULT_CORPORA
  end

  puts BOLD_SEP
  puts "rigor-rs Scaled Differential Corpus Harness  (Audit R4)"
  puts BOLD_SEP
  puts "  Reference: #{REFERENCE_EXE}"
  puts "  rigor-rs:  #{RIGOR_RS_BIN}"
  puts "  Corpora:   #{corpora.size}"
  puts "  Limit:     #{CORPUS_LIMIT} files/corpus (overridden per-corpus where noted)"
  puts

  all_results = corpora.map do |corpus|
    print "Running corpus [#{corpus[:label]}] (limit=#{corpus[:limit]})... "
    $stdout.flush
    result = run_corpus(**corpus.transform_keys(&:to_sym))
    puts "done (#{result.files_scanned} files)"
    result
  end

  # Per-corpus detailed report
  all_results.each { |r| print_corpus_report(r) }

  # ─── Grand summary ───────────────────────────────────────────────────────
  total_files   = all_results.sum(&:files_scanned)
  total_ref     = all_results.sum(&:ref_total)
  total_rs      = all_results.sum(&:rs_total)
  total_matched = all_results.sum(&:matched)
  total_missing = all_results.sum(&:missing)
  all_fps       = all_results.flat_map(&:false_positives)
  total_fps     = all_fps.size

  grand_coverage = if total_ref.zero?
    total_matched.zero? ? 100.0 : 0.0
  else
    (total_matched.to_f / total_ref * 100).round(1)
  end

  puts
  puts BOLD_SEP
  puts "GRAND SUMMARY (all corpora)"
  puts BOLD_SEP
  puts "  Total files scanned:     #{total_files}"
  puts "  Total ref diagnostics:   #{total_ref}"
  puts "  Total rigor-rs diags:    #{total_rs}"
  puts "  Matched:                 #{total_matched}"
  puts "  Coverage gaps (missing): #{total_missing}"
  puts "  Grand coverage %:        #{grand_coverage}%"
  puts

  if total_fps.zero?
    puts "  *** FALSE POSITIVES: 0 — STRONG RESULT ***"
    puts "  rigor-rs emitted zero false positives across #{total_files} real Ruby files."
    puts
    puts "RESULT: PASS"
    exit 0
  else
    puts "  *** FALSE POSITIVES: #{total_fps} — ARCHITECT ACTION REQUIRED ***"
    puts
    # Print condensed FP list for quick scanning
    all_fps.each_with_index do |fp, i|
      d = fp[:diag] || {}
      rel = begin
        Pathname.new(fp[:file]).relative_path_from(Pathname.new("/Users/megurine/repo")).to_s
      rescue
        fp[:file]
      end
      puts "  [FP #{i + 1}] #{rel}:#{fp[:key].line}:#{fp[:key].col}  #{fp[:key].rule}  — #{d["message"]}"
    end
    puts
    puts "RESULT: FAIL — #{total_fps} false positive(s) detected across #{total_files} files."
    puts "  Do NOT suppress via divergence-registry. Fix the underlying crate rule."
    exit 1
  end
end

main
