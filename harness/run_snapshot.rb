#!/usr/bin/env ruby
# frozen_string_literal: true

# harness/run_snapshot.rb — Snapshot-mode parity gate (ADR-0002, §14 track c)
#
# The CI parity gate. Identical to `harness/run.rb` EXCEPT it loads each
# fixture's pinned reference diagnostics from a committed
# `harness/snapshots/NN_name.json` instead of running the live reference. It
# then runs the built rigor-rs binary and applies the IDENTICAL comparison
# logic (shared from `harness/lib.rb`): false positives (unregistered extras)
# FAIL, missing diagnostics are coverage gaps and never fail, the divergence
# registry excuses specific extras, comparison keys on (rule, line, column)
# over error/warning severities.
#
# Dependencies: the built `rigor` binary + Ruby + the committed snapshots.
#   NO reference checkout — this script never touches REFERENCE_RIGOR_DIR. It
#   only reads harness/snapshots/ and runs RIGOR_RS_BIN.
#
# Snapshots are regenerated locally by `harness/snapshot.rb` from the live
# reference; `harness/run.rb` (live) remains the local source-of-truth gate.
#
# Usage: ruby harness/run_snapshot.rb   (from repo root)

require_relative "lib"

include RigorHarness

def main
  ensure_rigor_rs_binary!

  registry = load_registry(DIVERGENCE_REGISTRY_PATH)

  results = []

  fixtures.each do |fixture_path|
    print "Running fixture (snapshot): #{File.basename(fixture_path)} ... "
    ref_diags = load_snapshot(fixture_path)   # pinned reference, no live run
    rs_diags  = run_rigor_rs(fixture_path)
    excused   = build_excused_set(registry, fixture_path)
    result    = compare_fixture(fixture_path, ref_diags, rs_diags, excused)
    results << result
    puts "done"
  end

  results.each { |r| print_fixture_report(r) }

  exit(print_summary_and_exit_code(results, mode_label: "snapshot — no reference"))
end

main
