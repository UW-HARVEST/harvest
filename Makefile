.PHONY: test
test:
	RUSTFLAGS="-D warnings" cargo build
	RUSTFLAGS="-D warnings" cargo test
	RUSTFLAGS="-D warnings" cargo clippy
	cargo fmt --check
	cd nightly && \
		RUSTFLAGS="-D warnings" cargo miri test --manifest-path=../Cargo.toml


# Code style; defines `style-check` and `style-fix`.
ifeq (,$(wildcard .plume-scripts))
dummy := $(shell git clone --depth=1 -q https://github.com/plume-lib/plume-scripts.git .plume-scripts)
endif
include .plume-scripts/code-style.mak
