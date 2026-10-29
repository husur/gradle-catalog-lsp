# Contributing

Bug reports, ideas and pull requests are welcome. For anything bigger than a fix, open an issue first
so we can agree on the approach. The extension sticks to Gradle version catalogs: for `pom.xml` and
other ecosystems, [Depsy](https://github.com/mpiton/zed-depsy) is the place.

## Layout

- `server/` is the language server: a standalone binary that talks LSP over stdio. Its
  `src/lib.rs` holds the catalog parser and the version logic, and `src/main.rs` the LSP loop.
- `src/lib.rs` at the root is the Zed extension, compiled to WebAssembly by Zed. It only tells Zed
  to start `gradle-catalog-lsp` on TOML files, and prefers a binary found in your `PATH` over the
  downloaded one.

## Building and testing

Install Rust with [rustup](https://rustup.rs), then:

```sh
cargo test --workspace          # all the tests
cargo install --path server     # puts gradle-catalog-lsp in ~/.cargo/bin, which must be in your PATH
```

Keep `--workspace`: a plain `cargo test` at the root only tests the Zed extension, which has no tests.

The tests live in `server/tests`. Those in `lsp.rs` start the server binary and talk LSP to it, with
a local HTTP server standing in for the Maven repositories (through
`GRADLE_CATALOG_LSP_REPOSITORIES`), so they never reach the network.

The CI runs `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and the
tests through [cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov), which fails under 90% line
coverage of the server. To measure it locally:
`cargo llvm-cov --workspace --exclude-from-report gradle-catalog-zed`.

To try your changes in Zed, run `zed: install dev extension` and pick this folder. After each change
to the server, reinstall it and run `editor: restart language server`.

## Pull requests

- Everything written in the repository is in English: code, comments, docs, commits, pull requests
  and issues.
- Commits follow [Conventional Commits](https://www.conventionalcommits.org):
  `type(scope): description` in the imperative mood, with a body that explains why. Keep one logical
  change per commit: pull requests are merged with a merge commit, so every commit stays in the
  history.
- The pull request title follows the same format. Its type picks the label that files the change in
  the release notes and sets the next version (`feat` bumps the minor, a `!` after the type the
  major). The branch prefix adds a label too, so name the branch after the same type: `fix/…` for a
  `fix:` title.
- Answer review comments with new commits rather than rewriting the history.

## Releasing

For maintainers.

1. Merge the pull requests. Release Drafter keeps a draft release up to date, with the next version
   resolved from their labels: `breaking` bumps the major, `feat` the minor, anything else the patch.
2. If the extension (`src/` or `extension.toml`) changed since the commit the Zed extension
   registry points to, set `version` in `extension.toml` to the version of the draft, in a pull
   request merged before publishing.
3. Publish the draft. The release workflow builds the server for the five platforms and attaches
   the binaries to the release: check they are all there. Users get the new server the next time
   Zed starts, without an update of the extension.
4. If the extension changed, publish it in the
   [Zed extension registry](https://github.com/zed-industries/extensions), as described in
   [Updating an Extension](https://zed.dev/docs/extensions/publishing/updating-and-maintenance):
   in a fork, move the `extensions/gradle-catalog-lsp` submodule to the release commit, set the same
   version in `extensions.toml`, and open a pull request. Write its description yourself: the
   registry does not accept contributions from AI agents (see its
   [AI policy](https://github.com/zed-industries/extensions/blob/main/AI_POLICY.md)).
