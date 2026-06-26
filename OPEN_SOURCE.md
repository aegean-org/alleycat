# Open Source Policy

Alleycat is the GPLv3 desktop daemon used by NeCode Mobile. It is published as a separate open-source component so the separately licensed NeCode CLI does not need to vendor GPL code.

## License

This repository is licensed under the GNU General Public License version 3 only (`GPL-3.0-only`). See [LICENSE](LICENSE).

Any distributed binary built from this repository must provide the corresponding source code under the same GPLv3 terms.

## Role In NeCode

```text
NeCode CLI
  -> invokes/downloads this daemon
  -> daemon connects through iroh relay
  -> NeCode Mobile connects by QR pairing
```

The daemon owns pairing, relay connectivity, local agent discovery, and forwarding requests to the local `necode` command. It does not belong inside the `necode-cli` package.

## Upstream

This repository is derived from the Alleycat/Litter ecosystem and keeps the GPLv3 licensing boundary. NeCode-specific changes are maintained under `aegean-org/alleycat`.

## Distribution

Release artifacts should be published from this repository under NeCode mobile daemon names, for example:

- `necode-mobile-daemon-windows-x64.exe`
- `necode-mobile-daemon-macos-arm64`
- `necode-mobile-daemon-linux-x64`

The NeCode CLI may download or execute those artifacts, but should not copy the daemon source or binary into a differently licensed package.
