# The vendored plugin RBS must stay BYTE-IDENTICAL to the reference's, and this
# fixture is what makes that measurable. Sidecar: 98_plugin_sig_parity.rigor.yml.
#
# `crates/rigor-index/vendor/plugins/` is a pin-tracking surface — exactly like
# `vendor/rbs/overlay/` — but nothing re-synced it and nothing graded it. It was
# vendored 2026-06-26 from a LOCAL rigor working checkout (not the pinned
# submodule: the same hazard UPSTREAM.md records as hazard 3) and never moved
# again, while upstream's copy grew. Measured at the `v0.3.4` pin, that drift was
# **10 live false positives** on the ten selectors below — and neither sweep tool
# can see them, because `fp_audit.py` runs both sides from a clean cwd with no
# `.rigor.yml`, so no plugin is ever loaded. Fixture 17 was the whole of the
# plugin surface's coverage, and it exercises three lines.
#
# Every line below is oracle-measured at the `v0.3.4` pin (`b10bd5df`), with the
# reference's own `rigor-activesupport-core-ext` lib pinned onto `-I`.

# --- STAYS SILENT: declared by the plugin's RBS ------------------------------

# The ten that were false positives before the re-sync.
"abc".titlecase
"abc".dasherize
"abc".upcase_first
"a-b".remove("-")
"a-b".remove!("-")
1.in?([1, 2])
Time.now.advance(days: 1)
Time.now.all_day
Date.today.advance(days: 1)
Date.today.all_day

# Already covered before the re-sync — these must not regress.
"abc".underscore
3.minutes

# ActiveSupport WIDENS the arity of `Date#to_time` to `to_time(form = :local)`.
# The reference carries the row as a full redeclaration at this pin, which is
# the defect below; upstream's fix (master `44bd23bf`, #437) turns it into an
# overload continuation so both arities keep resolving. Either way this call
# must never draw an arity diagnostic.
Date.today.to_time(:utc)

# --- FIRES on both sides ------------------------------------------------------

# Not declared anywhere: the control that proves the plugin's RBS is loaded
# rather than the whole receiver having gone Dynamic.
"abc".frobnicate_zzz

# A chained selector's declared return type still witnesses (fixture 17's shape,
# kept here so the two fixtures fail independently).
"abc".squish.frobsquish_zzz

# --- REGISTERED DIVERGENCE (upstream #437) -----------------------------------

# rigor-rs FIRES on both; the reference at this pin is SILENT on both.
#
# The plugin's own full `Date#to_time` declaration redeclares the row rbs's
# `stdlib/date` already ships, `RBS::DefinitionBuilder` raises on the duplicate,
# the reference fails soft, and `Date` AND `DateTime` both degrade to
# `Dynamic[top]` — so real methods and typos alike stop being witnessed for every
# project that activates the plugin. rigor-rs's own ingestion does not share that
# raise-on-duplicate, so it keeps both definitions and witnesses the typos.
#
# This is the FIXED-UPSTREAM direction: master `44bd23bf` makes the row an
# overload continuation and the reference then fires on both lines exactly as
# rigor-rs does (measured on a master worktree). The registry entry retires
# itself at the pin bump that lands it.
Date.today.frobdate_zzz
DateTime.now.frobdatetime_zzz
