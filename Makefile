.PHONY: test
test:
	RUSTFLAGS="-D warnings" cargo build
	RUSTFLAGS="-D warnings" cargo test
	RUSTFLAGS="-D warnings" cargo clippy
	cargo fmt --check
	cd nightly && \
		RUSTFLAGS="-D warnings" MIRIFLAGS="-Zmiri-disable-isolation" cargo miri test --manifest-path=../Cargo.toml
