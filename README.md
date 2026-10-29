# ActualKlasterKraft

Minecraft Java Edition server implementation in C++.

Calcite is migrating this C++ implementation to an idiomatic Rust 2024
workspace. The C++ tree remains the behavioral oracle while the Rust crates
are built up incrementally on branch `perepisyvanie-na-rust`.

## Architectural Choices

- Modern C++23 and Boost
- Heavy usage of coroutines (preventing callback hell and explicit FSMs)
- C++ modules, not headers/TUs
- Less heap allocations, more pools, arenas and stack

## Dependencies 

- GCC or Clang supporting C++23
- Boost
- CMake 3.30+
- OpenSSL

## Status

The server responds Server List Pings, i.e. shows information about self in the server list.

You're spawned in a world with only one chunk which looks like a stack of Air and Grass Block 16\*16\*16 cubes.

You're in Creative Mode, you can fly and break blocks. You can also cheat items into your inventory (since you're in Creative Mode) and place blocks.

You can see how other player move around and see their and your chat messages. Blocks and items are not synchronised.

## Rust migration / Миграция на Rust

### Implemented / Реализовано

- Cargo workspace crates for core types, protocol, networking, entities,
  world, and server layers.
- Minecraft Java protocol 775 (`26.1.2`) handshake validation and framed
  VarInt packet primitives.
- Status request/ping and pong handling, including partial TCP frame reads.
- Initial chunk storage with explicit global-to-local coordinate mapping and
  player pool capacity/slot reuse invariants.

### Not implemented / Не реализовано

- Full Login, Configuration, Play, compression, encryption, and live-client
  compatibility remain incomplete.
- Persistent world storage, complete chunk/light/block-entity serialization,
  and synchronized gameplay state are not yet available.
- The C++ implementation above remains the reference until each subsystem is
  ported and regression-tested.

### Build and test / Сборка и тесты

```powershell
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p calcite-server
```

The Rust server currently accepts a TCP listener and exercises the protocol
framing/status path; use the C++ build for the broader gameplay behavior until
those migration stages are complete.
