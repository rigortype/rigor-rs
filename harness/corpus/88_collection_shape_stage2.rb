# Collection-shape chain ROOTS (spec
# docs/notes/20260807-collection-shape-slice-spec.md, STAGE 2). Stage 1 made a
# mutated/bound collection LOCAL survive; the rows left open were the ones whose
# receiver comes from an EXPRESSION whose root rigor-rs could not type at all.
# Three roots are pinned here, each with its decline.
#
# 2a/2c THE MECHANISM: `method_signature`'s all-overloads-agree collapse drops a
# return whenever ANY overload disagrees — including a BLOCK overload whose
# return is unrelated (`Dir.glob { } -> nil`, `String#split { } -> self`). At a
# BLOCK-FREE call site the reference's `OverloadSelector` never considers those,
# so it types `Array[String]` and rigor-rs typed `Dynamic[top]`. A parallel
# block-free-overloads-only return slot fixes it; a block CALL still reads the
# block slot, and divergence AMONG the block-free overloads still declines.
#
# 2b: `ENV` is an RBS OBJECT CONSTANT (`ENV: RBS::Unnamed::ENVClass`), not a
# class — so nothing typed it. Only the CALL's return is typed here; `ENV`
# itself is never minted as a witnessable nominal.
#
# 2e: a fully-qualified `::A::B::C::CONST` read is ONE `ConstantRead` whose name
# is the whole path, so the bare-name C5 map missed it while the lexical
# spelling of the SAME constant folded.
#
# Numbering note: 87 is reserved for an in-flight narrowing branch.

# --- positives ---------------------------------------------------------------

# (1) 2a — `Dir.glob` (block overload returns nil). The chain's block folds and
# `sort` already worked; only the ROOT was untyped. FIRES `for Array`.
def glob_chain
  Dir.glob('[a-z]*.yml')
    .reject { |f| f.empty? }
    .map { |f| f.upcase }
    .sort
    .to_sentence
end

# (2) 2a control — `Dir.[]` has a SINGLE overload, so the flat return slot
# always carried it. FIRES `for Array` (unchanged by this slice; pinned so a
# regression in the shared arm is visible).
def bracket_glob
  files = Dir['*.db']
  files.blank?
end

# (3) 2b — `ENV.keys` types `Array[String]` through the object-constant
# declaration; the rest of the chain (`select {}`, `Array#-`) already worked.
# FIRES `for Array`.
def env_keys_chain
  base_keys = %w[HOST PORT]
  (ENV.keys.select { |x| x.start_with?('REGISTRY_') } - base_keys).present?
end

# (4) 2c — `String#split` (block overload returns self) rooted at a typed
# receiver. FIRES `for Array`. NOTE the root must already be typed: the oracle's
# c08 probe pins that a `Dynamic`-rooted `x.to_s.split(...)` stays SILENT.
def split_second(token)
  Base64.decode64(token || '').split(':', 2).second
end

# (5) 2e — the SAME constant read through a fully-qualified path. FIRES
# `for Array`, exactly like the lexical spelling on the line above it.
module Reports
  module Codequality
    class Summary
      SEVERITY_PRIORITIES = { high: 1, low: 2 }.freeze

      def lexical_summary
        SEVERITY_PRIORITIES.keys.index_with(0)
      end

      def qualified_summary
        ::Reports::Codequality::Summary::SEVERITY_PRIORITIES.keys.index_with(0)
      end
    end
  end
end

# (6) 2b — the mechanism is not ActiveSupport-specific: a genuinely absent
# method on the typed chain FIRES `for Array` too.
def env_keys_typo
  ENV.keys.frobnicate_zzz
end

# --- negative controls -------------------------------------------------------

# (7) 2a/2c decline — the BLOCK form of the same method never reads the
# block-free slot; the reference selects the block overload, whose return is
# `nil`. It therefore fires `for nil` and rigor-rs does not: an accepted
# COVERAGE GAP (nil-receiver typing of a block return is out of this slice),
# never an FP. What it pins here is that we do NOT type this `Array`.
def glob_with_block
  Dir.glob('*.rb') { |f| f }.frobnicate_zzz
end

# (8) 2a decline — overloads that diverge for a reason OTHER than a block
# (`Regexp.last_match`: `MatchData?` vs `String?`) still collapse to no return.
# SILENT on both tools.
def divergent_overload_root
  Regexp.last_match(2).frobnicate_zzz
end

# (9) 2b decline — a NILABLE declared return (`ENVClass#[] : (String) ->
# String?`). The reference carries `String | nil` and declines dispatch on the
# union; typing it as a bare `String` fired where the oracle is silent (13 of
# the 15 sweep FPs on the first cut of this arm). SILENT on both tools.
def env_index_is_nilable
  ENV['COMPRESS_CMD'].present?
end

# (10) 2e decline — a qualified path whose constant is NOT lexically visible
# from the use site. Stage 2e is a SPELLING extension of the C5 lexical gate,
# not a wider resolution reach.
#
# This is an accepted COVERAGE GAP, and the gate behind it was measured, not
# theorised: the reference fires here (it resolves the sibling constant), but
# the SAME shape ACROSS FILES — gitlab's `Gitlab::GitalyClient::DiffBlob::ATTRS`
# read from `…::DiffBlobsStitcher` — is one the reference does NOT resolve, and
# folding it was an oracle FP on the sweep. rigor-rs's constant harvest is
# project-wide and cannot tell the two apart, so it declines both. Losing this
# in-file case is the price of not shipping the cross-file FP.
module Sibling
  module Space
    class Holder
      KEYS = { a: 1 }.freeze
    end

    class Reader
      def read
        ::Sibling::Space::Holder::KEYS.keys.index_with(0)
      end
    end
  end
end

# (11) 2e decline — a qualified path naming nothing the project harvested
# resolves to nothing. SILENT on both tools.
def unknown_qualified_path
  ::No::Such::Namespace::MISSING_ZZZ.keys.index_with(0)
end
