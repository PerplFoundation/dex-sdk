# Contributing to perpl-sdk

Thanks for your interest in contributing to the [Perpl](https://perpl.xyz) DEX SDK.

## Branching model

This repository uses a two-branch model:

| Branch | Purpose |
| ------ | ------- |
| `dev`  | Integration branch. **All contributions land here first.** |
| `main` | Release branch. Every merge publishes to crates.io and cuts a GitHub release. |

### Pull requests must target `dev`, never `main`

> **All pull requests must be opened against `dev`.**
> Pull requests opened directly against `main` will not be accepted.

The only pull request that may target `main` is the `dev` -> `main` release PR, which is
opened by a maintainer when a release is cut.

This is enforced in CI, not just by convention. The `Check PR branch` step in
[`.github/workflows/pull_request.yaml`](.github/workflows/pull_request.yaml) fails the
build for any pull request whose base is `main` and whose head is not `dev`:

```
Pull requests to main must come from dev branch.
```

If you opened a PR against `main` by mistake, edit the PR and change its base branch to
`dev` — there is no need to close it and open a new one.

### Workflow

1. Fork the repository (external contributors) or create a branch (maintainers).
2. Branch **from `dev`**, not from `main`:
   ```bash
   git fetch origin
   git switch -c feat/my-change origin/dev
   ```
3. Make your change, keeping commits focused.
4. Run the local checks (see below) until they pass.
5. Push and open a pull request **into `dev`**.
6. Once approved and CI is green, a maintainer merges it into `dev`.

Branch names follow a `<type>/<short-description>` convention, e.g.
`feat/adding-order-from-cli-functionality`, `fix/premium-pnl-settlement`,
`chore/pinning-versions`.

## Prerequisites

* Rust >= 1.85.0 (the pinned toolchain is in [`rust-toolchain.toml`](rust-toolchain.toml))
* The nightly toolchain used for formatting: `nightly-2026-08-22`
  ```bash
  rustup toolchain install nightly-2026-08-22 --component rustfmt,clippy
  ```
* The `anvil` binary from
  [Monad's Foundry fork](https://github.com/category-labs/foundry/releases/tag/v1.5.0-monad.0.2.0)
  — this is a custom `anvil` build for Monad specifically, and the test suite needs it on
  your `PATH`.

## Local checks

Run these before pushing; CI runs the same targets.

```bash
make check   # cargo check
make fmt     # cargo +nightly-2026-08-22 fmt
make lint    # cargo clippy
make build   # cargo build --all-features
make test    # cargo test

make all     # convenience wrapper for all of the above
```

CI additionally runs [`cargo-machete`](https://github.com/bnjbvr/cargo-machete) to catch
unused dependencies, and `git diff --exit-code` after formatting — so **commit the result
of `make fmt`**, or CI will fail on a dirty tree.

## Commit messages

Follow the conventional-commit style already used in the history:

```
feat(A-1957): inconsistent-funding-start-for-new-perps
fix(SDK): pep-perp fee schedule tracking
chore: relist sol
```

Use `feat`, `fix`, `chore`, `docs`, `refactor`, or `test`, with an optional scope — either
a crate (`sdk`, `cli`, `num`, `types::Trade`) or a ticket ID (`A-1957`).

Do not hand-write `Bump version to vX.Y.Z` commits — versioning is automated (below).

## Versioning and releases

Both of these are automated; contributors should not touch the workspace `version` in
[`Cargo.toml`](Cargo.toml).

* **On push to `dev`** — [`dev.yaml`](.github/workflows/dev.yaml) runs `make dev-version`,
  which bumps the patch version if `dev` is still on the same version as `main` and pushes
  a `Bump version to vX.Y.Z` commit. It is a no-op if `dev` has already been versioned
  since the last release.
* **On push to `main`** — [`main.yaml`](.github/workflows/main.yaml) runs `cargo publish`
  and then `make release`, which tags `vX.Y.Z` and creates the GitHub release.

Because a merge into `main` publishes a crate version immediately, `main` only ever
receives merges from `dev`.

## Project layout

* [`crates/sdk`](crates/sdk/src/lib.rs) — SDK types for building and maintaining the
  in-memory cache of exchange state, plus order-posting helpers.
* [`crates/cli`](crates/cli/README.md) — CLI for reading and tracing exchange state and
  events.
* `abi/` — contract ABIs consumed by the SDK.

Generate and browse the API docs with:

```bash
cargo doc -p perpl-sdk --no-deps --open
```

For usage examples, see
[PerplFoundation/dex-sdk-examples](https://github.com/PerplFoundation/dex-sdk-examples).

## License

By contributing, you agree that your contributions are licensed under the terms in
[LICENSE](LICENSE).
