## Summary

<!-- What changes and why. Link the issue it fixes. -->

## LemMinX / IntelliJ comparison

<!-- For a user-visible behaviour: what the reference implementations do, and where this differs. Remove when not relevant. -->

## Checklist

- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass
- [ ] Conformance baselines (`crates/xml-conformance/baselines`) only shrink, or the added lines are explained above
- [ ] User-visible change: README "Features" bullet and `CHANGELOG.md` entry under `Unreleased`
- [ ] New setting: `docs/configuration.md`
- [ ] New diagnostic code or `data.kind`/`data.rule`: added to `docs/diagnostics.md` (existing ones are never renamed)
- [ ] Stacked on another pull request: `Stacked on #N` in this description

## Tests

<!-- What you ran and what the new tests cover. -->
