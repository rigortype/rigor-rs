# The `e59b7b89` re-sync of `data/core_overlay/` (vendored as
# `crates/rigor-index/vendor/rbs/overlay/core_overlay/`) added three files the
# reference loads on every run. Without them rigor-rs's surface was strictly
# weaker than the oracle's; with `string_io.rbs` missing, every Enumerable call
# on a StringIO was a live false positive. `hash_rbs3.rbs` is deliberately NOT
# vendored (the reference gates it to rbs `< 4.0`; see the tree's PROVENANCE.md).
#
# Every line below is oracle-measured at the `e59b7b89` pin, one fresh temp cwd,
# `--no-cache`.

require "stringio"

# --- STAYS SILENT -------------------------------------------------------------

# `string_io.rbs`: `class StringIO; include Enumerable[String]; end`.
io = StringIO.new("a\nb\n")
io.detect { |line| line.start_with?("b") }
io.map { |line| line.chomp }
io.each_slice(2).to_a
io.include?("a\n")

# `enumerator.rbs`: the `Enumerator::Lazy` methods CRuby redefines to chain
# lazily, so a terminal only a lazy enumerator answers resolves.
numbers = [1, 2, 3]
numbers.lazy.map { |x| x * 2 }.select { |x| x > 2 }.force
numbers.lazy.map { |x| x * 2 }.eager
numbers.lazy.take(2).force

# `enumerable.rbs`: `detect(ifnone)` fallback overloads.
numbers.detect(-> { 0 }) { |x| x > 5 }

# --- FIRES on both sides ------------------------------------------------------

# The control proving StringIO's surface is enumerated, not gone Dynamic.
io.nonexistent_zzz
