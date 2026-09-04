#!/usr/bin/env ruby
# frozen_string_literal: true

# A deliberately small, credential-free witness for Herdr's newline-delimited
# Unix socket protocol. It emits only fixed status words; all JSON and any
# private paths returned by the local runtime remain in the caller's private
# capture files.

require "json"
require "socket"

MAX_LINE_BYTES = 1_048_576
HERDR_VERSION = "0.8.2"
HERDR_PROTOCOL = 20
WORKSPACE_SUBSCRIPTION_TYPE = "workspace.created"
WORKSPACE_EVENT_NAME = "workspace_created"

def fail_contract(message)
  warn message
  exit 1
end

def read_json_line(socket)
  line = socket.gets(MAX_LINE_BYTES + 1)
  fail_contract("Herdr socket closed before a response") if line.nil?
  fail_contract("Herdr socket response exceeded the bound") if line.bytesize > MAX_LINE_BYTES || !line.end_with?("\n")

  JSON.parse(line)
rescue JSON::ParserError
  fail_contract("Herdr socket returned malformed JSON")
end

def send_request(socket, request_id, method, params)
  payload = JSON.generate("id" => request_id, "method" => method, "params" => params)
  fail_contract("Herdr socket request exceeded the bound") if payload.bytesize > MAX_LINE_BYTES

  socket.write(payload)
  socket.write("\n")
end

def assert_response_id(response, request_id)
  fail_contract("Herdr socket response correlation failed") unless response.is_a?(Hash) && response["id"] == request_id
end

def assert_snapshot(snapshot, expected_label, expected_count)
  required = %w[version protocol workspaces tabs panes layouts agents]
  fail_contract("Herdr snapshot shape changed") unless snapshot.is_a?(Hash)
  fail_contract("Herdr snapshot shape changed") unless required.all? { |key| snapshot.key?(key) }
  fail_contract("Herdr snapshot version changed") unless snapshot["version"] == HERDR_VERSION
  fail_contract("Herdr snapshot protocol changed") unless snapshot["protocol"] == HERDR_PROTOCOL

  collections = %w[workspaces tabs panes layouts agents]
  fail_contract("Herdr snapshot collection shape changed") unless collections.all? do |key|
    snapshot[key].is_a?(Array)
  end
  fail_contract("Herdr snapshot workspace count changed") unless snapshot["workspaces"].length == expected_count

  if expected_count.positive?
    workspace = snapshot["workspaces"].find { |entry| entry["label"] == expected_label }
    fail_contract("Herdr snapshot lost the synthetic workspace") unless workspace
    workspace_id = workspace["workspace_id"]
    fail_contract("Herdr workspace id shape changed") unless workspace_id&.match?(/\Aw[0-9]+\z/)
    fail_contract("Herdr snapshot omitted the workspace tab") unless snapshot["tabs"].any? do |tab|
      tab["workspace_id"] == workspace_id
    end
    fail_contract("Herdr snapshot omitted the workspace pane") unless snapshot["panes"].any? do |pane|
      pane["workspace_id"] == workspace_id
    end
  else
    fail_contract("Herdr snapshot retained closed resources") unless collections.all? do |key|
      snapshot[key].empty?
    end
  end
end

def assert_subscription_event(event, expected_label)
  fail_contract("Herdr event shape changed") unless event.is_a?(Hash)
  fail_contract("Herdr event unexpectedly carried a response id") if event.key?("id")
  fail_contract("Herdr event kind changed") unless event["event"] == WORKSPACE_EVENT_NAME
  data = event["data"]
  fail_contract("Herdr event data shape changed") unless data.is_a?(Hash)
  fail_contract("Herdr event data kind changed") unless data["type"] == "workspace_created"
  fail_contract("Herdr event lost the synthetic workspace") unless data.dig("workspace", "label") == expected_label
end

mode = ARGV.fetch(0) { fail_contract("Herdr socket witness requires a mode") }
socket_path = ARGV.fetch(1) { fail_contract("Herdr socket witness requires a socket") }
socket = UNIXSocket.new(socket_path)

case mode
when "snapshot"
  expected_label = ARGV.fetch(2) { fail_contract("snapshot witness requires a label") }
  expected_count = Integer(ARGV.fetch(3) { fail_contract("snapshot witness requires a count") }, 10)
  fail_contract("snapshot witness count is out of bounds") unless (0..1).cover?(expected_count)
  send_request(socket, "synthetic-snapshot", "session.snapshot", {})
  response = read_json_line(socket)
  assert_response_id(response, "synthetic-snapshot")
  assert_snapshot(response.dig("result", "snapshot"), expected_label, expected_count)
  puts "snapshot_ok"
when "subscribe"
  expected_event = ARGV.fetch(2) { fail_contract("subscription witness requires an event") }
  expected_label = ARGV.fetch(3) { fail_contract("subscription witness requires a label") }
  fail_contract("unsupported Herdr event witness") unless expected_event == "workspace_created"
  send_request(
    socket,
    "synthetic-subscription",
    "events.subscribe",
    { "subscriptions" => [{ "type" => WORKSPACE_SUBSCRIPTION_TYPE }] },
  )
  acknowledgement = read_json_line(socket)
  assert_response_id(acknowledgement, "synthetic-subscription")
  fail_contract("Herdr subscription acknowledgement changed") unless acknowledgement.dig("result", "type") == "subscription_started"
  puts "subscription_started"
  $stdout.flush
  assert_subscription_event(read_json_line(socket), expected_label)
  puts "subscription_ok"
  $stdout.flush
else
  fail_contract("Herdr socket witness received an unknown mode")
end
