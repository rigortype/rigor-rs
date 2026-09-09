# The dead arm of a DECIDABLE Ruby version guard reports nothing (ADR-47 WD5,
# upstream #627 / `d20d6f90`, shipped in `v0.3.8`).
#
# A multi-version gem picks an API generation with `if RUBY_VERSION < "2.7."`.
# The dead arm is honest code for an older Ruby and never runs on the Ruby being
# checked with, so a diagnostic there is a false positive — the `v0.3.4` port
# emitted four of them on the standing sweep (`date-3.5.1/ext/date/extconf.rb`,
# `stringio-3.2.0/ext/stringio/extconf.rb`, concurrent-ruby's `extconf.rb` and
# `promises_spec.rb`).
#
# `Inference::VersionGuard.verdict` is a PURE function of the AST, and
# `CheckRules::DeadVersionGuardArms.filter` then drops every diagnostic whose
# location falls inside the dead arm — for every rule EXCEPT `suppression.*`.
# The evaluator's separate elision of the dead arm's WRITES is typing precision
# rather than FP safety and is deliberately NOT ported (see the last block).
#
# HOST RUBY: the reference folds these guards against ITS OWN runtime while
# rigor-rs bakes `HOST_RUBY_VERSION` / `HOST_RUBY_ENGINE` ("4.0.5" / "ruby",
# overridable by `RIGOR_RUBY_VERSION` / `RIGOR_RUBY_ENGINE`), so the two agree
# only where the guard's verdict is the same on both. Every row below is
# therefore chosen to be STABLE across host Ruby versions — the comparisons are
# against versions far from any Ruby this toolchain runs on, and the one live
# equality tests the ENGINE (see f15). EVERY line — firing and silent alike —
# was oracle-measured at the `v0.3.8` pin (`ffb456b0`), one fresh temp cwd per
# case, `--no-cache`, both reference libs pinned onto `-I` (UPSTREAM.md hazard 1).

# --- STAYS SILENT: the dead arm ---------------------------------------------

# (f1) a modifier `if` whose predicate is false — String semantics, LEXICAL.
"abc".frobnicate_f1 if RUBY_VERSION < "2.7."

# (f3a/f3b) only the dead half goes quiet; the `else` still reports.
if RUBY_VERSION < "3.4"
  "abc".frobnicate_f3a
else
  "abc".frobnicate_f3b
end

# (f4) `unless` runs its body on the FALSEY edge, so a TRUE predicate kills it.
"abc".frobnicate_f4 unless RUBY_ENGINE == "ruby"

# (f5) engine equality that does not hold on this host.
"abc".frobnicate_f5 if RUBY_ENGINE == "jruby"

# (f7) `Gem::Version` on BOTH sides — version semantics, not lexical.
"abc".frobnicate_f7 if Gem::Version.new(RUBY_VERSION) < Gem::Version.new("3.4")

# (f12a) a one-line ternary drops only the dead half (the filter compares
# offsets, not lines).
RUBY_VERSION < "3.4" ? "abc".frobnicate_f12a : "abc".frobnicate_f12b

# (f13) a modifier `return`.
def f13_modifier_return
  return "abc".frobnicate_f13 if RUBY_VERSION < "3.4"
end

# (f14b) an `elsif` link is decided on its own; here the FIRST arm wins, so the
# whole `elsif` chain is dead.
if RUBY_VERSION >= "3.4"
  "abc".frobnicate_f14a
elsif RUBY_VERSION >= "3.0"
  "abc".frobnicate_f14b
end

# (f16) the literal may be on the LEFT.
"abc".frobnicate_f16 if "2.7" > RUBY_VERSION

# (f19) FLOW rules are filtered too, not just the call rules. The control two
# blocks down proves this line is not silent for some unrelated reason.
def f19_dead_assignment_in_a_dead_arm
  if RUBY_VERSION < "3.4"
    y_f19 = 1
  end
end

# (f22) a dead arm containing a nested LIVE guard is still entirely dead.
if RUBY_VERSION < "3.4"
  if RUBY_VERSION >= "3.0"
    "abc".frobnicate_f22
  end
end

# (f23a) …and a LIVE arm containing a nested dead guard drops just the nested
# dead half (f23b, in the nested `else`, still reports).
if RUBY_VERSION >= "3.0"
  if RUBY_VERSION < "3.4"
    "abc".frobnicate_f23a
  else
    "abc".frobnicate_f23b
  end
end

# (f24b) an `unless` with an `else`: a FALSE predicate kills the `else`.
unless RUBY_ENGINE == "jruby"
  "abc".frobnicate_f24a
else
  "abc".frobnicate_f24b
end

# (f26) `Psych::VERSION` is the ONE curated `X::VERSION` the reference reads out
# of its own runtime, and rigor-rs has none. Declining it would REPORT inside
# the arm the oracle killed (this line is silent upstream) — so the port answers
# "unreadable" and drops BOTH arms. See f29 for the price.
"abc".frobnicate_f26 if Gem::Version.new(Psych::VERSION) < Gem::Version.new("3.1.0")

# --- STILL FIRES: the live arm, and every undecidable guard ------------------
#
# Each of these is a control a naive over-broad fix would silence. Filtering
# every `if` whose predicate merely MENTIONS `RUBY_VERSION` would kill f9, f10
# and f17; folding `Gem::Version` against a bare String would kill f18; reading
# a `ConstantRead` by NAME alone would kill f25.

# (f2) the live arm of a true guard.
"abc".frobnicate_f2 if RUBY_VERSION >= "3.0"

# (f6) `RUBY_PLATFORM` is never folded — the analyzer's machine need not be the
# program's.
"abc".frobnicate_f6 if RUBY_PLATFORM =~ /java/

# (f8) a `.to_f` spelling is a different comparison entirely.
"abc".frobnicate_f8 if RUBY_VERSION.to_f < 3.4

# (f9) a `&&` composition is not a single comparison call.
"abc".frobnicate_f9 if RUBY_VERSION < "3.4" && ENV["X"]

# (f10) …nor is a `||` composition.
"abc".frobnicate_f10 if RUBY_VERSION < "3.4" || RUBY_ENGINE == "jruby"

# (f11a/f11b) a `case` SUBJECT is not a version guard; both branches stay live.
case RUBY_VERSION
when "2.7.0" then "abc".frobnicate_f11a
else "abc".frobnicate_f11b
end

# (f15) an equality that is TRUE on this host keeps its body. Deliberately the
# ENGINE and not `RUBY_VERSION == "<patch>"`: the reference folds against its own
# runtime while rigor-rs bakes `HOST_RUBY_VERSION`, so a version-equality row goes
# red the moment the harness host takes a patch bump (measured: this row was
# `RUBY_VERSION == "4.0.5"` and a host moving to 4.0.6 turns it into an
# unregistered false positive — the oracle drops the arm, the port keeps folding
# it true). The engine is stable across every Ruby this toolchain runs on.
"abc".frobnicate_f15 if RUBY_ENGINE == "ruby"

# (f15b) the same operator on the dead edge, equally host-stable: no Ruby this
# analyzer runs on is 1.9.3, so the arm is dropped on both sides.
"abc".frobnicate_f15b if RUBY_VERSION == "1.9.3"

# (f17) a value read through a LOCAL is not a readable operand.
def f17_through_a_local
  v = RUBY_VERSION
  "abc".frobnicate_f17 if v < "3.4"
end

# (f18) a MIXED `Gem::Version` / bare-String pair raises at runtime, so there is
# no arm to pick.
"abc".frobnicate_f18 if Gem::Version.new(RUBY_VERSION) < "3.4"

# (f25) `::RUBY_VERSION` is a `ConstantPathNode` upstream, and the predefined
# read admits `ConstantReadNode` only. rigor-rs lowers both to one
# `ConstantRead`, so the port re-checks the SOURCE SPELLING.
"abc".frobnicate_f25 if ::RUBY_VERSION < "3.4"

# (f27) a `defined?` capability probe is not a comparison.
"abc".frobnicate_f27 if defined?(Ractor)

# (f28) a `!` composition is not a comparison either.
"abc".frobnicate_f28 if !(RUBY_VERSION < "3.4")

# (f19-control) the SAME dead-assignment shape under an undecidable guard, so
# f19's silence above is the filter and not a missing rule.
def f19_control_undecidable_guard
  if RUBY_PLATFORM =~ /java/
    y_f19c = 1
  end
end

# (f21) `suppression.*` is produced AFTER the filter and stays reportable inside
# a dead arm — a malformed `# rigor:disable` marker is an authoring error
# whether or not its code runs. This line fires `suppression.unknown-rule` while
# the call beside it stays silent.
if RUBY_VERSION < "3.4"
  "abc".frobnicate_f21 # rigor:disable not.a.rule
end

# --- REGISTERED COVERAGE GAPS (the reference fires, rigor-rs does not) -------

# (f20) the evaluator ALSO elides the dead arm's WRITES from the post-`if`
# scope, so upstream still types `x` as `1` and reports the typo. That half is
# typing precision, not FP safety, and is deliberately not ported.
def f20_write_elision_not_ported
  x = 1
  x = "s" if RUBY_VERSION < "3.4"
  x.frobnicate_f20
end

# (f29) the price of the f26 decision: for an unreadable `Psych::VERSION` guard
# the port kills BOTH arms, where upstream kills exactly one. Whichever arm the
# oracle keeps is a coverage gap here — never a false positive.
if Psych::VERSION >= "3.1.0"
  "abc".frobnicate_f29a
else
  "abc".frobnicate_f29b
end
