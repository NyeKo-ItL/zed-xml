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
ifeq ($(OS),Windows_NT)
# Git for Windows may set GNU make's shell to sh.exe. Use cmd.exe for the
# PowerShell recipe so make does not expand PowerShell's `$` variables.
install-local-lsp: SHELL := cmd.exe
install-local-lsp: .SHELLFLAGS := /C
install-local-lsp:
	@powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "$$ErrorActionPreference = 'Stop'; $$versionLine = Get-Content extension.toml | Where-Object { $$_.TrimStart().StartsWith('version = ') } | Select-Object -First 1; if (-not $$versionLine) { throw 'cannot read the version of extension.toml' }; $$version = $$versionLine.Trim().Split([char]34)[1]; $$dataDir = if ($$env:ZED_DATA_DIR) { $$env:ZED_DATA_DIR } else { Join-Path $$env:LOCALAPPDATA 'Zed' }; $$workDir = Join-Path $$dataDir 'extensions\\work\\xml'; $$binary = Join-Path (Get-Location) 'target\\local\\release\\xml-lsp.exe'; $$target = Join-Path $$workDir ('xml-lsp-' + $$version + '.exe'); $$processName = [System.IO.Path]::GetFileNameWithoutExtension($$target); $$stopLsp = { Get-Process -Name @('xml-lsp', $$processName) -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue; Get-Process -ErrorAction SilentlyContinue | Where-Object { try { $$_.Path -eq $$target } catch { $$false } } | Stop-Process -Force -ErrorAction SilentlyContinue }; New-Item -ItemType Directory -Force -Path $$workDir | Out-Null; & $$stopLsp; $$env:XML_LSP_LOCAL_BUILD = '1'; $$env:CARGO_TARGET_DIR = 'target/local'; cargo clean --release -p xml-lsp; if ($$LASTEXITCODE -ne 0) { exit $$LASTEXITCODE }; cargo build --release -p xml-lsp; if ($$LASTEXITCODE -ne 0) { exit $$LASTEXITCODE }; for ($$attempt = 0; $$attempt -lt 40; $$attempt++) { & $$stopLsp; Remove-Item -Force -ErrorAction SilentlyContinue $$target, ($$target + '.sha256'); try { Copy-Item -Force -LiteralPath $$binary -Destination $$target -ErrorAction Stop; break } catch { if ($$attempt -eq 39) { throw }; Start-Sleep -Milliseconds 250 } }; $$sha = [System.Security.Cryptography.SHA256]::Create(); $$stream = [System.IO.File]::OpenRead($$target); try { $$hash = -join ($$sha.ComputeHash($$stream) | ForEach-Object { $$_.ToString('x2') }) } finally { $$stream.Dispose(); $$sha.Dispose() }; [System.IO.File]::WriteAllText($$target + '.sha256', $$hash + [Environment]::NewLine, [System.Text.Encoding]::ASCII); Write-Host ('Installed ' + (& $$target --version) + ' at ' + $$target); Write-Host 'Zed should restart the language server automatically.'"

reset-local-lsp: SHELL := cmd.exe
reset-local-lsp: .SHELLFLAGS := /C
reset-local-lsp:
	@powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "$$ErrorActionPreference = 'Stop'; $$dataDir = if ($$env:ZED_DATA_DIR) { $$env:ZED_DATA_DIR } else { Join-Path $$env:LOCALAPPDATA 'Zed' }; $$workDir = Join-Path $$dataDir 'extensions\\work\\xml'; $$stopLsp = { Get-Process -Name 'xml-lsp*' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue }; for ($$attempt = 0; $$attempt -lt 40; $$attempt++) { & $$stopLsp; try { Get-ChildItem -Path $$workDir -Filter 'xml-lsp-*' -File -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction Stop; break } catch { if ($$attempt -eq 39) { throw }; Start-Sleep -Milliseconds 250 } }; Remove-Item -Recurse -Force -ErrorAction SilentlyContinue 'target\\local'; Write-Host 'Removed the local xml-lsp cache. Zed will download the official LSP again.'"
else
install-local-lsp:
	@set -eu; \
	version=$$(sed -n 's/^version = "\\(.*\\)"/\\1/p' extension.toml | sed -n '1p'); \
	[ -n "$$version" ] || { echo "cannot read the version of extension.toml" >&2; exit 1; }; \
	case "$$(uname -s)" in \
	  Darwin) data_dir="$${ZED_DATA_DIR:-$$HOME/Library/Application Support/Zed}" ;; \
	  Linux) data_dir="$${ZED_DATA_DIR:-$${XDG_DATA_HOME:-$$HOME/.local/share}/zed}" ;; \
	  *) echo "unsupported OS: $$(uname -s)" >&2; exit 1 ;; \
	esac; \
	work_dir="$$data_dir/extensions/work/xml"; \
		XML_LSP_LOCAL_BUILD=1 CARGO_TARGET_DIR=target/local cargo clean --release -p xml-lsp; \
		XML_LSP_LOCAL_BUILD=1 CARGO_TARGET_DIR=target/local cargo build --release -p xml-lsp; \
	binary="target/local/release/xml-lsp"; target="$$work_dir/xml-lsp-$$version"; \
	mkdir -p "$$work_dir"; rm -f "$$target" "$$target.sha256"; cp "$$binary" "$$target"; chmod +x "$$target"; \
	if command -v sha256sum >/dev/null 2>&1; then hash=$$(sha256sum "$$target" | cut -d ' ' -f 1); else hash=$$(shasum -a 256 "$$target" | cut -d ' ' -f 1); fi; \
	printf '%s\\n' "$$hash" > "$$target.sha256"; \
	echo "Installed $$($$target --version) at $$target"; echo "Restart the language server in Zed (editor: restart language server)."

reset-local-lsp:
	@set -eu; \
	case "$$(uname -s)" in \
	  Darwin) data_dir="$${ZED_DATA_DIR:-$$HOME/Library/Application Support/Zed}" ;; \
	  Linux) data_dir="$${ZED_DATA_DIR:-$${XDG_DATA_HOME:-$$HOME/.local/share}/zed}" ;; \
	  *) echo "unsupported OS: $$(uname -s)" >&2; exit 1 ;; \
	esac; \
	pkill -f 'xml-lsp' 2>/dev/null || true; \
	rm -f "$$data_dir/extensions/work/xml"/xml-lsp-*; \
	rm -rf target/local; \
	echo 'Removed the local xml-lsp cache. Zed will download the official LSP again.'
endif
