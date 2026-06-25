# Spike — ADR-0003 gate: does Prism expose source ranges, comments/trivia, and
# error recovery? The Rust `ruby-prism` crate mirrors this API.
# Run: `ruby spike/probe_prism.rb`
require "prism"

src = <<~RUBY
  # rigor: a leading pragma comment
  def slug(title) # trailing comment
    title.downcase.gsub(/\\s+/, "-")
  end

  s = slug("Hello World")
  s.lenght
RUBY

res = Prism.parse(src)
puts "prism #{Prism::VERSION}"

puts "\n== comments (text + location) =="
res.comments.each do |c|
  loc = c.location
  puts "  L#{loc.start_line} #{loc.start_offset}..#{loc.end_offset}: #{c.slice.inspect}"
end

puts "\n== node spans: locate the `lenght` call precisely =="
# walk the AST to find the CallNode named :lenght
found = nil
visit = ->(node) do
  return if node.nil?
  if node.is_a?(Prism::CallNode) && node.name == :lenght
    found = node
  end
  node.compact_child_nodes.each { |ch| visit.call(ch) }
end
visit.call(res.value)
if found
  ml = found.message_loc
  puts "  call .#{found.name}  receiver=#{found.receiver&.slice.inspect}"
  puts "  message_loc: L#{ml.start_line}:#{ml.start_column} offset #{ml.start_offset}..#{ml.end_offset}"
  puts "  full call_loc: #{found.location.start_offset}..#{found.location.end_offset}"
end

puts "\n== error recovery: parse broken code, still get a tree + errors =="
broken = "def f(\n  x.\nend\n y = 1\n y.foo"
bres = Prism.parse(broken)
puts "  errors: #{bres.errors.size}; first: #{bres.errors.first&.message.inspect}"
puts "  tree still produced: #{!bres.value.nil?} (#{bres.value.class})"
stmts = bres.value.statements.body.size rescue 0
puts "  top-level statements recovered: #{stmts}"
