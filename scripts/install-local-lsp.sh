#!/usr/bin/env sh
# Builds xml-lsp and installs it in place of the binary the extension downloads
# (extension work directory of Zed), with the name and checksum it verifies.
# The binary reports "xml-lsp <version> (local)" in --version.
set -eu

cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' extension.toml | head -n 1)
[ -n "$version" ] || { echo "cannot read the version of extension.toml" >&2; exit 1; }

case "$(uname -s)" in
  Darwin) default_data="$HOME/Library/Application Support/Zed"; suffix="" ;;
  Linux) default_data="${XDG_DATA_HOME:-$HOME/.local/share}/zed"; suffix="" ;;
  MINGW* | MSYS* | CYGWIN*) default_data="${LOCALAPPDATA:-$HOME/AppData/Local}/Zed"; suffix=".exe" ;;
  *) echo "unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac
data_dir="${ZED_DATA_DIR:-$default_data}"
work_dir="$data_dir/extensions/work/xml"

XML_LSP_LOCAL_BUILD=1 CARGO_TARGET_DIR=target/local cargo build --release -p xml-lsp
binary="target/local/release/xml-lsp$suffix"

mkdir -p "$work_dir"
target="$work_dir/xml-lsp-$version$suffix"
rm -f "$target" "$target.sha256"
cp "$binary" "$target"
chmod +x "$target"
if command -v sha256sum >/dev/null 2>&1; then
  hash=$(sha256sum "$target" | cut -d ' ' -f 1)
else
  hash=$(shasum -a 256 "$target" | cut -d ' ' -f 1)
fi
printf '%s\n' "$hash" > "$target.sha256"

echo "Installed $("$target" --version) at $target"
echo "Restart the language server in Zed (editor: restart language server)."
