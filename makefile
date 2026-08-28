TOML_FILE := Cargo.toml
MASTER_BRANCH := main
DEV_BRANCH := dev

VERSION := $(shell sed -n 's/^version *= *"\(.*\)"/\1/p' $(TOML_FILE))

.PHONY: dev-version release tag lint fmt build test run codegen all


dev-version:
	@set -e; \
	git fetch origin $(MASTER_BRANCH); \
	CURRENT_VERSION=$$(sed -n 's/^version *= *"\(.*\)"/\1/p' $(TOML_FILE)); \
	MASTER_VERSION=$$(git show origin/$(MASTER_BRANCH):$(TOML_FILE) \
		| sed -n 's/^version *= *"\(.*\)"/\1/p'); \
	echo "dev version:    $$CURRENT_VERSION"; \
	echo "master version: $$MASTER_VERSION"; \
	if [ "$$CURRENT_VERSION" != "$$MASTER_VERSION" ]; then \
		echo "dev has already been versioned; nothing to do."; \
		exit 0; \
	fi; \
	NEW_VERSION=$$(echo "$$CURRENT_VERSION" \
		| awk -F. '{printf "%d.%d.%d", $$1, $$2, $$3+1}'); \
	echo "Bumping $$CURRENT_VERSION -> $$NEW_VERSION"; \
	sed -i.bak \
		's/^version *= *".*"/version = "'"$$NEW_VERSION"'"/' \
		$(TOML_FILE); \
	rm -f $(TOML_FILE).bak; \
	cargo check; \
	git add $(TOML_FILE) Cargo.lock; \
	git commit -m "Bump version to v$$NEW_VERSION"; \
	git push origin HEAD:$(DEV_BRANCH)


release:
	@set -e; \
	VERSION=$$(sed -n 's/^version *= *"\(.*\)"/\1/p' $(TOML_FILE)); \
	echo "Releasing v$$VERSION"; \
	git tag -a "v$$VERSION" -m "Release v$$VERSION"; \
	git push origin "v$$VERSION"; \
	gh release create "v$$VERSION" \
		--title "v$$VERSION" \
		--notes "Release v$$VERSION"
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
