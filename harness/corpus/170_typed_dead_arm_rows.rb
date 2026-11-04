# An arm the reference elides because its predicate folds (`branch_certainty`),
# where the port's syntax-level dead set keeps it live (rigor-rs#482). The
# reference never evaluates the arm, so the arm's own writes never reach its
# reads: `z.upcase` and `k == 2` stay silent there. The port declines every row
# inside such an arm.
#
# Must-still-fire controls: the outer `h[:format] == "json"` row, an inner row
# in a live arm (`ENV["X"]`), and an arm the port's own fold proves dead
# (`x == 2`), whose `w.upcase` both engines keep silent.
h = {format: "text"}
if h[:format] == "json"
  y = 1
  puts 2 if y == 2
  z = nil
  z.upcase
end
a = [1]
if a.empty?
  k = 1
  puts 3 if k == 2
end
def m
  opts = {mode: "fast"}
  if opts[:mode] == "slow"
    n = 1
    puts 4 if n == 2
  end
end
if ENV["X"]
  j = 1
  puts 5 if j == 2
end
x = 1
if x == 2
  w = nil
  w.upcase
end
1.zork
