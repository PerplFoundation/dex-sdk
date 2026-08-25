check: 
	cargo check
fmt:
	cargo +nightly-2026-08-22 fmt

lint: 
	cargo clippy

build:
	cargo build --all-features

test:
	cargo test -- --no-capture

all: check fmt lint build test
