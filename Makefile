SHELL := /bin/bash
export PATH := /opt/homebrew/bin:$(PATH)

NAME := conversation-search-v2
LABEL := io.github.curiouself.conversation-search
BIN := $(HOME)/.local/bin/conversation-search
PLIST := $(HOME)/Library/LaunchAgents/$(LABEL).plist
STATE_DIR := $(HOME)/Library/Application Support/conversation-search
LOG_DIR := $(HOME)/Library/Logs/conversation-search

.PHONY: install uninstall purge test

## Build, install under launchd, and register with both CLIs.
install:
	@scripts/install.sh

## Stop and unregister, keeping the index store and the logs.
uninstall:
	@launchctl bootout gui/$$(id -u)/$(LABEL) 2>/dev/null || true
	@claude mcp remove --scope user $(NAME) 2>/dev/null || true
	@codex mcp remove $(NAME) 2>/dev/null || true
	@rm -f "$(PLIST)" "$(BIN)"
	@echo "Removed the agent, the binary, and both v2 registrations."
	@echo "State and logs kept. 'make purge' deletes those."

## Delete the index store and the logs, after confirmation.
purge:
	@echo "This will permanently delete:"
	@echo "  $(STATE_DIR)"
	@echo "  $(LOG_DIR)"
	@echo
	@echo "Transcripts under ~/.claude/projects and ~/.codex/sessions are NOT touched."
	@read -p "Type 'delete' to confirm: " answer; \
	if [ "$$answer" = "delete" ]; then \
	  rm -rf "$(STATE_DIR)" "$(LOG_DIR)"; \
	  echo "Deleted. The next start rebuilds the index from the transcripts."; \
	else \
	  echo "Cancelled."; \
	fi

## Unit tests, the parser checks over synthetic fixtures, and the tailing scenarios.
test:
	cargo test --release
