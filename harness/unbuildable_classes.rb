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
# The set is a property of the (reference pin × rbs version × host gem set)
# triple, so it must be re-derived on any pin or rbs bump — `--check` in the
# bump ritual, plain mode to print a fresh list to paste.
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

# Derive the set exactly as a configless `rigor check` run would see it: the
# reference's own `DEFAULT_LIBRARIES` with no project `signature_paths:`. The two
# sides are probed SEPARATELY because the reference builds them independently and
# they fail independently (`Bundler`'s instance definition builds; only its
# singleton raises), and rigor-rs mirrors that split.
#
# Returns `[name, instance_fails, singleton_fails, reason]` triples, sorted.
def derive
  env = Rigor::Environment::RbsLoader.build_env_for(
    libraries: Rigor::Environment::DEFAULT_LIBRARIES, signature_paths: []
  )
  builder = RBS::DefinitionBuilder.new(env: env)
  env.class_decls.each_key.filter_map do |type_name|
    failures = {}
    %i[build_instance build_singleton].each do |kind|
      builder.public_send(kind, type_name)
    rescue StandardError => e
      failures[kind] = e.class.name.split("::").last
    end
    next if failures.empty?

    [
      type_name.to_s.sub(/\A::/, ""),
      failures.key?(:build_instance),
      failures.key?(:build_singleton),
      failures.map { |k, v| "#{k}=#{v}" }.join(", ")
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

def render(triple)
  name, inst, sing = triple
  "#{name} (instance_fails=#{inst}, singleton_fails=#{sing})"
end

derived = derive
if ARGV.include?("--check")
  want = derived.map { |name, inst, sing, _| [name, inst, sing] }
  have = committed
  if want == have
    puts "OK: #{have.size} classes, rbs.rs matches the pinned reference"
    exit 0
  end
  (want - have).each { |t| puts "MISSING from / WRONG in rbs.rs: #{render(t)}" }
  (have - want).each { |t| puts "STALE in rbs.rs (the reference no longer fails this way): #{render(t)}" }
  exit 1
end

puts "// derived from #{REFERENCE_RIGOR_DIR} (#{derived.size} classes)"
derived.each do |name, inst, sing, reason|
  puts format("    (%-28s %-6s %-6s // %s", "\"#{name}\",", "#{inst},", "#{sing}),", reason)
end
