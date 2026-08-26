# The `effects.envelopes:` reproducer for issue #114 — a project with a
# declared lane and NOT ONE effect annotation anywhere. `sig/` is absent on
# purpose and no `.rb` line here may contain `%a{`, because the port's
# `carries_effect_annotations` line scan would then fire for the wrong reason
# and the project would stop testing the arm it exists to test.
#
# The declared lane is the CALLER's (`unit_scan.rb:328-338`; the slice-1
# catalogue probe § 7): an envelope on the CALLEE joins the CALLER's `≤` lane at
# the call site, so the annotated/bounded method's own row stays empty.

module Svc
  class Repo
    # PRODUCER 1 (`import_envelope`, `unit_scan.rb:337`), config stratum.
    #
    # `row(id)` is an implicit-self call, so `envelope_target` answers the
    # unit's own owner class — `Svc::Repo` — and the index is consulted at
    # `Svc::Repo#row`. A `namespace:` entry distributes exactly as a class-level
    # annotation does, i.e. it answers for ANY selector of a matching class, so
    # the `io.db` bound joins THIS method's declared lane. Nothing subsumes it:
    # the proven lane here is empty, and `rendered_declared` only drops what the
    # proven lane already admits (`effect_table.rb:41`).
    #
    # This is the DECLARED-MISMATCH the shipped binary scores: the port reports
    # the method with `declared: []` against the oracle's `["io.db"]`.
    def find(id)
      row(id)
    end

    # PRODUCER 1, the CALLEE side — the control for "a method's own bound never
    # colours its own row". `Svc::Repo#row` is bounded by the same `namespace:`
    # entry, calls nothing bounded, and so must report `declared: []` on BOTH
    # sides. If this row ever grew a lane the fixture would be measuring the
    # wrong mechanism.
    def row(id)
      id.to_s
    end
  end
end
