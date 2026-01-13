# perpl-sdk
[Perpl](https://perpl.xyz) DEX SDK

## Prerequisites

* Rust >= 1.85.0
* `anvil` binary from [Foundry](https://getfoundry.sh/) for local testing


## Documentation

```
cargo doc -p perpl-sdk --no-deps --open
```

## Crates

* [perpl-sdk](./crates/sdk/src/lib.rs): SDK types for building and maintaining in-memory cache of the exchange state, along with order posting helpers.
* [perpl-cli](./crates/cli/README.md): CLI for reading and tracing exchange state and events.

## Usage

See [PerplFoundation/dex-sdk-examples](https://github.com/PerplFoundation/dex-sdk-examples) for some usage examples.