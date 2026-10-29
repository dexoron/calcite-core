# ActualKlasterKraft

Minecraft Java Edition server implementation in C++.

Calcite is migrating this C++ implementation to an idiomatic Rust 2024
workspace; the C++ tree remains the behavioral oracle on branch
`perepisyvanie-na-rust`.

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

Implemented / Реализовано: Cargo workspace; protocol 775 (`26.1.2`) handshake,
VarInt framing, status ping/pong with partial reads; initial chunk indexing and
player-pool invariants.

Incomplete / Не реализовано: full Login/Configuration/Play, compression,
encryption, live-client compatibility, persistent worlds, complete chunk data,
and synchronized gameplay. The C++ tree remains the compatibility reference.

Checks / Проверки: `cargo test --workspace`; `cargo fmt --all -- --check`;
`cargo clippy --workspace --all-targets -- -D warnings`; run with
`cargo run -p calcite-server`.
