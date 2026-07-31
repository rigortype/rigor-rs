#!/usr/bin/env ruby
# frozen_string_literal: true

# Regenerate / verify `UNBUILDABLE_DEFINITIONS` in `crates/rigor-index/src/rbs.rs`.
#
# The reference loads a DECLARATION for every class in its RBS universe, but for
# a handful of them `RBS::DefinitionBuilder` raises when asked to build the
# definition — a duplicated method definition across two colliding signature
# sources, a dangling type reference, a missing superclass. `RbsLoader`
# fail-softs those to `nil`, so the dispatcher degrades EVERY call on such a
# class to `Dynamic[Top]` and the oracle is silent about it — including on a
# misspelled method. rigor-rs vendors a NON-colliding subset of the same
# signatures, so its index builds cleanly and would witness the typo: a false
# positive by ADR-0002. `UNBUILDABLE_DEFINITIONS` is how rigor-rs carries that
# set; this script is its executable provenance.
#
# **The set is a property of (reference pin × rbs version × THE HOST'S INSTALLED
# GEMS), not of the pin alone.** `RBS::EnvironmentLoader#add(library:)` prefers
# an installed gem's own `sig/` over `rbs`'s `stdlib/<lib>/` copy, so which
# signature files collide depends on what is installed. Measured: uninstall the
# `bigdecimal` gem and `BigMath` leaves the set — the same pinned reference then
# FIRES on `BigMath.frobnicate(1)`. Consequences:
#
#   * Run this in THE SAME ENVIRONMENT the gates run in. Run elsewhere it will
#     legitimately disagree, and the disagreement is not a bug in either place.
#   * A diff can mean A GEM CHANGED rather than upstream changing. The per-entry
#     `sources:` lines below say which files supplied each colliding declaration
#     and tag each `[env]` (host-installed gem — moves when that gem moves) or
#     `[pin]` (the reference's own `data/` tree, or the rbs gem whose version is
#     locked to the pin by the reference's Gemfile.lock).
#
#     ruby harness/unbuildable_classes.rb            # print the derived set
#     ruby harness/unbuildable_classes.rb --check    # diff it against rbs.rs
#
# Env: REFERENCE_RIGOR_DIR (default `reference/rigor`, the PINNED submodule).
# Never point it at a working checkout — `UPSTREAM.md` hazard 3.

REPO = File.expand_path("..", __dir__)
REFERENCE_RIGOR_DIR = File.expand_path(ENV.fetch("REFERENCE_RIGOR_DIR", "reference/rigor"), REPO)
RBS_RS = File.join(REPO, "crates/rigor-index/src/rbs.rs")

$LOAD_PATH.unshift(File.join(REFERENCE_RIGOR_DIR, "lib"))
require "rigor"
require "rigor/environment/default_libraries"

# The rbs gem's own root, so its `core/` + `stdlib/` + `sig/` can be told apart
# from an arbitrary host gem. The reference's Gemfile.lock locks this version to
# the pin, so anything under it counts as pin-determined, not environment-supplied.
# (`DEFAULT_CORE_ROOT` is `<gem root>/core`, so exactly ONE level up — `../..`
# would be the whole `gems/` directory and would tag every host gem `[pin]`.)
RBS_GEM_ROOT = File.expand_path("..", RBS::EnvironmentLoader::DEFAULT_CORE_ROOT.to_s)

# Classify one declaration's source file: `[pin]` when it comes from the pinned
# reference's own `data/` tree or from the version-locked rbs gem, `[env]` when it
# comes from any other installed gem — the part of the answer that moves when the
# host's gem set moves rather than when the pin does.
def classify(path)
  return "pin" if path.start_with?(REFERENCE_RIGOR_DIR)
  return "pin" if path.start_with?(RBS_GEM_ROOT)

  "env"
end

# Shorten a source path to the part that identifies it (`bigdecimal-4.1.2/sig/…`,
# `rbs-4.1.0/stdlib/…`, `data/vendored_gem_sigs/…`) — the full absolute paths are
# machine-specific noise in a diff.
def short(path)
  return "reference/#{path.delete_prefix(REFERENCE_RIGOR_DIR).delete_prefix('/')}" if path.start_with?(REFERENCE_RIGOR_DIR)

  path.sub(%r{\A.*/gems/}, "")
end

# Derive the set exactly as a configless `rigor check` run would see it: the
# reference's own `DEFAULT_LIBRARIES` with no project `signature_paths:`. The two
# sides are probed SEPARATELY because the reference builds them independently and
# they fail independently (`Bundler`'s instance definition builds; only its
# singleton raises), and rigor-rs mirrors that split.
#
# Returns `[name, instance_fails, singleton_fails, reason, sources]`, sorted;
# `sources` is the `[kind, short_path]` list of the declarations that were merged
# into that class — the collision's ingredients.
def derive
  env = Rigor::Environment::RbsLoader.build_env_for(
    libraries: Rigor::Environment::DEFAULT_LIBRARIES, signature_paths: []
  )
  builder = RBS::DefinitionBuilder.new(env: env)
  env.class_decls.filter_map do |type_name, entry|
    failures = {}
    %i[build_instance build_singleton].each do |kind|
      builder.public_send(kind, type_name)
    rescue StandardError => e
      failures[kind] = e.class.name.split("::").last
    end
    next if failures.empty?

    sources = []
    entry.each_decl do |decl|
      path = decl.location&.buffer&.name.to_s
      next if path.empty?

      sources << [classify(path), short(path)]
    end
    [
      type_name.to_s.sub(/\A::/, ""),
      failures.key?(:build_instance),
      failures.key?(:build_singleton),
      failures.map { |k, v| "#{k}=#{v}" }.join(", "),
      sources.uniq
    ]
  end.sort_by(&:first)
end

# The committed table, as `[name, instance, singleton]` triples.
def committed
  # `rbs.rs` is UTF-8 (the doc comments carry `⇒`, `…`); read it as such rather
  # than inheriting the process's default external encoding.
  src = File.read(RBS_RS, encoding: "UTF-8")
  body = src[/const UNBUILDABLE_DEFINITIONS: &\[\(&str, bool, bool\)\] = &\[(.*?)\];/m, 1]
  abort "unbuildable_classes: UNBUILDABLE_DEFINITIONS not found in rbs.rs" unless body

  body.scan(/\("([^"]+)",\s*(true|false),\s*(true|false)\)/)
      .map { |name, inst, sing| [name, inst == "true", sing == "true"] }
      .sort_by(&:first)
end

def sources_for(derived, name)
  row = derived.find { |n, _, _, _, _| n == name }
  row ? row[4] : []
end

# A mismatched entry is reported WITH its sources, so "the set changed" reads as
# "your bigdecimal gem changed" instead of sending the reader to bisect the pin.
def explain(derived, triple, headline)
  name, inst, sing = triple
  puts "#{headline}: #{name} (instance_fails=#{inst}, singleton_fails=#{sing})"
  srcs = sources_for(derived, name)
  if srcs.empty?
    puts "    this environment's oracle BUILDS it — nothing collides here. Most likely a"
    puts "    signature source present where the table was derived is absent here (or vice"
    puts "    versa); compare the `sources:` lines from a plain run on both machines."
  else
    srcs.each { |kind, path| puts "    sources: [#{kind}] #{path}" }
    puts "    ^ an [env] source means a HOST GEM supplied it: that gem moving explains this diff" if srcs.any? { |k, _| k == "env" }
  end
end

derived = derive
if ARGV.include?("--check")
  want = derived.map { |name, inst, sing, _, _| [name, inst, sing] }
  have = committed
  if want == have
    envd = derived.count { |_, _, _, _, s| s.any? { |k, _| k == "env" } }
    puts "OK: #{have.size} classes, rbs.rs matches this environment's oracle " \
         "(#{envd} of them environment-supplied — see the header)"
    exit 0
  end
  (want - have).each { |t| explain(derived, t, "MISSING from / WRONG in rbs.rs") }
  (have - want).each { |t| explain(derived, t, "STALE in rbs.rs (does not fail here)") }
  warn "\nA diff is not automatically an upstream change: this set depends on the " \
       "host's installed gems as well as the pin. Check the [env] sources above, and " \
       "confirm you are running in the same environment as the gates."
  exit 1
end

puts "// derived from #{REFERENCE_RIGOR_DIR} (#{derived.size} classes)"
derived.each do |name, inst, sing, reason, sources|
  puts format("    (%-28s %-6s %-6s // %s", "\"#{name}\",", "#{inst},", "#{sing}),", reason)
  sources.each { |kind, path| puts "    //     [#{kind}] #{path}" }
end
