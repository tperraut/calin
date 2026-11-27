# Calin: one entry point for every routine command.
# The toolchains live in the home directory, so put them on PATH here
# instead of relying on the caller's shell profile.
SHELL := /bin/bash
export PATH := $(HOME)/.cargo/bin:$(HOME)/.local/share/vite-plus/bin:$(PATH)

WEB := apps/web
GEN := $(WEB)/src/gen

.PHONY: help check check-rust check-web gen gen-check fmt build dev-server dev-web install e2e

help: ## list targets
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | sed -E 's/:.*## /\t/' | column -t -s "$$(printf '\t')"

install: ## install web dependencies
	cd $(WEB) && vp install

check: check-rust gen-check check-web ## the validation gate: run before every commit

check-rust: ## format, lint and test the Rust workspace
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test --workspace -q

check-web: ## format, lint, type-check, test and build the web app
	cd $(WEB) && vp check && vp test run && vp build

gen: ## regenerate the TypeScript wire types from calin-proto
	rm -rf $(GEN)
	cargo test -q -p calin-proto export_bindings

gen-check: gen ## fail when the generated types differ from what is committed
	@if [ -n "$$(git status --porcelain -- $(GEN))" ]; then \
	  echo "error: $(GEN) is out of date; run 'make gen' and commit the result"; \
	  git status --short -- $(GEN); exit 1; fi

fmt: ## format everything
	cargo fmt --all
	cd $(WEB) && vp fmt

build: ## release build: web app first, then the binary that embeds it
	cd $(WEB) && vp build
	cargo build --release -p calin-cli

dev-server: ## run the server with the mock runner on 127.0.0.1:4680
	CALIN_DRY_RUN=1 cargo run -p calin-cli -- serve

dev-web: ## run the web dev server, proxying /api to the server
	cd $(WEB) && vp dev

e2e: ## browser end-to-end checks against the built binary with the mock runner (needs a Chromium-family browser)
	cd $(WEB) && vp build
	cargo build -q -p calin-cli
	cargo build -q -p calin-auth --features test-provider --example test-provider
	cd e2e && vp install && node run.mjs ../target/debug/calin \
	  && node signin.mjs ../target/debug/calin ../target/debug/examples/test-provider

