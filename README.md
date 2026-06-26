# Alleycat

![Alleycat logo](assets/alleycat-logo.png)

Alleycat is the GPLv3 desktop daemon used by NeCode Mobile. It runs on the user's computer, exposes local coding agents over an iroh connection, and lets the phone app pair by scanning a QR code.

For NeCode, the main path is:

```text
NeCode Mobile
  -> iroh relay
  -> Alleycat daemon on the user's computer
  -> local necode CLI
```

Project files, command execution, model configuration, and agent state stay on the user's computer. The relay only provides connectivity.

See [OPEN_SOURCE.md](OPEN_SOURCE.md) for the license and distribution boundary.

## Install

NeCode users normally access this daemon through `necode-cli`:

```bash
necode mobile serve
necode mobile status
necode mobile qr
```

For source builds from this repository:

```bash
cargo install --path crates/alleycat
alleycat serve
```

## First Run

```bash
alleycat install      # autostart at login, no admin required
alleycat status       # node id, token fingerprint, relay, agent availability
alleycat pair --qr    # QR code for the phone app
```

The `install` command registers a launchd user agent on macOS, a systemd `--user` unit on Linux, or a Startup-folder shortcut on Windows.

## Commands

| Command | What it does |
|---|---|
| `alleycat serve` | Run the daemon in the foreground. |
| `alleycat install` / `uninstall` | Per-user autostart, idempotent. |
| `alleycat status [--json]` | Print daemon status and configured agent availability. |
| `alleycat pair [--qr]` | Print the stable pair payload, optionally with an ASCII QR code. |
| `alleycat rotate` | Mint a fresh token. Node id is preserved. |
| `alleycat reload` | Re-read `host.toml` and swap agent config without restarting. |
| `alleycat agents list` | List configured agents and their availability. |
| `alleycat logs [-f]` | Tail daemon logs. |
| `alleycat stop` | Gracefully stop the daemon. |
| `alleycat probe --agent necode --method thread/list` | Connect like a mobile client and smoke-test the NeCode agent. |

## NeCode Configuration

`host.toml` is created on first run. A NeCode-focused config should enable the `necode` agent and point at the relay used by the mobile app:

```toml
token = "..."
relay = "https://relay.inoteexpress.com"

[agents.necode]
enabled = true
bin = "necode"
```

If `necode` is not on `PATH`, set `bin` to an absolute path.

## Pair Payload

`alleycat pair` prints:

```json
{
  "v": 1,
  "node_id": "<iroh public key>",
  "token": "<32-byte hex>",
  "host_name": "<computer name>",
  "relay": "https://relay.inoteexpress.com"
}
```

The token authenticates the first JSON frame on every stream. Treat the JSON and QR code as private connection credentials.

## Supported Agents

The daemon can multiplex multiple local coding agents, but NeCode Mobile only needs:

| Agent | Install |
|---|---|
| `necode` | Install `@aegean-org/necode-cli` or the NeCode binary, then run `necode` once to complete login/model setup. |

Other inherited bridges remain available for development: Codex, Pi, Amp, OpenCode, Claude, Factory Droid, Hermes, Grok, Devin, and shell.

## Building From Source

```bash
cargo build --release -p alleycat
target/release/alleycat status
target/release/alleycat serve
```

The workspace crates are:

- `crates/alleycat` - daemon binary, iroh endpoint, persistent identity, agent dispatcher, and OS-native control socket.
- `crates/bridge-core` - shared JSON-RPC framing, server scaffolding, and notification plumbing.
- `crates/codex-proto` - shared codex app-server wire shapes used by bridge crates.
- `crates/*-bridge` - adapters for individual local coding agents.

## Releases

This repository is the canonical NeCode daemon source. Release artifacts should be attached to `aegean-org/alleycat` and consumed by `necode mobile setup` or equivalent installer logic.

The mobile app source lives in [`aegean-org/aegean-org-necode-mobile`](https://github.com/aegean-org/aegean-org-necode-mobile).
