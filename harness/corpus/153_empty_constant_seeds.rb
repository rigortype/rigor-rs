# Upstream v0.4.7 (#2500, `ScopeIndexer::EmptySeed`): a constant seeded
# with an empty literal (`""`, `[]`, `{}`) is an accumulator filled later,
# so it no longer folds its emptiness or its elements.
#
# Must-still-fire controls: a non-empty seed (`NAME = "x"`,
# `ITEMS = [1]`) and a frozen empty literal (`EMPTY = [].freeze`).

class Cfg153
  PREFIX = ""
  LIST = []
  MAP = {}
  NAME = "x"
  ITEMS = [1]
  def go
    puts 1 if PREFIX.length > 0
    puts 2 if PREFIX == ""
    puts 3 if LIST.size == 0
    puts 4 if LIST.length.zero?
    puts 5 if MAP.size == 0
    puts 6 if MAP[:a] == 1
    LIST.first.zzz
    MAP[:k].zzz
    puts 7 if NAME == "x"
    puts 8 if ITEMS.size == 1
    ITEMS[0].zzz
  end
end
puts 9 if Cfg153::PREFIX == ""
Cfg153::LIST.last.zzz
SEP153 = ""
puts 10 if SEP153 == ""
EMPTY153 = [].freeze
puts 11 if EMPTY153.size == 0
