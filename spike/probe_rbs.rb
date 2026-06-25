# Spike — ADR-0004 gate: does RBS expose typed method definitions
# (return types, parameter types, variance)?
#
# The Rust `ruby-rbs` crate parses the SAME RBS grammar, so if the data model
# carries these types here (via the Ruby `rbs` gem, pinned at 4.0.2 in the
# reference), a Rust RBS AST carries them too — or a thin extraction layer over
# its AST suffices. Run: `ruby spike/probe_rbs.rb`
require "rbs"

puts "rbs #{RBS::VERSION}"

# (a) Parser-level: the syntax AST carries method types, variance, generics, blocks.
sig = <<~RBS
  class Box[out T]
    def initialize: (T value) -> void
    def get: () -> T
    def map: [U] () { (T) -> U } -> Box[U]
    def fetch: (Integer index, ?T default) -> (T | nil)
  end
RBS

result = RBS::Parser.parse_signature(sig)
decls = result.is_a?(Array) ? result.last : result
box = decls.first

puts "\n== parser AST (types are present in the tree) =="
tp = box.type_params.map { |p| "#{p.variance} #{p.name}" }.join(", ")
puts "class #{box.name}[#{tp}]"
box.members.each do |m|
  next unless m.is_a?(RBS::AST::Members::MethodDefinition)
  overloads = m.respond_to?(:overloads) ? m.overloads.map(&:method_type) : m.types
  overloads.each do |mt|
    fn = mt.type
    req = fn.required_positionals.map { |p| p.type.to_s }
    opt = fn.optional_positionals.map { |p| "?#{p.type}" }
    params = (req + opt).join(", ")
    blk = mt.block ? " { #{mt.block.type} }" : ""
    tvars = mt.type_params.any? ? "[#{mt.type_params.map { |p| p.respond_to?(:name) ? p.name : p }.join(', ')}] " : ""
    puts "  #{m.name}: #{tvars}(#{params})#{blk} -> #{fn.return_type}"
  end
end

# (b) Resolved builder: a real stdlib typed method (String#upcase, Integer#+).
loader = RBS::EnvironmentLoader.new
env = RBS::Environment.from_loader(loader).resolve_type_names
builder = RBS::DefinitionBuilder.new(env: env)

{ "::String" => %i[upcase length], "::Integer" => %i[+] }.each do |cls, names|
  defn = builder.build_instance(RBS::TypeName.parse(cls))
  names.each do |name|
    m = defn.methods[name]
    next unless m
    puts "\n== #{cls}##{name} (resolved typed signature) =="
    m.method_types.each { |mt| puts "  #{mt}" }
  end
end
