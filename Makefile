.PHONY: install-local-lsp ci fmt generated clippy test msrv wasm deny coverage conformance queries benchmarks

# Run every pull-request validation job locally.
ci: fmt generated clippy test msrv wasm deny coverage conformance queries benchmarks

fmt:
	cargo fmt --all -- --check

generated:
	test -z "$$(git ls-files -- 'grammars/**' '*.wasm' 'target/**')"

clippy:
	cargo clippy --workspace --all-targets -- -D warnings
	cargo clippy -p zed-xml --target wasm32-wasip2 -- -D warnings

test:
	cargo test --workspace

msrv:
	cargo build --workspace --all-targets --locked

wasm:
	cargo build -p zed-xml --target wasm32-wasip2 --release --locked

deny:
	@command -v cargo-deny >/dev/null 2>&1 || { \
		echo "cargo-deny is required; install it with: cargo install cargo-deny --locked" >&2; \
		exit 127; \
	}
	cargo deny check advisories bans licenses sources

coverage:
	cargo llvm-cov --workspace --no-report
	mkdir -p coverage
	cargo llvm-cov report --lcov --output-path coverage/lcov.info
	cargo llvm-cov report --html --output-dir coverage
	cargo llvm-cov report --summary-only > coverage/summary.txt

conformance:
	XML_TEST_SUITES_REQUIRED=1 scripts/fetch-test-suites.sh
	cargo test --release -p xml-conformance
	cargo test --release -p xml-lsp fixture_smoke

queries:
	scripts/check-queries.sh

benchmarks:
	cargo bench -p xml-core --bench xml_core -p xsd-core --bench xsd_core -p xml-lsp --bench server -- --quick --noplot
	cargo test --release -p xml-lsp latency -- --nocapture

# Build xml-lsp (reporting "(local)" in --version) and install it in place of
# the downloaded binary in the Zed extension work directory.
install-local-lsp:
	scripts/install-local-lsp.sh
