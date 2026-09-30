# Contributing

Thanks for helping improve XML support in Zed. This guide covers the development setup, the build of the two artifacts (the Zed extension and the native language server), tests and the release process. The architecture and coding conventions are described in [AGENTS.md](AGENTS.md); user-facing documentation lives in [README.md](README.md) and [docs/](docs/).

## Repository layout

| Path | Content |
|------|---------|
| `extension.toml`, `src/lib.rs` | The Zed extension (a `wasm32-wasip2` module): grammars, languages and the command that starts `xml-lsp`. |
| `languages/xml`, `languages/dtd` | Zed language definitions and tree-sitter queries (highlights, brackets, outline, indents, injections, overrides, text objects). |
| `crates/xml-core` | XML parsing, tag scanner, well-formedness checks, formatter, text diff, basic completion. |
| `crates/xsd-core` | XSD parsing, schema resolution, component model, validation and completion. |
| `crates/dtd-core` | DTD parsing, entity expansion limits, content-model automata and validation. |
| `crates/xml-lsp` | The native language server binary (`xml-lsp`). |
| `crates/xml-conformance` | Test-only crate: specification cases, real-world documents, parser corpora and the W3C/libxml2 conformance suites, with baselines of known failures. |
| `tests/fixtures` | Shared test inputs: small hand-written cases, `real-world/` documents and `corpus/` files (sources and licences in `SOURCES.md`). |
| `scripts/fetch-test-suites.sh` | Downloads the pinned external conformance suites into `target/test-suites`. |
| `docs/` | Detailed user documentation (settings reference). |
| `.github/workflows/ci.yml` | Pull request checks and the release pipeline. |

## Prerequisites

- A Rust toolchain installed with [rustup](https://rustup.rs) with `rustfmt` and `clippy`. The minimum supported Rust version (MSRV) is **1.88** (`rust-version` in every `Cargo.toml`); CI builds with that version and with the latest stable.
- The WASI target used by Zed extensions: `rustup target add wasm32-wasip2`.
- [Zed](https://zed.dev) to try the extension.

## Build

The language server is a native executable:

```sh
cargo build -p xml-lsp            # target/debug/xml-lsp
cargo build -p xml-lsp --release  # target/release/xml-lsp
cargo run -p xml-lsp -- --version
```

It speaks LSP over stdio (`cargo run -p xml-lsp` and type JSON-RPC messages, or better, use it from Zed).

The extension is a WASI module, not a console binary:

```sh
cargo build --target wasm32-wasip2
```

Zed compiles the extension itself when it is installed as a dev extension; building it manually is only needed to check that `src/lib.rs` compiles for the right target.

## Development extension

1. Build the server: `cargo build -p xml-lsp`.
2. Point the extension at it through the environment of the shell Zed is started from:

   ```sh
   export XML_LSP_PATH="$PWD/target/debug/xml-lsp"
   ```

   ```powershell
   $env:XML_LSP_PATH = "$PWD\target\debug\xml-lsp.exe"
   ```

3. In Zed, run `zed: install dev extension` and select this repository. Installing a dev extension replaces the published one.
4. Open an XML file. Use `zed: open log` if the server does not start and `dev: open language server logs` to inspect the LSP traffic.
5. After changing the server, rebuild it with Cargo and run `editor: restart language server`. After changing `src/lib.rs`, `extension.toml` or `languages/`, use **Rebuild** in the dev extensions list (`zed: extensions`).

Keep `XML_LSP_PATH` set when opening XML files outside this repository, otherwise the extension downloads the released binary matching `extension.toml`'s version.

## Checks

Run these before pushing:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --target wasm32-wasip2   # when src/lib.rs, Cargo.toml or extension.toml changed
cargo deny check                     # when dependencies changed (cargo install cargo-deny)
```

Useful focused runs:

```sh
cargo test -p xml-core
cargo test -p xml-lsp -- rename      # tests whose name contains "rename"
cargo test -p zed-xml                # the extension's unit tests (host target)
```

The external conformance suites (W3C XML, W3C XSD, libxml2 schemas) are skipped until fetched; the `Conformance suites` CI job always runs them:

```sh
scripts/fetch-test-suites.sh
cargo test --release -p xml-conformance
cargo test --release -p xml-lsp fixture_smoke   # also covers fixtures over 64 KB
BLESS=1 cargo test --release -p xml-conformance # after a fix, rewrite the baselines and review the diff
```

A change that makes more cases pass must delete their lines from `crates/xml-conformance/baselines/`; a new failure fails the build. See [tests/README.md](tests/README.md) for what each suite checks and how to add fixtures.

### Continuous integration

Every pull request runs these jobs of `.github/workflows/ci.yml`; all must pass:

| Job | What it checks |
|-----|----------------|
| Format | `cargo fmt --all -- --check`, and that `target/`, `grammars/` and `*.wasm` are not tracked. |
| Clippy | `cargo clippy --workspace --all-targets -- -D warnings`, and the extension crate for `wasm32-wasip2`, with the pinned toolchain `CLIPPY_TOOLCHAIN` (top of `ci.yml`): any warning fails. |
| Tests | `cargo test --workspace` on Ubuntu, Windows and macOS. Keep tests portable: paths built with `Path::join` under `std::env::temp_dir()`, `file://` URIs derived from those paths, no assumption about the line endings of files read from disk (`.gitattributes` checks sources out with LF everywhere; fixtures stay byte-exact). |
| Minimum supported Rust version | Builds the workspace and the extension with the toolchain named by `rust-version` (1.88, required by edition 2024 `let` chains and by dependencies such as `encoding_rs`). The job fails if a crate declares another value. Raise it in every `Cargo.toml` at once, in its own pull request. |
| Extension (wasm32-wasip2) | Release build of the extension module, uploaded as the `zed-xml-extension-wasm` artifact. |
| Dependencies (cargo-deny) | `cargo deny check` with [`deny.toml`](deny.toml): licences compatible with MIT, RustSec advisories (vulnerable, unmaintained, unsound or yanked crates), wildcard versions, and crates.io as the only source. Duplicate versions are reported as warnings. |
| Coverage | `cargo llvm-cov` over the workspace; the summary is written to the job summary and the `coverage` artifact holds `lcov.info` and an HTML report (`html/index.html`). No external service is involved. |
| Conformance suites | The external suites and the fixture smoke test (below). |

The Clippy toolchain is pinned so that lints introduced by a new stable Rust release never break an unrelated pull request. To move to a newer release, in a dedicated `ci:` pull request: install it (`rustup toolchain install 1.NN -c clippy -t wasm32-wasip2`), run `cargo +1.NN clippy --workspace --all-targets -- -D warnings` and `cargo +1.NN clippy -p zed-xml --target wasm32-wasip2 -- -D warnings`, fix the new warnings (keeping code within the MSRV), and update `CLIPPY_TOOLCHAIN`. Locally, `cargo clippy` with your own toolchain is fine; when it reports a lint CI does not, fix it too.

Jobs share a [`Swatinem/rust-cache`](https://github.com/Swatinem/rust-cache) cache per pull request, and a new push cancels the checks still running for the previous one. To reproduce coverage locally: `cargo install cargo-llvm-cov`, then `cargo llvm-cov --workspace --html` (report in `target/llvm-cov/html`).

Tests live next to the code (`#[cfg(test)] mod tests`). Language-server features get unit tests in their module plus an LSP round-trip test in `crates/xml-lsp/src/main.rs`; see [AGENTS.md](AGENTS.md#testing) for the patterns.

## Pull requests

- Branch from `main` (or from the branch you build on when stacking pull requests) and keep each pull request focused on one feature or fix.
- Use [Conventional Commits](https://www.conventionalcommits.org) in English for commit messages and pull request titles: `feat: ...`, `fix: ...`, `docs: ...`, `test: ...`, `chore: ...`, `ci: ...`.
- Describe what changed, how it was tested, and how the behaviour compares to LemMinX (the Red Hat XML language server) or IntelliJ when relevant.
- Update [README.md](README.md) (features), [docs/configuration.md](docs/configuration.md) (settings) and the `Unreleased` section of [CHANGELOG.md](CHANGELOG.md).
- Never commit `target/`, `grammars/` (generated by Zed from `extension.toml`) or `*.wasm` files.

## Release process

Versions are kept identical in `Cargo.toml`, `crates/*/Cargo.toml`, `Cargo.lock` and `extension.toml`: the extension downloads the `xml-lsp` release whose version equals its own and checks it with `--version`. The pipeline in `.github/workflows/ci.yml` does the synchronization:

1. Move the `Unreleased` entries of `CHANGELOG.md` under the new version and merge that to `main`.
2. Push a tag `vX.Y.Z` on `main`. The `prepare_release_pr` job bumps every version to `X.Y.Z`, runs `cargo check --workspace` to refresh `Cargo.lock`, pushes the branch `release/version-sync-vX.Y.Z` and opens the pull request `chore: synchronize release version vX.Y.Z`.
3. Merge that pull request. The `native` job builds `xml-lsp` for `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu` and `aarch64-apple-darwin`; the `release` job moves the tag to the merge commit and publishes a GitHub release with the binaries (`xml-lsp-<target>[.exe]`) and generated notes.
4. To ship the extension through the Zed registry, update this repository's submodule and its `version` in `extensions.toml` in [zed-industries/extensions](https://github.com/zed-industries/extensions), as described in Zed's [publishing guide](https://zed.dev/docs/extensions/developing-extensions). The release must exist first, since the extension downloads the server from it.

## License

By contributing, you agree that your contributions are licensed under the [MIT License](LICENSE), the license of this repository (one of the licenses accepted by the Zed extension registry).
