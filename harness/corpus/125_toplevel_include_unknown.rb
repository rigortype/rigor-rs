# Upstream v0.4.2 (rigor#1706 / issue #1697): a top-level `include` of a
# module neither the project nor the bundled RBS declares mixes an
# unenumerable surface into `Object`, so the reference declines EVERY bare
# top-level call in the project (before the include, inside a top-level `def`,
# and in other files), while a call with an explicit receiver keeps firing: an
# unknown module answers only a bare call (`EXPLICIT_RECEIVER_SKIP`).
#
# Every row is oracle-measured at the `v0.4.2` pin.

tu_before
include SomeGem::Helpers
tu_after
def tu_in_def = tu_called_in_def

# MUST STILL FIRE: an explicit receiver is not answered by an unknown module.
1.tu_int
"s".tu_str
:sym.tu_sym
