#!/bin/bash
# Build, install, and register the conversation-search v2 daemon.
#
# Registration goes through `claude mcp` and `codex mcp` only. This script never
# edits ~/.claude.json or ~/.codex/config.toml, and never reads, changes, or
# removes the v1 entries.
#
#   scripts/install.sh          # build, swap, health-check, register
#   FORCE=1 scripts/install.sh  # also re-register a v2 entry pointing elsewhere
set -euo pipefail

# Homebrew first: the Homebrew codex is the one that understands `mcp add --url`.
export PATH="/opt/homebrew/bin:$PATH"

NAME=conversation-search-v2
LABEL=io.github.curiouself.conversation-search
URL=http://127.0.0.1:7317/mcp
HEALTH=http://127.0.0.1:7317/healthz

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="$HOME/.local/bin"
BIN="$BIN_DIR/conversation-search"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
STATE_DIR="$HOME/Library/Application Support/conversation-search"
LOG_DIR="$HOME/Library/Logs/conversation-search"
STORE_PREV="$STATE_DIR/store.prev"
DOMAIN="gui/$(id -u)"
SERVICE="$DOMAIN/$LABEL"

step()  { printf '\n\033[1m==> %s\033[0m\n' "$1"; }

# `launchctl bootout` returns before the job has actually gone, and bootstrapping
# a label that is still present fails with "Input/output error". Wait it out.
stop_agent() {
  launchctl bootout "$SERVICE" 2>/dev/null || true
  for _ in $(seq 1 60); do
    launchctl print "$SERVICE" >/dev/null 2>&1 || return 0
    sleep 0.25
  done
  return 0
}

start_agent() {
  for attempt in 1 2 3; do
    if launchctl bootstrap "$DOMAIN" "$PLIST" 2>/dev/null; then
      return 0
    fi
    sleep 1
  done
  launchctl bootstrap "$DOMAIN" "$PLIST"
}

# `current` is a symlink to a directory, so plain `mv` would move the new link
# *inside* it. `ln -sfn` replaces the symlink itself.
point_current_at() {
  ln -sfn "$1" "$STATE_DIR/stores/current"
  rm -f "$STATE_DIR/stores/current.switching"
}
note()  { printf '    %s\n' "$1"; }
die()   { printf '\033[31mERROR: %s\033[0m\n' "$1" >&2; exit 1; }

health_field() { # health_field <key>
  curl -s --max-time 3 "$HEALTH" 2>/dev/null | sed -n "s/.*\"$1\":\"\{0,1\}\([^,\"}]*\)\"\{0,1\}.*/\1/p" | head -1
}

# ---------------------------------------------------------------- preflight --
step "Preflight"
command -v claude >/dev/null || die "claude is not on PATH"
command -v codex  >/dev/null || die "codex is not on PATH"
claude mcp add --help 2>&1 | grep -q -- '--transport' \
  || die "this claude does not support 'mcp add --transport'"
codex mcp add --help 2>&1 | grep -q -- '--url' \
  || die "this codex does not support 'mcp add --url'"
note "claude $(claude --version 2>&1 | head -1)"
note "codex  $(codex --version 2>&1 | head -1) at $(command -v codex)"

if lsof -nP -iTCP:7317 -sTCP:LISTEN >/dev/null 2>&1; then
  owner="$(health_field name)"
  if [ "$owner" != "conversation-search" ]; then
    note "Port 7317 is held by:"
    lsof -nP -iTCP:7317 -sTCP:LISTEN | sed 's/^/      /'
    die "something other than conversation-search is listening on 7317"
  fi
  note "port 7317 already answers as conversation-search (this daemon)"
fi

# -------------------------------------------------------------------- build --
step "Build"
( cd "$REPO" && cargo build --release ) || die "cargo build --release failed"
VERSION="$("$REPO/target/release/conversation-search" --version | awk '{print $2}')"
note "built version $VERSION"

# -------------------------------------------------------------------- stage --
step "Stage"
mkdir -p "$BIN_DIR" "$STATE_DIR" "$LOG_DIR" "$HOME/Library/LaunchAgents"
cp "$REPO/target/release/conversation-search" "$BIN.new"
chmod +x "$BIN.new"

HAD_BIN=0
if [ -f "$BIN" ]; then
  cp "$BIN" "$BIN.prev"
  HAD_BIN=1
fi
HAD_PLIST=0
if [ -f "$PLIST" ]; then
  cp "$PLIST" "$PLIST.prev"
  HAD_PLIST=1
fi

PLIST_NEW="$(mktemp)"
sed -e "s|__BINARY__|$BIN|g" -e "s|__LOG_DIR__|$LOG_DIR|g" \
  "$REPO/launchd/$LABEL.plist.template" > "$PLIST_NEW"

# Record where `current` points before anything is booted out, so a rollback can
# put the old binary back in front of the store it was built for.
if [ -L "$STATE_DIR/stores/current" ]; then
  readlink "$STATE_DIR/stores/current" > "$STORE_PREV"
  note "current store recorded in store.prev"
fi

# The rollback trap is armed just before the swap and disarmed once everything
# has succeeded, so a failure anywhere in swap, health, or registration undoes
# exactly what this run changed.
ARMED=0
FINISHED=0
REGISTERED_CLAUDE=0
REGISTERED_CODEX=0
PRIOR_CLAUDE_URL=""
PRIOR_CODEX_URL=""

rollback() {
  printf '\n\033[31m==> Rolling back\033[0m\n'
  stop_agent

  if [ "$HAD_BIN" = 1 ]; then
    mv -f "$BIN.prev" "$BIN"
    note "restored the previous binary"
  else
    rm -f "$BIN"
    note "removed the binary this run installed"
  fi
  if [ "$HAD_PLIST" = 1 ]; then
    mv -f "$PLIST.prev" "$PLIST"
  else
    rm -f "$PLIST"
  fi
  rm -f "$BIN.new" "$PLIST_NEW"

  if [ -s "$STORE_PREV" ]; then
    point_current_at "$(cat "$STORE_PREV")"
    note "pointed current back at the recorded store"
  fi

  # Undo only what this run changed. v1 entries are never touched.
  if [ "$REGISTERED_CLAUDE" = 1 ]; then
    if [ -n "$PRIOR_CLAUDE_URL" ]; then
      claude mcp remove --scope user "$NAME" >/dev/null 2>&1 || true
      claude mcp add --transport http --scope user "$NAME" "$PRIOR_CLAUDE_URL" >/dev/null 2>&1 || true
      note "restored the previous Claude Code registration"
    else
      claude mcp remove --scope user "$NAME" >/dev/null 2>&1 || true
      note "removed the Claude Code registration this run added"
    fi
  fi
  if [ "$REGISTERED_CODEX" = 1 ]; then
    if [ -n "$PRIOR_CODEX_URL" ]; then
      codex mcp remove "$NAME" >/dev/null 2>&1 || true
      codex mcp add "$NAME" --url "$PRIOR_CODEX_URL" >/dev/null 2>&1 || true
      note "restored the previous Codex registration"
    else
      codex mcp remove "$NAME" >/dev/null 2>&1 || true
      note "removed the Codex registration this run added"
    fi
  fi

  if [ "$HAD_BIN" = 1 ] && [ -f "$PLIST" ]; then
    start_agent 2>/dev/null || true
    note "restarted the previous daemon"
  fi
  printf '\033[31mInstall failed and was rolled back.\033[0m\n' >&2
}
# An explicit `exit` does not fire an ERR trap, so the guard hangs off EXIT.
on_exit() {
  status=$?
  if [ "$ARMED" = 1 ] && [ "$FINISHED" != 1 ]; then
    rollback
  fi
  exit $status
}
trap on_exit EXIT

# --------------------------------------------------------------------- swap --
step "Swap"
ARMED=1
stop_agent
mv -f "$BIN.new" "$BIN"
mv -f "$PLIST_NEW" "$PLIST"
start_agent
note "agent bootstrapped"

# ------------------------------------------------------------------- health --
step "Health"
deadline=$(( $(date +%s) + 30 ))
until [ "$(health_field name)" = "conversation-search" ]; do
  [ "$(date +%s)" -lt "$deadline" ] || die "no healthy /healthz within 30s"
  sleep 1
done
got_version="$(health_field version)"
[ "$got_version" = "$VERSION" ] || die "/healthz reports version $got_version, expected $VERSION"
note "live: version $got_version, pid $(health_field pid)"

deadline=$(( $(date +%s) + 1200 ))
until [ "$(health_field ready)" = "true" ]; do
  [ "$(date +%s)" -lt "$deadline" ] || die "not ready within 20 minutes"
  note "building: $(health_field files_indexed)/$(health_field files_total) files"
  sleep 10
done
note "ready: generation $(health_field generation)"

# ----------------------------------------------------------------- register --
step "Register with Claude Code"
if claude_entry="$(claude mcp get "$NAME" 2>&1)"; then
  if printf '%s' "$claude_entry" | grep -qF "$URL"; then
    note "already registered at $URL"
  elif [ "${FORCE:-0}" = 1 ]; then
    PRIOR_CLAUDE_URL="$(printf '%s' "$claude_entry" | sed -n 's/.*\(http[^ ]*\).*/\1/p' | head -1)"
    claude mcp remove --scope user "$NAME"
    claude mcp add --transport http --scope user "$NAME" "$URL"
    REGISTERED_CLAUDE=1
    note "re-registered (was $PRIOR_CLAUDE_URL)"
  else
    printf '%s\n' "$claude_entry" | sed 's/^/      /'
    die "$NAME is registered with Claude Code at a different URL; re-run with FORCE=1 to replace it"
  fi
else
  claude mcp add --transport http --scope user "$NAME" "$URL"
  REGISTERED_CLAUDE=1
  note "added"
fi

step "Register with Codex"
if codex_entry="$(codex mcp get "$NAME" 2>&1)"; then
  if printf '%s' "$codex_entry" | grep -qF "$URL"; then
    note "already registered at $URL"
  elif [ "${FORCE:-0}" = 1 ]; then
    PRIOR_CODEX_URL="$(printf '%s' "$codex_entry" | sed -n 's/.*\(http[^ ]*\).*/\1/p' | head -1)"
    codex mcp remove "$NAME"
    codex mcp add "$NAME" --url "$URL"
    REGISTERED_CODEX=1
    note "re-registered (was $PRIOR_CODEX_URL)"
  else
    printf '%s\n' "$codex_entry" | sed 's/^/      /'
    die "$NAME is registered with Codex at a different URL; re-run with FORCE=1 to replace it"
  fi
else
  codex mcp add "$NAME" --url "$URL"
  REGISTERED_CODEX=1
  note "added"
fi

FINISHED=1

step "Done"
"$BIN" status
printf '\n    Sessions started before now need a restart to see the new server.\n'
