# frozen_string_literal: true
#
# rigor-rs Ruby sidecar (ADR-0008 / ADR-0036).
#
# A persistent worker the rigor-rs binary spawns to execute the real Ruby calls
# it does not reimplement natively (the long tail of constant folding and, later,
# plugin target-library invocation). Protocol: newline-delimited JSON over
# stdin/stdout (v1 — the ADR's MessagePack framing arrives with batching). The
# analyzed application's own code is NEVER executed here; only rigor-decided,
# purity-gated calls will be (folding lands in a later slice).
#
# Slice 1 surface: a handshake line on startup, then a request loop supporting
# `ping` (round-trip liveness) and `shutdown`. Unknown ops answer with an error
# rather than crashing, so a client/sidecar version skew degrades, never hangs.

require "json"

$stdout.sync = true

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
  when "shutdown"
    break
  else
    puts JSON.generate("error" => "unknown_op", "op" => req["op"])
  end
end
