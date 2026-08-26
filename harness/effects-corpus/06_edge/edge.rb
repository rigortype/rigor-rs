# The TRANSITIVE-vs-DIRECT trap (ADR-0043 slice 3), isolated.
#
# `exhaustive` in `rigor effects --format=json` is upstream's TRANSITIVE bit:
# `causes.empty?` per unit (`unit_scan.rb:138`), joined across reopenings
# (`summary.rb:89`), then ANDed along every RESOLVED project edge to a fixpoint
# (`propagator.rb:128`). A port that computes the direct bit and prints it there
# over-claims by construction — measured at 10 OVER on the graded corpus and 986
# on mastodon/app (`docs/notes/20260826-effects-s3-probe.md` § 3a).
#
# What makes the trap non-obvious is that a call the catalogue CLAIMS — no taint
# at the site, so the direct bit stays true — can still contribute a project
# edge: `keeps_project_edge?(entry, implicit)` is `entry.posture? || implicit`
# (`unit_scan.rb:409`), and its own comment names the measured Redmine case,
# "`Kernel#format` is a real row and `CustomField#format` is a real method, and
# only the union reads both correctly".
#
# Every method below is DIRECTLY exhaustive on the caller's side and
# TRANSITIVELY tainted, so a port emitting the direct bit reads as OVER here and
# no other corpus project sees it. Nothing in this project has a proven label to
# lose: it grades one lane, the bit.

class Shadow
  # `Kernel#format` is a real catalogue row with `effects: []`, so the call is
  # CLAIMED and contributes no taint of its own — direct bit true. The name is
  # unqualified, so it also keeps a project edge, and `Shadow#format` below is
  # not exhaustive: transitive bit FALSE.
  def calls_shadowed
    format("a")
  end

  # The shadowing definition, and the reason the edge matters. Its receiver is
  # an untyped parameter, so it taints on both sides.
  def format(spec)
    spec.render
  end

  # The same shape through a second Kernel row, so a port cannot pass by
  # special-casing one selector.
  def calls_shadowed_caller
    caller
  end

  def caller
    @backend.frames
  end
end

class Reader
  # The POSTURE half of `keeps_project_edge?`: `slurp` is rowed by no class and
  # is not a universal name, so upstream's `File` class default answers it, and a
  # posture answer keeps its edge too — into the project's own `File.slurp`.
  #
  # rigor-rs reaches the same FALSE by a different road (its posture tier is off
  # entirely, issue #106, so the call is uncatalogued and taints directly). The
  # case is kept because it is upstream's second edge source, and because a
  # future slice that re-introduces the tier must not silently lose the edge.
  def read_it
    File.slurp("x")
  end
end

class File
  def self.slurp(path)
    path.read_all
  end
end
