# Large samples

Large documents written on one or two lines, used to check that the language
server formats big inputs (`formats_the_large_samples` in
`crates/xml-lsp/src/tests.rs`, `large_samples_are_formatted` in
`crates/xml-conformance/tests/large_samples.rs`). They are kept out of
`tests/fixtures` so that the per-request smoke tests do not walk them.

| File | Size | Content |
|------|------|---------|
| `complex-nested.xml` | 170 KB | deeply nested `library`/`book` records |
| `large-dataset.xml` | 5.6 MB | `employees` list with nested projects |

Both come from [Sample-Files.com](https://sample-files.com) (see the comment
at the top of each file). The licence of these samples has not been
confirmed; replace them with generated or permissively licensed documents
before relying on them in a distributed package.
