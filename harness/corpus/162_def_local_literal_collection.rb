# frozen_string_literal: true

# rigor-rs#472 item 3: a literal Array / Hash local inside a `def` names its
# literal shape in `call.undefined-method`, as at the top level.

def array_seed
  list = [1, :a, "s", nil]
  list.frobnicate_zzz
end

def hash_seed
  table = { a: 1, "b" => [2, 3] }
  table.frobnicate_zzz
end

def empty_seeds
  list = []
  table = {}
  list.frobnicate_zzz
  table.frobnicate_zzz
end

def read_only_calls
  list = [1, 2]
  list.each { |x| x }
  list.frobnicate_zzz
end

# Must still fire, as the widened class: a mutator widens the seed.
def mutated_seed
  list = [1]
  list << 2
  list.frobnicate_zzz
end

def mutated_hash
  table = { a: 1 }
  table[:b] = 2
  table.frobnicate_zzz
end

class Holder
  def nested
    rows = [[1, 2], { k: [3] }]
    rows.frobnicate_zzz
  end
end

top = [1]
top.frobnicate_zzz
