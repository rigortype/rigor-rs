# frozen_string_literal: true
#
# rigor-rs Ruby sidecar (ADR-0008 / ADR-0036).
#
# A persistent worker the rigor-rs binary spawns to execute the real Ruby calls
# it does not reimplement natively — the long tail of constant folding (and,
# later, plugin target-library invocation). Protocol: newline-delimited JSON over
# stdin/stdout (v1 — the ADR's MessagePack framing arrives with batching). The
# analyzed application's own code is NEVER executed here; only rigor-decided,
# purity-gated calls on values rigor built from literals are.
#
# Ops: a handshake line on startup, then `ping` (liveness), `fold` (execute one
# gated pure call on scalar literals), and `shutdown`. Unknown ops and any
# execution error answer with a decline rather than crashing, so a version skew
# or a surprising value degrades (rigor widens to the nominal type), never hangs.

require "json"

$stdout.sync = true

# A scalar literal crosses the wire tagged: {"t"=>"int","v"=>..} etc. These
# mirror rigor's `Scalar` (Int/Float/Str/Sym/Bool/Nil) exactly.
def decode_scalar(h)
  case h["t"]
  when "int"   then Integer(h["v"])
  when "float" then Float(h["v"])
  when "str"   then h["v"].to_s
  when "sym"   then h["v"].to_s.to_sym
  when "bool"  then h["v"] ? true : false
  when "nil"   then nil
  else raise "unrepresentable scalar tag: #{h["t"].inspect}"
  end
end

# Encode a result back to a tagged scalar, or `nil` when it is not one of rigor's
# scalar carriers (so the caller declines rather than inventing a type). A
# non-finite Float has no JSON form, so it declines too.
def encode_scalar(v)
  case v
  when Integer          then { "t" => "int", "v" => v }
  when Float            then v.finite? ? { "t" => "float", "v" => v } : nil
  when String           then { "t" => "str", "v" => v }
  when Symbol           then { "t" => "sym", "v" => v.to_s }
  when true, false      then { "t" => "bool", "v" => v }
  when NilClass         then { "t" => "nil" }
  end
end

# Execute one call. rigor's Rust side has already decided the (class, method) is
# pure + deterministic + foldable (the `sidecar_foldable` allowlist), so this
# trusts the method name but still runs under rescue: any raise, or a result that
# is not a scalar carrier, is a decline (`{"ok"=>false}`), never a crash.
def fold_call(recv_h, method, arg_hs)
  recv = decode_scalar(recv_h)
  args = Array(arg_hs).map { |h| decode_scalar(h) }
  result = recv.public_send(method.to_s, *args)
  enc = encode_scalar(result)
  enc ? { "ok" => true, "result" => enc } : { "ok" => false }
rescue StandardError
  { "ok" => false }
end

# Handshake: the client reads this FIRST to confirm the sidecar is usable (the
# ADR-0036 availability probe). `rigor_sidecar` is the protocol version.
puts JSON.generate("rigor_sidecar" => 1, "ruby_version" => RUBY_VERSION)

STDIN.each_line do |line|
  line = line.strip
  next if line.empty?

  begin
    req = JSON.parse(line)
  rescue JSON::ParserError
    puts JSON.generate("error" => "bad_json")
    next
  end

  case req["op"]
  when "ping"
    puts JSON.generate("ok" => true)
  when "fold"
    puts JSON.generate(fold_call(req["recv"], req["method"], req["args"]))
  when "shutdown"
    break
  else
    puts JSON.generate("error" => "unknown_op", "op" => req["op"])
  end
end
