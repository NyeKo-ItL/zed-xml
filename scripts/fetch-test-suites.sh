#!/usr/bin/env bash
# Fetches the external conformance suites used by crates/xml-conformance into
# target/test-suites (or $XML_TEST_SUITES_DIR). Every source is pinned, so a
# suite change is always a reviewed change to this script and the baselines.
#
#   scripts/fetch-test-suites.sh            # all suites
#   scripts/fetch-test-suites.sh xmlconf    # one suite
#
# Suites and licences:
#   xmlconf   W3C XML Conformance Test Suite 20130923, via the npm package
#             xml-conformance-suite (MIT packaging, W3C Software Notice and
#             License for the suite)
#   xsdtests  W3C XML Schema test suite (github.com/w3c/xsdtests, W3C Document
#             Notice and License)
#   libxml2   libxml2 schema regression tests (MIT)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="${XML_TEST_SUITES_DIR:-$ROOT/target/test-suites}"

XMLCONF_URL="https://registry.npmjs.org/xml-conformance-suite/-/xml-conformance-suite-1.2.0.tgz"
XMLCONF_SHA256="9b3d0a175832a9746bb0aa6fa6d0a1b763cb49552f83f624bde42adc94e54282"
XSDTESTS_REPO="https://github.com/w3c/xsdtests.git"
XSDTESTS_COMMIT="7bc3365c652a322f3d762021b3879eb92dae7e30"
LIBXML2_REPO="https://github.com/GNOME/libxml2.git"
LIBXML2_COMMIT="c43dc98d27ac315a48d93dbd399c6c22cf7125b1"

mkdir -p "$DEST"

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# git_at_commit <repo> <commit> <directory> [sparse paths...]
git_at_commit() {
  local repo="$1" commit="$2" directory="$3"
  shift 3
  if [[ -d "$directory/.git" ]] && [[ "$(git -C "$directory" rev-parse HEAD)" == "$commit" ]]; then
    return
  fi
  rm -rf "$directory"
  git init -q "$directory"
  git -C "$directory" remote add origin "$repo"
  if [[ $# -gt 0 ]]; then
    git -C "$directory" sparse-checkout set "$@"
  fi
  git -C "$directory" fetch -q --depth 1 origin "$commit"
  git -C "$directory" checkout -q FETCH_HEAD
}

fetch_xmlconf() {
  local directory="$DEST/xmlconf" archive="$DEST/xmlconf.tgz"
  if [[ -f "$directory/.sha256" ]] && [[ "$(cat "$directory/.sha256")" == "$XMLCONF_SHA256" ]]; then
    return
  fi
  curl -fsSL -o "$archive" "$XMLCONF_URL"
  local actual
  actual="$(sha256 "$archive")"
  if [[ "$actual" != "$XMLCONF_SHA256" ]]; then
    echo "xmlconf: checksum mismatch ($actual)" >&2
    exit 1
  fi
  rm -rf "$directory"
  mkdir -p "$directory"
  tar -xzf "$archive" -C "$directory" --strip-components=1
  rm "$archive"
  echo "$XMLCONF_SHA256" > "$directory/.sha256"
}

fetch_xsdtests() {
  git_at_commit "$XSDTESTS_REPO" "$XSDTESTS_COMMIT" "$DEST/xsdtests"
}

fetch_libxml2() {
  git_at_commit "$LIBXML2_REPO" "$LIBXML2_COMMIT" "$DEST/libxml2" test/schemas result/schemas
}

suites=("$@")
if [[ ${#suites[@]} -eq 0 ]]; then
  suites=(xmlconf xsdtests libxml2)
fi
for suite in "${suites[@]}"; do
  echo "fetching $suite into $DEST/$suite"
  "fetch_$suite"
done
