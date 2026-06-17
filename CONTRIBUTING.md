# Contributing

Contributions are welcome — open an issue or a PR. Bug fixes, new chain presets,
docs improvements, and ideas are all appreciated.

## Quick start

```sh
cargo build --all-features
cargo test --all-features
```

Keep these three green before opening a PR (CI runs the same and treats warnings
as errors):

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

That's the whole bar. Small, focused PRs are easiest to review, and updating the
README when you change the public API is appreciated. Be kind.

By contributing, you agree your work is licensed under the [MIT License](./LICENSE).
