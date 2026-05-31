.PHONY: test test-clean
test:
	RUSTFLAGS="-D warnings" cargo build
	RUSTFLAGS="-D warnings" cargo test
	RUSTFLAGS="-D warnings" cargo clippy
	cargo fmt --check
	cd nightly && \
		RUSTFLAGS="-D warnings" cargo miri test --manifest-path=../Cargo.toml

# Same as `test` but runs `cargo clean` first. Use before pushing changes
# that modify shared IR struct shapes (e.g. fields on types in
# tools/build_config/src/ir.rs): cargo's incremental cache has been observed
# to skip re-typechecking downstream tests when an upstream struct gains a
# field, hiding compile errors that only surface on a CI fresh checkout.
test-clean:
	cargo clean
	$(MAKE) test
