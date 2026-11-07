.PHONY: build test lint fmt check install run

build:
	cargo build --release

test:
	cargo nextest run

lint:
	cargo clippy --all-targets -- -D warnings

fmt:
	cargo fmt

check: fmt lint test

# Rebuilds in release mode and overwrites ~/.cargo/bin/w0rk
install:
	cargo install --path . --locked

run:
	cargo run
