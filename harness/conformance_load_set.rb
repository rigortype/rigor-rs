#!/usr/bin/env ruby
# frozen_string_literal: true

# Regenerate / verify `LOAD_SET_DIVERGENT` in
# `crates/rigor-index/src/rbs/conformance/load_set.rs` (issue #129, ADR-0044).
#
# The `conforms-to` scan answers "does the reference's RBS environment hold
# this interface / give this class this member". The port's own index is NOT
# that environment: the reference's default libraries also load `prism`,
# `rbs` (and `rdoc` through it), and on this host it reads the installed
# `bigdecimal` / `base64` / `mutex_m` gems' own `sig/` where the port vendors
# the rbs stdlib copies. A name the reference holds and the port does not
# turns "not loaded" into a false positive (`Prism::_Visitor`); a class the
# reference gives more members (`RDoc::Constant#value`, reopened from a
# project file) turns a missing member into one. This script lists every
# name whose view differs, so the scan never resolves through it, trusts its
# surface, or places its declaration order:
#
#   * a class/module, interface, type alias, class alias, constant or global
#     only one side has (a project file declaring one may be quarantined
#     upstream for a duplicate declaration the port cannot see);
#   * a class/module whose kind or type-parameter arity differs, whose
#     instance definition the reference cannot build while the port computes
#     one, or whose instance-method NAME SET differs;
#   * an interface whose `build_interface` member LIST (order included)
#     differs, or which the reference cannot build while the port can.
#
# The reference side is `RbsLoader.build_env_for(DEFAULT_LIBRARIES, [])` —
# exactly what a configless run loads, capability roles included — built with
# `RBS::DefinitionBuilder`. The port side is `CoreData::load()` read through
# the SAME walk the scan uses (`conformance_surface_dump`, via the ignored
# test `dump_conformance_surface`), with the list itself not applied.
#
# **Like `harness/unbuildable_classes.rb`, the list is a property of
# (reference pin × rbs version × THE HOST'S INSTALLED GEMS).**
# `RBS::EnvironmentLoader#add(library:)` prefers an installed gem's `sig/`
# over rbs's stdlib copy, so run this where the gates run; a diff can mean a
# host gem moved rather than the pin.
#
#     ruby harness/conformance_load_set.rb            # print the derived list
#     ruby harness/conformance_load_set.rb --check    # diff it against load_set.rs
#     ruby harness/conformance_load_set.rb --write    # rewrite load_set.rs
#     ruby harness/conformance_load_set.rb --check --plugin activesupport-core-ext
#
# `--plugin ID` compares the two sides with that bundled plugin's `sig/` loaded
# (the reference DEFERS it after the project's own files; the port ingests its
# vendored copy) and reports the names that diverge ONLY with it. The scan has
# no per-plugin list: a non-empty answer there is a finding (vendor drift, a
# plugin file the reference quarantines) to fix before trusting the plugin.
#
# Env: REFERENCE_RIGOR_DIR (default `reference/rigor`, the PINNED submodule).
# Never point it at a working checkout — `UPSTREAM.md` hazard 3.

require "open3"
require "set"
require "tmpdir"

REPO = File.expand_path("..", __dir__)
REFERENCE_RIGOR_DIR = File.expand_path(ENV.fetch("REFERENCE_RIGOR_DIR", "reference/rigor"), REPO)
LOAD_SET_RS = File.join(REPO, "crates/rigor-index/src/rbs/conformance/load_set.rs")

$LOAD_PATH.unshift(File.join(REFERENCE_RIGOR_DIR, "lib"))
require "rigor"
require "rigor/environment/default_libraries"

def strip(name) = name.to_s.sub(/\A::/, "")

def arity(params)
  [params.count { |p| p.default_type.nil? }, params.size]
end

# `{ classes: {name => [kind, min, max, methods|nil]}, ifaces: {name => [min,
# max, members|nil]}, aliases: Set, class_aliases: Set }`.
PLUGIN = (i = ARGV.index("--plugin")) ? ARGV.fetch(i + 1) : nil

def reference_view
  deferred = PLUGIN ? [File.join(REFERENCE_RIGOR_DIR, "plugins", "rigor-#{PLUGIN}", "sig")] : []
  abort "conformance_load_set: no sig/ for plugin #{PLUGIN}" if PLUGIN && !File.directory?(deferred.first)
  env = Rigor::Environment::RbsLoader.build_env_for(
    libraries: Rigor::Environment::DEFAULT_LIBRARIES, signature_paths: [],
    deferred_signature_paths: deferred
  )
  env = env.first if env.is_a?(Array)
  builder = RBS::DefinitionBuilder.new(env: env)
  classes = {}
  env.class_decls.each do |type_name, entry|
    kind = entry.is_a?(RBS::Environment::ModuleEntry) ? "module" : "class"
    min, max = begin
      arity(entry.type_params)
    rescue StandardError
      [-1, -1]
    end
    methods = begin
      builder.build_instance(type_name).methods.keys.map(&:to_s).sort
    rescue StandardError
      nil
    end
    classes[strip(type_name)] = [kind, min, max, methods]
  end
  ifaces = {}
  env.interface_decls.each do |type_name, entry|
    min, max = arity(entry.decl.type_params)
    members = begin
      builder.build_interface(type_name).methods.keys.map(&:to_s)
    rescue StandardError
      nil
    end
    ifaces[strip(type_name)] = [min, max, members]
  end
  {
    classes: classes,
    ifaces: ifaces,
    aliases: env.type_alias_decls.keys.to_set { |n| strip(n) },
    class_aliases: env.class_alias_decls.keys.to_set { |n| strip(n) },
    constants: env.constant_decls.keys.to_set { |n| strip(n) },
    globals: env.global_decls.keys.to_set(&:to_s)
  }
end

def port_view
  Dir.mktmpdir("conformance-load-set") do |dir|
    path = File.join(dir, "dump.tsv")
    cmd = %w[cargo test --offline -q -p rigor-index --lib --
             --ignored --exact rbs::conformance::tests::dump_conformance_surface]
    env = { "RIGOR_CONFORMANCE_DUMP" => path }
    env["RIGOR_CONFORMANCE_DUMP_PLUGIN"] = PLUGIN if PLUGIN
    out, status = Open3.capture2e(env, *cmd, chdir: REPO)
    abort "conformance_load_set: the port dump failed:\n#{out}" unless status.success? && File.exist?(path)

    view = { classes: {}, ifaces: {}, aliases: Set.new, class_aliases: Set.new, constants: Set.new,
             globals: Set.new }
    File.foreach(path, chomp: true) do |line|
      kind, name, *rest = line.split("\t", -1)
      case kind
      when "class"
        k, min, max, surface = rest
        view[:classes][name] = [k, min.to_i, max.to_i, surface == "?" ? :unknown : surface.split(" ").sort]
      when "iface"
        min, max, members = rest
        view[:ifaces][name] = [min.to_i, max.to_i, members == "?" ? :unknown : members.split(" ")]
      when "alias" then view[:aliases] << name
      when "class_alias" then view[:class_aliases] << name
      when "const" then view[:constants] << name
      when "global" then view[:globals] << name
      end
    end
    view
  end
end

# `{ name => reason }` for every name whose view differs.
def derive(ref, port)
  out = {}
  (ref[:classes].keys | port[:classes].keys).each do |name|
    r = ref[:classes][name]
    p = port[:classes][name]
    reason =
      if r.nil? then "port-only class/module"
      elsif p.nil? then "reference-only #{r[0]}"
      elsif r[0] != p[0] then "kind: reference #{r[0]}, port #{p[0]}"
      elsif r[1..2] != p[1..2] then "type-parameter arity: reference #{r[1..2]}, port #{p[1..2]}"
      elsif p[3] == :unknown then nil # the port cannot build it: silent anyway
      elsif r[3].nil? then "the reference cannot build its instance definition"
      elsif r[3] != p[3]
        "instance surface: reference-only #{(r[3] - p[3]).first(3).inspect}#{(r[3] - p[3]).size > 3 ? '…' : ''}, " \
          "port-only #{(p[3] - r[3]).first(3).inspect}#{(p[3] - r[3]).size > 3 ? '…' : ''}"
      end
    out[name] = reason if reason
  end
  (ref[:ifaces].keys | port[:ifaces].keys).each do |name|
    r = ref[:ifaces][name]
    p = port[:ifaces][name]
    reason =
      if r.nil? then "port-only interface"
      elsif p.nil? then "reference-only interface"
      elsif r[0..1] != p[0..1] then "interface arity: reference #{r[0..1]}, port #{p[0..1]}"
      elsif p[2] == :unknown then nil
      elsif r[2].nil? then "the reference cannot build this interface"
      elsif r[2] != p[2] then "interface members: reference #{r[2].first(4)}…, port #{p[2].first(4)}…"
      end
    out[name] = reason if reason
  end
  (ref[:aliases] ^ port[:aliases]).each do |name|
    out[name] ||= "type alias on #{ref[:aliases].include?(name) ? 'the reference' : 'the port'} only"
  end
  (ref[:class_aliases] ^ port[:class_aliases]).each do |name|
    out[name] ||= "class alias on #{ref[:class_aliases].include?(name) ? 'the reference' : 'the port'} only"
  end
  # A project file declaring one of these collides upstream only
  # (`DuplicatedDeclarationError` quarantines the whole file there).
  (ref[:constants] ^ port[:constants]).each do |name|
    out[name] ||= "constant on #{ref[:constants].include?(name) ? 'the reference' : 'the port'} only"
  end
  (ref[:globals] ^ port[:globals]).each do |name|
    out[name] ||= "global on #{ref[:globals].include?(name) ? 'the reference' : 'the port'} only"
  end
  out.sort.to_h
end

def render(derived)
  lines = []
  lines << "//! GENERATED by `ruby harness/conformance_load_set.rb --write`; verify with"
  lines << "//! `--check` (a pin-bump and host-gem ritual, like `unbuildable_classes.rb`)."
  lines << "//! Do not edit by hand. Derived from the pinned reference's default"
  lines << "//! environment on the gate host: #{derived.size} names."
  lines << ""
  lines << "/// Names whose view differs between the port's recorded RBS model and the"
  lines << "/// reference's default environment (see the generator's header). The"
  lines << "/// `conforms-to` scan never resolves through, trusts, or orders by one."
  lines << "pub(super) const LOAD_SET_DIVERGENT: &[&str] = &["
  derived.each do |name, reason|
    lines << "    #{name.inspect}, // #{reason.gsub('*/', '* /')}"
  end
  lines << "];"
  "#{lines.join("\n")}\n"
end

def committed
  src = File.read(LOAD_SET_RS, encoding: "UTF-8")
  body = src[/LOAD_SET_DIVERGENT: &\[&str\] = &\[(.*?)\n\];/m, 1] or abort "conformance_load_set: list not found"
  body.scan(/^\s*"((?:[^"\\]|\\.)*)",/).flatten.to_set
end

derived = derive(reference_view, port_view)
if PLUGIN
  extra = derived.reject { |name, _| committed.include?(name) }
  if extra.empty?
    puts "OK: plugin #{PLUGIN} adds no divergence beyond load_set.rs"
    exit 0
  end
  extra.each { |name, reason| puts "DIVERGES WITH #{PLUGIN}: #{name} (#{reason})" }
  exit(ARGV.include?("--check") ? 1 : 0)
end
if ARGV.include?("--check")
  have = committed
  want = derived.keys.to_set
  if have == want
    puts "OK: #{want.size} names, load_set.rs matches this environment's oracle"
    exit 0
  end
  (want - have).sort.each { |n| puts "MISSING from load_set.rs: #{n} (#{derived[n]})" }
  (have - want).sort.each { |n| puts "STALE in load_set.rs: #{n}" }
  warn "\nA diff is not automatically an upstream change: the list depends on the host's " \
       "installed gems as well as the pin. Run where the gates run."
  exit 1
elsif ARGV.include?("--write")
  File.write(LOAD_SET_RS, render(derived))
  puts "wrote #{derived.size} names to #{LOAD_SET_RS.delete_prefix("#{REPO}/")}"
else
  derived.each { |name, reason| puts "#{name}\t#{reason}" }
  puts "(#{derived.size} names)"
end
