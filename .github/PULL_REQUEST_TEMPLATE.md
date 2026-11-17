## Summary

<!-- Observable behavior, not a file list. Do not include hostnames, home
paths, absolute worktree paths, or other private environment details. -->

## Tests

- [ ] `cargo fmt --check`
- [ ] `cargo test --workspace --locked`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `bash scripts/check-workflows.sh` and `python3 -m unittest discover -s scripts/tests -t .` (when workflows or the `v*` path changed)

## Impact

<!-- Log format, public API, ACL, restart path. Write "None" if unchanged. -->
