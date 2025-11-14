fmt:
	cargo fmt

clippy:
	cargo clippy --all-targets -- -D warnings
