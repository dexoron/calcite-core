# Calcite Core

High-performance Minecraft Java Edition server core written in Rust.
The Rust rewrite targets behavioral compatibility with the legacy oracle while
building a smaller, typed, testable implementation.

## Rust workspace

The repository uses Rust 2024, Cargo, and a workspace split by responsibility:

- `calcite-core` - shared positions, UUIDs, and domain errors.
- `calcite-protocol` - protocol 775 (`Minecraft 26.1.2`) primitives,
  handshake parsing, VarInt encoding, and packet framing.
- `calcite-net` - incremental TCP frame decoding and transport boundaries.
- `calcite-entity` - player state and bounded player-pool foundation.
- `calcite-world` - chunk storage, world-height bounds, and coordinate mapping.
- `calcite-server` - Tokio listener and connection lifecycle.

## Current status

Implemented:

- Status request, Server List Ping, and Pong handling.
- Partial TCP reads with bounded VarInt packet framing.
- Protocol-775 handshake validation and malformed-input errors.
- Offline-mode Login, Configuration fixture stream with strict Client
  Information parsing, and transition to Play.
- Minimal Play session: Keep Alive scheduling, reply correlation, timeout
  and Disconnect packets.
- Initial chunk indexing and player-pool capacity/slot reuse invariants.

Incomplete:

- Play gameplay packets (Login (play), position sync, chunks, chat, player
  list); a real client does not spawn yet.
- Compression, encryption, and verified live-client compatibility.
- Persistent worlds, complete chunk/light/block-entity serialization, and
  synchronized gameplay state.

## Build and checks

```powershell
cargo build --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p calcite-server
```

The active rewrite branch is `rewrite/rust-migration`.
