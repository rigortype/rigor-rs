# Upstream v0.4.7 (#2521, #2570 review): a call no def answers reaches a
# project `method_missing`, whose parameters sit behind the method name,
# so the local it passes reads floored after the call.
#
# Must-still-fire control: a local no call passes.

class Sub151
  def method_missing(name, options) = (options[:format] = "json")
  def respond_to_missing?(*) = true
end

def run151
  options = { format: "text" }
  Sub151.new.helper(options)
  puts 1 if options[:format] == "json"
  untouched = { format: "text" }
  puts 2 if untouched[:format] == "json"
end
