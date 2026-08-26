# The `plugins:`-only reproducer for issue #114 — a project whose declared lane
# is produced entirely by a plugin's effect rows. No annotation anywhere, no
# `effects:` block in the config, no `sig/`: strike the `plugins:` entry and
# this project has no declared lane at all.
#
# A plugin row's labels go to the DECLARED lane, ALWAYS: `attribute_plugin`
# calls `add_declared(Origin.plugin(row.key), labels)` at `unit_scan.rb:258`
# whether or not the row discharges, and it never touches the proven lane. That
# is why the rows below show a non-empty `declared:` beside an empty `effects:`
# on the oracle, and why `rendered_declared` (declared minus what the proven
# lane already admits, `effect_table.rb:41`) drops nothing here.

class Clock
  # PRODUCER 3 (`attribute_plugin`, `unit_scan.rb:258`) — a receiver-path row.
  # `Time.current` is `row(TIME, :current, CLOCK, singleton: true)` in
  # `rigor-activesupport-core-ext`'s `Effects.clock_rows`, whose labels are
  # `["nondet.time", "global.read"]`: the clock read plus the `Time.zone` read
  # the zone-aware spelling implies. DECLARED-MISMATCH on the shipped binary.
  def now
    Time.current
  end

  # PRODUCER 3, the `Date` spelling of the same row — `row(DATE, :current,
  # CLOCK, singleton: true)`. Present as a second instance of the clock family
  # so the fixture does not rest on one row's key resolving.
  def today
    Date.current
  end

  # PRODUCER 3, a DIFFERENT label set from the same plugin —
  # `row(TIME, :zone, ZONE_READ, singleton: true)` in `Effects.zone_rows`, which
  # is `["global.read"]` alone. It keeps the fixture from passing on a port that
  # hard-codes one bundle of labels for every plugin row.
  def zone
    Time.zone
  end

  # THE MUST-STILL-FIRE CONTROL, in the corpus half. `puts` is a core catalogue
  # row that no plugin claims, so this method has NO declared lane on either
  # side and its proven lane is `io.output.stdout` on both. Pre-fix it is the
  # evidence that the port really is reporting this project (a plugin-bearing
  # project that reported nothing would score 0 DECLARED-MISMATCH for the wrong
  # reason); post-fix it is what makes the whole project's silence visible as
  # four `absent-method` UNDERs rather than as an accidental agreement.
  def plain(row)
    puts row
  end
end
