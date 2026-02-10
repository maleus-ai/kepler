# kepler-protocol

Shared IPC protocol definitions for CLI-daemon communication.

## Library

This is a library crate used by both `kepler-daemon` and `kepler-cli`.

## Architecture

The protocol implements a **multiplexed, persistent** connection model over Unix domain sockets:

- **RequestEnvelope** -- Client messages with unique IDs
- **ServerMessage** -- Daemon responses and progress events, tagged with request IDs
- **Concurrent** -- Multiple requests in-flight on a single connection
- **Length-prefixed** -- 4-byte big-endian length + bincode-serialized payload

## Key Modules

| Module | Description |
|--------|-------------|
| `protocol.rs` | Message types: `RequestEnvelope`, `ServerMessage`, `Request`, `ServerPayload`, `ResponseData`, `ServicePhase` |
| `client.rs` | Client implementation: connection, multiplexed request/response, concurrent `&self` methods |
| `server.rs` | Server implementation: socket listener, peer credential verification, kepler group auth |
| `errors.rs` | `ClientError` and `ServerError` types |

## See Also

- [Protocol](../docs/protocol.md) -- Wire format and message reference
- [Security Model](../docs/security-model.md) -- Socket authentication
