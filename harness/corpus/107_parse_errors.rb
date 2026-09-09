# A file Prism CANNOT parse. Every other fixture in this corpus parses, so this
# is the first one that does not — `docs/notes/20260731-…fixture-corpus-blind-
# spot.md` is about exactly this class of hole.
#
# What it pins: the reference reports Prism's parse errors as diagnostics
# (`Runner#parse_diagnostics`) — one row per RAW Prism error, 1:1, no filtering
# and no dedupe — with `severity: error`, `rule: nil`, `line` =
# `error.location.start_line`, `column` = `error.location.start_column + 1`, and
# Prism's message verbatim. rigor-rs used to answer `[]` here and exit 0, so a CI
# gate built on the port read a file it could not read as clean.
#
# NOT ported alongside it: the file is still NOT analysed — no index, no
# inference, no rule diagnostics (the standing decision at the
# `result.errors().next().is_some()` guard in `crates/rigor-cli/src/main.rs`;
# Prism's error recovery invents bindings the rules over-fire on). So every row
# below is a parse error and there are no others.
#
# `rule: nil` must serialise as JSON `null`, never `""`: `harness/lib.rb`'s
# `DiagKey` is `(rule, line, column)`, so `""` would be a different key from the
# reference's `nil` and this fixture would score as a coverage gap AND an
# unregistered extra at once.
#
# Every row is oracle-measured at the `v0.3.8` pin (`ffb456b0`), from a fresh
# temp cwd, `--no-cache`, both reference libs pinned onto `-I` (UPSTREAM.md
# hazard 1). The five rows, in the reference's own order:
#
#   L48:1  error  rule=nil  unexpected 'else', ignoring it
#   L50:1  error  rule=nil  unexpected 'end', ignoring it
#   L59:15 error  rule=nil  expected a delimiter to close the parameters
#   L59:16 error  rule=nil  unexpected write target
#   L59:23 error  rule=nil  unexpected local variable or method, expecting
#                           end-of-input
#
# Prism's `warnings` are NOT reported — measured, not assumed: a file with one
# Prism warning and zero errors gives the reference zero parse rows.

# --- (1) the survey corpus's shape: a dangling `else` -------------------------
#
# `rigor-survey/Ruby/searches/{binary,linear,ternary}_search.rb` and
# `fibonacci_search.rb` are all this: a modifier `if` followed by an `else`
# that has no `if` to attach to. It is bucket 01 of the v0.3.8 gap adjudication
# (9 rows, 4 files).

haystack = [1, 2, 3]
needle = 2

puts "found" if haystack.include?(needle)
else
  puts "missing"
end

# --- (2) a broken parameter list: SEVERAL errors on ONE line ------------------
#
# The 1:1 contract has no dedupe and no per-line collapsing, so a line carrying
# three Prism errors contributes three rows. This is also the shape the standing
# skip decision names: Prism recovers `def f int a, int b` into a body that
# references a never-bound `b`, which is why the file is still not analysed.

def sum_of int a, int b
  a + b
end
