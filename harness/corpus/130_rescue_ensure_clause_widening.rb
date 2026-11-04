# Upstream v0.4.3 (#1837, rigor #1231 / #1457): a `rescue` arm runs after the
# `begin` body raised from any point in it, and an `ensure` clause after the
# body, its `else` or a rescue arm raised or finished, so either clause reads
# each local the body writes as untyped. The widening stays inside the clause:
# code after the `begin` keeps its precise types. A write inside a nested
# `def` is another scope's, and a `retry` aimed at the `begin` keeps the arm
# unwidened.
#
# Every row is oracle-measured at the `v0.4.3` pin (v0.4.2 fired each
# retracted row). MUST-STILL-FIRE rows read a local the body does not write,
# or rebind it inside the clause.

def cw_stage_flag(s)
  parsed = false
  begin
    Integer(s)
    parsed = true
    Float(s)
  rescue ArgumentError
    return :late if parsed
  end
  :ok
end

def cw_ensure_flag(s)
  done = false
  begin
    Integer(s)
    done = true
  ensure
    puts "rolled back" unless done
  end
  done
end

def cw_method_ensure(a)
  x = nil
  a.shift
ensure
  puts "x" if x
end

def cw_block_write(xs)
  seen = false
  begin
    xs.each { |x| seen = true; Integer(x) }
  rescue ArgumentError
    puts "s" if seen
  end
end

def cw_ternary(s)
  ok = false
  begin
    ok = true
    Integer(s)
  rescue ArgumentError
    ok ? 1 : 2
  end
end

def cw_else_write(s)
  ok = false
  begin
    Integer(s)
  rescue ArgumentError
    nil
  else
    ok = true
  ensure
    puts 1 if ok
  end
end

cw_top = false
begin
  cw_top = true
  Integer("z")
rescue ArgumentError
  puts 1 if cw_top
end

cw_um = 1
begin
  cw_um = "s"
  Integer("z")
rescue ArgumentError
  cw_um.upcase
end

# MUST STILL FIRE: the body does not write `parsed`.
def cw_ctl_not_written(s)
  parsed = false
  begin
    Integer(s)
  rescue ArgumentError
    return :late if parsed
  end
  :ok
end

# MUST STILL FIRE: the clause rebinds the name before reading it.
def cw_ctl_clause_rebind(s)
  ok = false
  begin
    ok = true
    Integer(s)
  rescue ArgumentError
    ok = nil
    puts 1 if ok
  end
end

# MUST STILL FIRE: a write inside a nested `def` is the method's own local.
class CwNestedDef
  x = nil
  begin
    def helper
      x = 1
    end
    Integer("z")
  ensure
    puts 1 if x
  end
end

# MUST STILL FIRE: neither the rescue arm nor the ensure reads a body write.
def cw_ctl_both(s)
  x = false
  y = nil
  begin
    y = 1
    Integer(s)
  rescue ArgumentError
    puts 1 if x
  ensure
    puts 2 if x
  end
  y
end

# MUST STILL FIRE: top-level arm reading an unwritten local, and the
# `rescue => e` binding over a body write.
cw_top_x = 1
cw_top_y = 2
begin
  cw_top_y = "s"
  Integer("z")
rescue ArgumentError => cw_top_e
  cw_top_x.upcase
  cw_top_e.cw_nope
end
cw_top_y
