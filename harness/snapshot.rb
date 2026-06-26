#!/usr/bin/env ruby
# frozen_string_literal: true

# harness/snapshot.rb — Reference-snapshot generator (ADR-0002, §14 track c)
#
# For each corpus fixture, runs the LIVE reference Ruby Rigor (exactly as
# `harness/run.rb` does — same sidecar/`--config` + plugin `-I` wiring) and
# writes its expected diagnostic set to `harness/snapshots/NN_name.json` in a
# stable, sorted, pretty-printed form. These committed snapshots let CI run the
# parity gate WITHOUT the reference checkout (see `harness/run_snapshot.rb`).
#
# This step REQUIRES the reference checkout (`REFERENCE_RIGOR_DIR`). It is a
# regenerable LOCAL step: re-run it whenever a fixture changes or the pinned
# reference updates, then commit the snapshot diff. Output is deterministic, so
# a no-op regeneration produces no diff.
#
# Usage:
#   ruby harness/snapshot.rb            # regenerate every snapshot
#   ruby harness/snapshot.rb --check    # verify snapshots are up to date
#                                        # (exit 1 if any differ — for CI/pre-commit)
#
# Env vars: same as harness/run.rb (REFERENCE_RIGOR_DIR, RIGOR_RS_BIN unused
# here, CORPUS_DIR, SNAPSHOT_DIR).

require_relative "lib"

include RigorHarness

def main
  check_only = ARGV.include?("--check")

  FileUtils.mkdir_p(SNAPSHOT_DIR) unless check_only

  drifted = []
  written = 0

  fixtures.each do |fixture_path|
    base = File.basename(fixture_path)
    print "Snapshotting #{base} ... "

    ref_diags = run_reference(fixture_path)
    json = snapshot_json(fixture_path, ref_diags)
    path = snapshot_path(fixture_path)

    existing = File.exist?(path) ? File.read(path) : nil

    if existing == json
      puts "up-to-date (#{ref_diags.size} diag#{ref_diags.size == 1 ? "" : "s"})"
    elsif check_only
      drifted << path
      puts "DRIFT"
    else
      File.write(path, json)
      written += 1
      puts "written (#{ref_diags.size} diag#{ref_diags.size == 1 ? "" : "s"})"
    end
  end

  puts "\n#{"=" * 60}"
  if check_only
    if drifted.empty?
      puts "SNAPSHOTS: up to date (no drift)."
      exit 0
    else
      puts "SNAPSHOTS: DRIFT in #{drifted.size} file(s):"
      drifted.each { |p| puts "  #{Pathname.new(p).relative_path_from(Pathname.new(REPO_ROOT))}" }
      puts "Regenerate with: ruby harness/snapshot.rb"
      exit 1
    end
  else
    puts "SNAPSHOTS: #{written} written, #{fixtures.size - written} unchanged."
    puts "Snapshot dir: #{Pathname.new(SNAPSHOT_DIR).relative_path_from(Pathname.new(REPO_ROOT))}"
    exit 0
  end
end

main
