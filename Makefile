# abtop — Makefile
#
# Common development and install targets. The canonical release path is
# `cargo-dist` via GitHub Actions (see dist-workspace.toml); this Makefile
# exposes local shortcuts for day-to-day work and user-side installation.

CARGO       ?= cargo
PREFIX      ?= $(HOME)/.local
BINDIR      ?= $(PREFIX)/bin
DESTDIR     ?=
BIN_NAME    := abtop
RELEASE_BIN := target/release/$(BIN_NAME)

.DEFAULT_GOAL := help

.PHONY: help build release run once demo test clippy fmt fmt-check check \
        install install-cargo install-user install-system uninstall \
        setup update dist clean

help: ## Show this help
	@awk 'BEGIN {FS = ":.*##"; printf "\nUsage: make <target>\n\nTargets:\n"} \
		/^[a-zA-Z_-]+:.*?##/ { printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

# ── build ──────────────────────────────────────────────────────────────────

build: ## Debug build
	$(CARGO) build

release: ## Optimized release build -> $(RELEASE_BIN)
	$(CARGO) build --release

run: ## Run the TUI (debug build)
	$(CARGO) run

once: ## Print a one-shot snapshot and exit
	$(CARGO) run -- --once

demo: ## Run with fake data (no live agents required)
	$(CARGO) run -- --demo

# ── quality ────────────────────────────────────────────────────────────────

test: ## Run unit tests
	$(CARGO) test

clippy: ## Lint with -D warnings
	$(CARGO) clippy --all-targets -- -D warnings

fmt: ## Format the workspace
	$(CARGO) fmt --all

fmt-check: ## Check formatting without modifying files
	$(CARGO) fmt --all -- --check

check: fmt-check clippy test ## Run fmt-check + clippy + tests

# ── install ────────────────────────────────────────────────────────────────

install: install-user ## Alias for install-user

install-cargo: ## Install via `cargo install --path .` (~/.cargo/bin)
	$(CARGO) install --path . --locked --force

install-user: release ## Install release binary to $(BINDIR) (default: ~/.local/bin)
	@mkdir -p "$(DESTDIR)$(BINDIR)"
	install -m 0755 "$(RELEASE_BIN)" "$(DESTDIR)$(BINDIR)/$(BIN_NAME)"
	@echo ""
	@echo "installed: $(DESTDIR)$(BINDIR)/$(BIN_NAME)"
	@echo "make sure $(BINDIR) is on your PATH, e.g.:"
	@echo "  echo 'export PATH=\"$(BINDIR):\$$PATH\"' >> ~/.zshrc"

install-system: release ## Install to /usr/local/bin (may require sudo)
	@mkdir -p "$(DESTDIR)/usr/local/bin"
	install -m 0755 "$(RELEASE_BIN)" "$(DESTDIR)/usr/local/bin/$(BIN_NAME)"
	@echo "installed: $(DESTDIR)/usr/local/bin/$(BIN_NAME)"

uninstall: ## Remove binaries from $(BINDIR), /usr/local/bin and ~/.cargo/bin
	-rm -f "$(DESTDIR)$(BINDIR)/$(BIN_NAME)"
	-rm -f "$(DESTDIR)/usr/local/bin/$(BIN_NAME)"
	-rm -f "$(HOME)/.cargo/bin/$(BIN_NAME)"

# ── post-install helpers ───────────────────────────────────────────────────

setup: ## Install Claude Code StatusLine rate-limit hook
	$(BIN_NAME) --setup || $(CARGO) run -- --setup

update: ## Self-update via the official GitHub releases installer
	$(BIN_NAME) --update

# ── release / housekeeping ─────────────────────────────────────────────────

dist: ## Local multi-platform build via cargo-dist (requires `dist` installed)
	dist build

clean: ## Remove target/ and any local build artifacts
	$(CARGO) clean
