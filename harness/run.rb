#!/usr/bin/env ruby
# frozen_string_literal: true

# harness/run.rb — Differential parity harness (ADR-0002 + ADR-0011)
#
# Compares rigor-rs diagnostic output against the LIVE reference Ruby Rigor
# implementation over a corpus of fixture files. This is the LOCAL
# source-of-truth gate: it needs the reference checkout
# (`REFERENCE_RIGOR_DIR`). The shared logic (how each tool is invoked, the
# (rule,line,column) comparison, the divergence registry) lives in
# `harness/lib.rb` so the snapshot generator (`snapshot.rb`) and the
# reference-free snapshot gate (`run_snapshot.rb`) reuse the IDENTICAL gate
# semantics — see those files.
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

require_relative "lib"

include RigorHarness

def main
  ensure_rigor_rs_binary!

  registry = load_registry(DIVERGENCE_REGISTRY_PATH)

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

  exit(print_summary_and_exit_code(results, mode_label: "live reference"))
end

main
