#!/usr/bin/env bash
# Compiles the tree-sitter queries of languages/ against the grammars pinned in
# extension.toml and checks the highlighting of fragments (tools/query-check).
# Needs git, a C compiler and cargo. Usage: scripts/check-queries.sh
set -euo pipefail
cd "$(dirname "$0")/.."

# The [grammars.xml] section (the dtd grammar is pinned to the same commit).
section=$(awk '/^\[grammars\.xml\]/{found=1; next} /^\[/{found=0} found' extension.toml)
commit=$(echo "$section" | grep '^commit' | sed 's/.*"\(.*\)".*/\1/')
repository=$(echo "$section" | grep '^repository' | sed 's/.*"\(.*\)".*/\1/')
directory="${TS_XML_DIR:-target/tree-sitter-xml}"

if [ ! -d "$directory/.git" ]; then
    git clone --quiet "$repository" "$directory"
fi
git -C "$directory" checkout --quiet "$commit"

TS_XML_DIR="$(cd "$directory" && pwd)" cargo run --quiet --manifest-path tools/query-check/Cargo.toml
