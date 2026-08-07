# Qualified-name class-narrowing witnessing
# (docs/notes/20260808-qualified-witnessing-mini-spec.md, slices S0-S3).
#
# The class-narrowing witness used to require BOTH `knows_toplevel_class` (which
# refuses every namespaced name) AND `CoreIndex::class_id` (a NINE-name array),
# so a guard on `File::Stat`, `URI::HTTP`, `Bundler::Source::Git` — or even a
# top-level `Time` — witnessed nothing. It now runs one resolution path and
# renders the FULL resolved path, exactly as the reference does.
#
# The tail of the file is the OTHER direction: a shaped carrier under a guard
# the reference collapses to `Bot`. Those lines were a rigor-rs false positive
# and must now be SILENT; the two controls after them must keep firing.
#
# Every line below was measured on the pinned reference (fresh cwd,
# `--no-cache`, plugin path pinned) before being registered. The probe id in
# each comment is the row in 20260808-qualified-witnessing-probes.md.

# --- positives: namespaced core / stdlib / gem guard classes ------------------

# p1a
def stat_typo(v)
  return unless v.is_a?(File::Stat)
  v.frobnicate_zzz
end

# p1c
def uri_typo(v)
  return unless v.is_a?(URI::HTTP)
  v.frobnicate_zzz
end

# r8 — a DEPTH-3 declaration (`module Bundler; module Source; class Git`), the
# row S0's registry fix exists for.
def bundler_git_typo(v)
  return unless v.is_a?(Bundler::Source::Git)
  v.frobnicate_zzz
end

# q5 — a qualified MODULE is a witnessable guard target, not only a class.
def digest_instance_typo(v)
  return unless v.is_a?(Digest::Instance)
  v.frobnicate_zzz
end

# u1/u2 — top-level, but outside the nine-name CORE_CLASSES array.
def time_typo(v)
  return unless v.is_a?(Time)
  v.frobnicate_zzz
end

def range_typo(v)
  return unless v.is_a?(Range)
  v.frobnicate_zzz
end

# p3c — a leading `::` renders as the bare qualified path.
def absolute_spelling(v)
  return unless v.is_a?(::File::Stat)
  v.frobnicate_zzz
end

# p1e — a CHAIN address (stage 3a-3) takes the same routing.
def chain_typo(h)
  return unless h.last.is_a?(File::Stat)
  h.last.frobnicate_zzz
end

# p9a — `instance_of?`.
def instance_of_typo(v)
  return unless v.instance_of?(File::Stat)
  v.frobnicate_zzz
end

# --- negative controls: the method is PRESENT ---------------------------------

# p7a — the class's own method.
def stat_own(v)
  return unless v.is_a?(File::Stat)
  v.directory?
end

# p7b — inherited over the AS-WRITTEN chain, through two ambiguous leaves
# (`Digest::Base` / `Digest::Class` / `include ::Digest::Instance`).
def digest_inherited(v)
  return unless v.is_a?(Digest::SHA256)
  v.hexdigest
end

# v1 — `host` is an `attr_reader` on `URI::Generic`, inherited by `URI::HTTP`.
def uri_attr(v)
  return unless v.is_a?(URI::HTTP)
  v.host
end

# q4b — an own method on the declaring class (and NO leaf fallback: the
# top-level `::Class#superclass` is not reachable from `Digest::Class`).
def digest_class_own(v)
  return unless v.is_a?(Digest::Class)
  v.digest
end

# --- negative controls: the guard class is not witnessable --------------------

# p2 — a class that exists nowhere.
def unresolvable(v)
  return unless v.is_a?(Foo::Bar::Baz)
  v.frobnicate_zzz
end

# p2b — a nonexistent top-level name.
def unresolvable_toplevel(v)
  return unless v.is_a?(Zorkmid)
  v.frobnicate_zzz
end

# p5 — an IN-SOURCE-only namespaced project class (ADR-0033 provenance
# leniency applies unchanged at the namespaced level).
module Proj
  class Thing
    def real_one
      1
    end
  end
end

def in_source_only(v)
  return unless v.is_a?(Proj::Thing)
  v.frobnicate_zzz
end

# --- the shaped-carrier family: silence is the fix (S3) -----------------------

# r1 — a resolvable-disjoint guard over an Array-literal carrier.
def shape_disjoint
  v = [1, 2]
  return unless v.is_a?(File::Stat)
  v.frobnicate_zzz
end

# r1b — the Hash-shaped carrier.
def shape_disjoint_hash
  v = { a: 1 }
  return unless v.is_a?(URI::HTTP)
  v.frobnicate_zzz
end

# r1f — the `if` form.
def shape_disjoint_if
  v = [1, 2]
  if v.is_a?(File::Stat)
    v.frobnicate_zzz
  end
end

# r1d — an UNRESOLVABLE top-level guard collapses the shape too: the
# reference's `subclass_of?` is false for `:unknown`, so resolution is not a
# precondition of the collapse.
def shape_unresolvable_toplevel
  v = [1, 2]
  return unless v.is_a?(Zorkmid)
  v.frobnicate_zzz
end

# r1e — likewise for an unresolvable QUALIFIED guard.
def shape_unresolvable_qualified
  v = [1, 2]
  return unless v.is_a?(Foo::Bar::Baz)
  v.frobnicate_zzz
end

# r1g — likewise for an in-source project class, which the guard mint declines
# to NARROW to but must still SEE.
def shape_in_source
  v = [1, 2]
  return unless v.is_a?(Proj::Thing)
  v.frobnicate_zzz
end

# --- S3 anti-over-suppression: these must keep firing -------------------------

# The shape SURVIVES a supertype guard (`subclass_of?(Array, Enumerable)` holds)
# and the call is still witnessed.
def shape_supertype
  v = [1, 2]
  return unless v.is_a?(Enumerable)
  v.frobnicate_zzz
end

# s4 — no guard at all.
def shape_no_guard
  v = [1, 2]
  v.frobnicate_zzz
end
