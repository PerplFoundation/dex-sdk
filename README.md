# perpl-sdk
[Perpl](https://perpl.xyz) DEX SDK

## Prerequisites

* Rust >= 1.85.0
* `anvil` binary from [Foundry](https://github.com/category-labs/foundry/releases/tag/v1.5.0-monad.0.2.0) Note: this is a custom anvil binary for Monad specifically.

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for the full guide. Note that **all pull requests
must target the `dev` branch — PRs directly into `main` are not accepted** (CI enforces this).

To contribute, the following make commands are provided.

```
make fmt
make lint
make test
# we provide a convenience wrapper to run all of the checks.
make all
```


## Documentation

```
cargo doc -p perpl-sdk --no-deps --open
```

## Crates

* [perpl-sdk](./crates/sdk/README.md): SDK types for building and maintaining in-memory cache of the exchange state, along with order posting helpers.
* [perpl-cli](./crates/cli/README.md): CLI for reading and tracing exchange state and events, and for placing orders.

## Usage

See [PerplFoundation/dex-sdk-examples](https://github.com/PerplFoundation/dex-sdk-examples) for some usage examples.