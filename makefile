check: 
	cargo check
fmt:
	cargo +nightly fmt

lint: 
	cargo clippy

build:
	cargo build --all-features

test:
	cargo test

all: check fmt lint build
