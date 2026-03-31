.DEFAULT_GOAL := help

.PHONY: help fmt fmt-check check build build-release test test-ci test-cli test-cli-blackbox test-cli-real test-cli-all test-server-retrieval-evals validate install-from-source

help:
	@printf "%s\n" \
		"Memory Bank Make targets" \
		"" \
		"  make fmt               Format the workspace" \
		"  make fmt-check         Check formatting without rewriting files" \
		"  make check             Run cargo check for the full workspace" \
		"  make build             Build the full workspace" \
		"  make build-release     Build release binaries for the full workspace" \
		"  make test              Run the default local test suite (includes mb black-box tests)" \
		"  make test-ci           Run the CI/release validation suite (skips mb black-box tests)" \
		"  make test-cli          Run memory-bank-cli unit/binary tests only" \
		"  make test-cli-blackbox Run mb black-box integration tests only" \
		"  make test-cli-real     Run opt-in real installed-CLI tests for memory-bank-cli" \
		"  make test-cli-all      Run all memory-bank-cli tests, including real installed-CLI tests" \
		"  make test-server-retrieval-evals  Run opt-in real-encoder retrieval evals for memory-bank-server" \
		"  make validate          Run fmt-check, check, and test-ci" \
		"  make install-from-source  Build and install this checkout into ~/.memory_bank"

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

check:
	cargo check --workspace

build:
	cargo build --workspace

build-release:
	cargo build --workspace --release

test:
	cargo test --workspace

test-ci:
	cargo test --workspace --exclude memory-bank-cli
	cargo test -p memory-bank-cli --lib --bins

test-cli:
	cargo test -p memory-bank-cli --lib --bins

test-cli-blackbox:
	cargo test -p memory-bank-cli --test mb_blackbox

test-cli-real:
	MEMORY_BANK_REAL_BIN_TESTS=1 cargo test -p memory-bank-cli real_

test-cli-all:
	MEMORY_BANK_REAL_BIN_TESTS=1 cargo test -p memory-bank-cli

test-server-retrieval-evals:
	MEMORY_BANK_RETRIEVAL_EVALS=1 cargo test -p memory-bank-server retrieval_eval:: -- --ignored --nocapture

validate: fmt-check check test-ci

install-from-source:
	./install.sh --from-source
