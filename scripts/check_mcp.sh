#!/bin/bash
# Integration checks against the running daemon: both MCP protocol eras, the
# required `cli` argument, and the four Host/Origin cases. Exits non-zero if any
# check fails.
#
# Era A: 2026-07-28, stateless — one POST per request, no session, with the
#        protocol metadata in `_meta` and the request method and tool name in
#        the `Mcp-Method` and `Mcp-Name` headers.
# Era B: 2025-06-18, sessions — `initialize` returns an Mcp-Session-Id that
#        every later request repeats.
URL=http://127.0.0.1:7317/mcp
ACCEPT='application/json, text/event-stream'
NEW='2026-07-28'
OLD='2025-06-18'
META="\"_meta\":{\"io.modelcontextprotocol/protocolVersion\":\"$NEW\",\"io.modelcontextprotocol/clientCapabilities\":{}}"

failures=0
check() { # check <description> <detail> <0 for pass>
  if [ "$3" = 0 ]; then
    printf '  ok    %s\n' "$1"
  else
    printf '  FAIL  %s (%s)\n' "$1" "$2"
    failures=$((failures + 1))
  fi
}
body() { grep '^data: ' /tmp/cs-mcp-body | sed 's/^data: //' | grep '"jsonrpc"' | tail -1; }

# post_new <mcp-method> <mcp-name-or-dash> <json-body> [extra curl args...]
post_new() {
  local method="$1"; local name="$2"; local payload="$3"; shift 3
  printf '%s' "$payload" > /tmp/cs-mcp-req
  local name_header=()
  [ "$name" != "-" ] && name_header=(-H "Mcp-Name: $name")
  curl -s -o /tmp/cs-mcp-body -w '%{http_code}' -X POST "$URL" \
    -H 'Content-Type: application/json' -H "Accept: $ACCEPT" \
    -H "MCP-Protocol-Version: $NEW" -H "Mcp-Method: $method" "${name_header[@]}" \
    "$@" --data-binary @/tmp/cs-mcp-req
}

echo "Protocol era $NEW (stateless)"

code=$(post_new initialize - "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"$NEW\",\"capabilities\":{},\"clientInfo\":{\"name\":\"check\",\"version\":\"1\"}}}")
check "initialize with no session and no Origin" "HTTP $code" "$([ "$code" = 200 ] && echo 0 || echo 1)"

code=$(post_new tools/list - "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{$META}}")
names=$(body | grep -o '"name":"[a-z_]*"' | sort -u | wc -l | tr -d ' ')
check "tools/list returns all five tools" "HTTP $code, $names names" \
  "$([ "$code" = 200 ] && [ "$names" = 5 ] && echo 0 || echo 1)"

code=$(post_new tools/call search_conversations "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"search_conversations\",\"arguments\":{\"query\":\"the\",\"cli\":\"codex\",\"top_k\":5},$META}}")
foreign=$(body | grep -o 'cli.....claude_code' | wc -l | tr -d ' ')
check "cli=codex returns only Codex turns" "HTTP $code, $foreign claude_code results" \
  "$([ "$code" = 200 ] && [ "$foreign" = 0 ] && echo 0 || echo 1)"

code=$(post_new tools/call search_conversations "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"search_conversations\",\"arguments\":{\"query\":\"the\",\"cli\":\"claude_code\",\"top_k\":5},$META}}")
foreign=$(body | grep -o 'cli.....codex' | wc -l | tr -d ' ')
check "cli=claude_code returns only Claude Code turns" "HTTP $code, $foreign codex results" \
  "$([ "$code" = 200 ] && [ "$foreign" = 0 ] && echo 0 || echo 1)"

code=$(post_new tools/call search_conversations "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/call\",\"params\":{\"name\":\"search_conversations\",\"arguments\":{\"query\":\"anything\"},$META}}")
errored=$(body | grep -c 'isError.:true' || true)
check "omitting cli is an error" "HTTP $code, isError seen $errored times" \
  "$([ "$errored" = 1 ] && echo 0 || echo 1)"

code=$(post_new tools/call index_status "{\"jsonrpc\":\"2.0\",\"id\":6,\"method\":\"tools/call\",\"params\":{\"name\":\"index_status\",\"arguments\":{},$META}}")
ok=$(body | grep -c 'per_cli' || true)
check "index_status needs no cli argument" "HTTP $code" \
  "$([ "$code" = 200 ] && [ "$ok" = 1 ] && echo 0 || echo 1)"

echo "Protocol era $OLD (sessions)"

printf '%s' "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"$OLD\",\"capabilities\":{},\"clientInfo\":{\"name\":\"check\",\"version\":\"1\"}}}" > /tmp/cs-mcp-req
code=$(curl -s -D /tmp/cs-mcp-head -o /tmp/cs-mcp-body -w '%{http_code}' -X POST "$URL" \
  -H 'Content-Type: application/json' -H "Accept: $ACCEPT" --data-binary @/tmp/cs-mcp-req)
SESSION=$(grep -i '^mcp-session-id' /tmp/cs-mcp-head | tr -d '\r' | awk '{print $2}')
check "initialize issues a session id" "HTTP $code, session ${SESSION:-none}" \
  "$([ "$code" = 200 ] && [ -n "$SESSION" ] && echo 0 || echo 1)"

printf '%s' '{"jsonrpc":"2.0","method":"notifications/initialized"}' > /tmp/cs-mcp-req
curl -s -o /dev/null -X POST "$URL" -H 'Content-Type: application/json' -H "Accept: $ACCEPT" \
  -H "Mcp-Session-Id: $SESSION" -H "MCP-Protocol-Version: $OLD" --data-binary @/tmp/cs-mcp-req

printf '%s' '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"index_status","arguments":{}}}' > /tmp/cs-mcp-req
code=$(curl -s -o /tmp/cs-mcp-body -w '%{http_code}' -X POST "$URL" \
  -H 'Content-Type: application/json' -H "Accept: $ACCEPT" \
  -H "Mcp-Session-Id: $SESSION" -H "MCP-Protocol-Version: $OLD" --data-binary @/tmp/cs-mcp-req)
ok=$(body | grep -c 'per_cli' || true)
check "tools/call on that session" "HTTP $code" \
  "$([ "$code" = 200 ] && [ "$ok" = 1 ] && echo 0 || echo 1)"

echo "Host and Origin"

code=$(post_new tools/list - "{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/list\",\"params\":{$META}}" -H 'Origin: http://evil.example')
check "a foreign Origin is rejected" "HTTP $code" "$([ "$code" = 403 ] && echo 0 || echo 1)"

code=$(post_new tools/list - "{\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"tools/list\",\"params\":{$META}}" -H 'Origin: http://127.0.0.1:7317')
check "our own Origin is accepted" "HTTP $code" "$([ "$code" = 200 ] && echo 0 || echo 1)"

code=$(post_new tools/list - "{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"tools/list\",\"params\":{$META}}")
check "no Origin at all is accepted" "HTTP $code" "$([ "$code" = 200 ] && echo 0 || echo 1)"

code=$(post_new tools/list - "{\"jsonrpc\":\"2.0\",\"id\":10,\"method\":\"tools/list\",\"params\":{$META}}" -H 'Host: evil.example')
check "a foreign Host is rejected" "HTTP $code" "$([ "$code" = 403 ] && echo 0 || echo 1)"

echo
if [ "$failures" -eq 0 ]; then
  echo "All MCP checks passed."
else
  echo "$failures MCP checks failed." >&2
  exit 1
fi
