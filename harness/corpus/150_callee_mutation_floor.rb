# Upstream v0.4.7 callee mutation floor (ADR-127, #2515 and follow-ups).
#
# A mutable local a call hands to a project body that may write it reads
# floored after the call: a keyword parameter, a hand-off to another def,
# an inherited def, an opaque sink (`send` with a computed name, `pr.call`,
# `instance_exec`), a splat array that holds the local, an alias / `tap` /
# a return inside the callee, a literal `define_method`, a `yield` of the
# parameter, `*rest` / `**kw` parameters, and a `blk.call`. The port floors
# every local such a call passes; the top-level receiver typing reads the
# same floor (`add150(h)` then `h[:a].upcase`).
#
# Must-still-fire controls: an ivar hand-off (`W150.new(o).go`), `yield o`,
# `super(o)`, a literal `public_send` of an undefined name, `JSON.generate`,
# an undefined receiverless name, an Integer local (no mutable carrier),
# and a top-level local no call passes.

module Opts150
  def self.add_kw(options:) = (options[:format] = "x")
  def self.add_pos(parser, options) = (options[:format] = "x")
end

class Base150
  def self.add_ro(options) = puts(options.to_s)
  def go150(h) = (h[:a] = 2)
end
class Sub150 < Base150; end

def leaf150(h) = (h[:format] = "json")
def mid150(h) = leaf150(h)
def poke150(h)
  x = h
  x[:format] = "json"
end
def poke_tap150(h) = h.tap { |t| t[:format] = "json" }
def ident150(h) = h
def each_h150(h) = yield(h)
def add_rest150(*rest) = (rest.first[:k] = 1)
def kwr150(**kw) = (kw[:h][:k] = 1)

class Dm150
  define_method(:helper150) { |h| h[:format] = "json" }
end

class C150 < Base150
  def kwarg
    options = { format: "text" }
    Opts150.add_kw(options: options)
    puts 1 if options[:format] == "json"
  end

  def handoff
    options = { format: "text" }
    mid150(options)
    puts 2 if options[:format] == "json"
  end

  def inherited
    options = { format: "text" }
    Sub150.add_ro(options)
    puts 3 if options[:format] == "json"
  end

  def sinks(name, pr)
    a = { format: "text" }
    send(name, a)
    puts 4 if a[:format] == "json"
    b = { format: "text" }
    pr.call(b)
    puts 5 if b[:format] == "json"
    c = { format: "text" }
    Object.new.instance_exec(c) { |x| x[:format] = "json" }
    puts 6 if c[:format] == "json"
  end

  def splat(p)
    options = { format: "text" }
    args = [p, options]
    Opts150.add_pos(*args)
    puts 7 if options[:format] == "json"
    o2 = { format: "text" }
    args2 = [p]
    args2 << o2
    Opts150.add_pos(*args2)
    puts 8 if o2[:format] == "json"
  end

  def aliases
    a = { format: "text" }
    poke150(a)
    puts 9 if a[:format] == "json"
    b = { format: "text" }
    poke_tap150(b)
    puts 10 if b[:format] == "json"
    c = { format: "text" }
    r = ident150(c)
    r[:format] = "json"
    puts 11 if c[:format] == "json"
  end

  def definers
    options = { format: "text" }
    Dm150.new.helper150(options)
    puts 12 if options[:format] == "json"
    o = { format: "text" }
    each_h150(o) { |x| x[:format] = "json" }
    puts 13 if o[:format] == "json"
  end

  def rests
    a = { k: 0 }
    add_rest150(a)
    puts 14 if a[:k] == 0
    b = { k: 0 }
    kwr150(h: b)
    puts 15 if b[:k] == 0
  end

  def blk_call(&blk)
    o = { a: 1 }
    blk.call(o)
    puts 16 if o[:a] == 1
  end

  def embed150(config, ca)
    cluster = config.dig(:clusters, 0, :cluster)
    cluster[:x] = ca
  end

  def kube(ca)
    config = { clusters: [{ cluster: { server: 1 } }] }
    embed150(config, ca) if ca
    config.deep_stringify_keys
  end

  # Controls.
  def ctl_yield
    o = { a: 1 }
    yield o
    puts 21 if o[:a] == 1
  end

  def go150(h)
    o = { a: 1 }
    super(o)
    puts 22 if o[:a] == 1
  end

  def ctl_public_send(m)
    o = { a: 1 }
    m.public_send(:zz150, o)
    puts 23 if o[:a] == 1
  end

  def ctl_int
    n = 1
    go150(n)
    puts 24 if n == 1
  end

  def ctl_json
    o = { a: 1 }
    JSON.generate(o)
    puts 25 if o[:a] == 1
  end

  def ctl_unknown
    o = { a: 1 }
    unknown_thing150(o)
    puts 26 if o[:a] == 1
  end
end

class W150
  def initialize(h) = (@h = h)
  def go = (@h[:format] = "json")
end

def ctl_ivar150
  options = { format: "text" }
  W150.new(options).go
  puts 27 if options[:format] == "json"
end

def add150(h)
  h[:a] = "s"
end
h6 = { a: 1 }
add150(h6)
h6[:a].upcase
opts = { k: 0 }
mid150(opts)
puts(:set) if opts[:k] == 0
h7 = { a: 1 }
h7[:a].upcase
