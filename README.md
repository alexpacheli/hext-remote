# Hext Remote

**English** | [Português](README.pt-BR.md)

Remote access and support for Windows, macOS and Linux, focused on smoothness. Free software
([AGPL-3.0](LICENSE)), written in Rust.

- **Connect by ID:** each computer has a 9-digit ID. The technician types the ID, and the person on the other
  side accepts or declines.
- **Direct when possible:** the two computers connect directly, even on different networks. When they cannot
  (a corporate firewall, for example), the session goes through a relay, always end-to-end encrypted.
- **Video on the graphics card:** capture, H.264 encoding and display on the GPU, falling back to the CPU
  automatically.
- **Unattended access:** with the service installed, the computer is online from boot. The technician gets in
  with a password the owner set or, later, without a password, by their key or by the account. On Windows, this
  also works on the sign-in screen and UAC prompts.
- **In the session:** multiple monitors, clipboard, files, chat, remote restart and up to 8 sessions at once.
- **Accounts (optional):** a synced list of computers, sharing with other accounts and an access log.
- **Transparent:** every session is shown on screen while it lasts, and each access is logged on the computer.

## How it works

```
                    ┌──────────────────────────────┐
         ┌─────────►│  Signaling server            │◄─────────┐
         │          │  (hext-server): IDs,         │          │
         │          │  presence and requests       │          │
         │          └──────────────────────────────┘          │
         │                                                    │
  ┌──────┴───────┐     direct session (P2P, QUIC)      ┌──────┴───────┐
  │  Technician  │◄───────────────────────────────────►│   Customer   │
  │ Hext Remote  │                                     │ Hext Remote  │
  └──────┬───────┘                                     └──────┬───────┘
         │          ┌──────────────────────────────┐          │
         └─────────►│  Relay (iroh-relay): only    │◄─────────┘
                    │  without a direct path; it   │
                    │  cannot see the content      │
                    └──────────────────────────────┘
```

1. When it opens, the app registers with the **signaling server** using the computer's key (Ed25519, created on
   first run) and gets its ID.
2. The technician asks to connect to an ID. The server passes the request on, and the other side accepts, or
   checks the technician's password or key.
3. The two open a **QUIC** connection to each other, through NAT. If they cannot, they use the **relay**.
4. The session is end-to-end encrypted between the two computers: neither the server nor the relay sees the
   screen, the keyboard or the files. The access password never travels (SPAKE2).

Details in [ARCHITECTURE.md](ARCHITECTURE.md) and in each phase's plan ([F3](docs/F3.md) to [F6](docs/F6.md)),
in Portuguese.

## Download and use

The installers are in [Releases](https://github.com/alexpacheli/hext-remote/releases):

| System | File | Notes |
|---|---|---|
| Windows | `HextRemote-<version>-x64.msi` | Installs the app and the service (unattended access, sign-in screen). For many computers: `msiexec /i HextRemote-<version>-x64.msi /qn`. |
| Windows | `HextRemote-<version>-portable.exe` | For a quick support session, without installing. |
| macOS | `HextRemote-<version>.dmg` | Drag Hext Remote to Applications. |
| Linux | `.deb`, `.rpm` or `.AppImage` | Command line for now (`hext-remote`). |

Free code signing on Windows provided by [SignPath.io](https://about.signpath.io/), certificate by
[SignPath Foundation](https://signpath.org/). See the [code signing policy](#code-signing-policy) and the
[privacy policy](#privacy-policy).

**First session:**

1. On the customer's computer, open Hext Remote. The home screen shows its **ID**.
2. On the technician's computer, open Hext Remote, type that ID and click **Conectar** (Connect).
3. The customer sees the request with the technician's name and permissions. Once accepted, the session starts.

To get in without anyone accepting, install the service (the `.msi` does) and set the access password in the
**Segurança** (Security) tab. The full walkthrough of each feature, including the command line, is in
[docs/testes.md](docs/testes.md), and the field support checklists in
[docs/atendimento-campo.md](docs/atendimento-campo.md) (in Portuguese). The interface is in Portuguese for now.

**Updates.** Installed on Windows (the service) or on a Mac (in Applications), Hext Remote updates itself
between sessions. Otherwise, the home screen says when there is a new version (on Linux:
`hext-remote update check`). Each version is signed by the release pipeline, and the app refuses any other.

## Your own server

The official build uses our server (`signal.hext.com.br` and `relay.hext.com.br`). Whoever supports many
computers, wants the data on their own infrastructure or distributes a modified build runs their own server and
builds the app pointing to it. The server and the relay come from this repository and the
[iroh](https://github.com/n0-computer/iroh) project, with no paid service.

**Step-by-step guide: [docs/self-hosting.md](docs/self-hosting.md)** (server, your own app build, update key,
backups and security). The summary follows.

**What it takes:**

- A Linux server with a public IP. To start, 1 to 2 vCPUs and 1 to 2 GB of RAM are enough (Oracle Cloud has a
  free tier with plenty of traffic). Prefer a server close to the users, because of the relay.
- A domain with two names pointing to the server's IP: `signal.<domain>` and `relay.<domain>`. On Cloudflare,
  leave both as **DNS only** (grey cloud).
- Ports **80 and 443 TCP** and **7842 UDP** open in the provider's firewall.

**What it uses.** Signaling is light: it only keeps IDs and presence and passes connection requests on, and a
small server handles thousands of connected computers. What costs is the **relay**: a session through it uses
0.5 to 3 Mbps (about 0.2 to 1.3 GB per hour). Most sessions are direct and do not go through it. Each device
sends at most 5 Mbit/s through the relay, and the app adjusts the quality to that. To follow the month's
traffic and the relay's use: `./deploy/uso.sh`, on the server.

### Option 1: Docker

With [Docker](https://docs.docker.com/engine/install/) 26 or newer on the server (the relay mounts only its own
certificate's folder, with `subpath`):

```sh
git clone https://github.com/alexpacheli/hext-remote.git
cd hext-remote/deploy/docker
cp .env.example .env
nano .env        # HEXT_DOMAIN and HEXT_RELAY_TOKEN (generate with: openssl rand -hex 32)
docker compose up -d --build
```

This starts three containers: `hext-server`, `iroh-relay` and Caddy, which gets the Let's Encrypt certificates
by itself. The first time, the build takes a few minutes, and the relay restarts a few times until Caddy gets
the certificate (under a minute). To check:

```sh
curl https://signal.<domain>/health        # ok
docker compose logs -f
```

To update: `git pull && docker compose up -d --build`. The database lives in the `hext_server` volume: back it
up. Without it, the computers get new IDs.

### Option 2: directly on the system (systemd)

On Debian or Ubuntu, with [Rust](https://rustup.rs) installed:

```sh
git clone https://github.com/alexpacheli/hext-remote.git
cd hext-remote
HEXT_DOMAIN=<domain> ./deploy/install.sh
```

The script installs Caddy, builds `hext-server` and `iroh-relay`, generates the relay's secret, creates the
systemd services, opens the ports in iptables and tests both addresses. To update: `git pull && ./deploy/install.sh`
(the domain is kept). Details, logs and backups in [docs/deploy.md](docs/deploy.md) (in Portuguese).

### Account e-mails (optional)

For accounts to get their verification codes, set an SMTP server: on Docker, the `HEXT_SMTP_*` lines of `.env`;
on systemd, the file `/etc/hext/mail.env` ([docs/hext-server.md](docs/hext-server.md)). Without it, no e-mail is
sent (for development, `--dev-mail-log` writes the codes to the server's log).

### The app for your server

The server and relay addresses go into the app at build time, through the `HEXT_SERVER` and `HEXT_RELAY`
variables:

- **On GitHub (recommended):** fork this repository and, in **Settings → Secrets and variables → Actions →
  Variables**, create `HEXT_SERVER` = `wss://signal.<domain>` and `HEXT_RELAY` = `https://relay.<domain>`. Each
  `v*` tag builds the installers in your fork's Releases, and the app looks for updates there. To sign the
  updates and the installers, see section 4 of [docs/F6.md](docs/F6.md).
- **On your machine:**

  ```sh
  HEXT_SERVER=wss://signal.<domain> HEXT_RELAY=https://relay.<domain> cargo build --release -p hext
  ```

For a quick test, without building: `HextRemote host --server wss://signal.<domain> --relay https://relay.<domain>`
and the same for `view`. The computers of one server do not see those of another.

## Building

You need [Rust](https://rustup.rs). The version is pinned in `rust-toolchain.toml` and installed by itself. On
Windows, you also need the Visual Studio Build Tools with the "Desktop development with C++" workload.

```sh
cargo run -p hext              # the interface
cargo run -p hext -- --help    # the command line
cargo run -p hext-server       # the server, at http://127.0.0.1:8080 (test: --server ws://127.0.0.1:8080)
cargo test --workspace         # tests (including an end-to-end host → viewer one)
```

Build variables, all optional:

| Variable | What it sets | Without it |
|---|---|---|
| `HEXT_SERVER` | Signaling server | `wss://signal.hext.com.br` |
| `HEXT_RELAY` | Relay | `https://relay.hext.com.br`; with another `HEXT_SERVER`, none (direct connections only) |
| `HEXT_RELEASES` | The releases page, where the app looks for updates | this repository's Releases |
| `HEXT_UPDATE_KEY` | Public key that signs the updates | no automatic updates |

## Layout

| Folder | Contents |
|---|---|
| `apps/hext` | The app: interface (Tauri), `host`, `view` and `service` |
| `apps/hext-server` | Server: IDs, presence, signaling and accounts ([docs/hext-server.md](docs/hext-server.md)) |
| `crates/proto` | Protocol messages: handshake, video, input |
| `crates/crypto` | The computer's identity (Ed25519) and access password (SPAKE2) |
| `crates/signal` | Signaling: 9-digit ID, messages between the app and the server, WebSocket client |
| `crates/net` | Transport (`iroh`/QUIC): direct when possible, relay otherwise |
| `crates/capture` | Screen capture (DXGI on Windows, ScreenCaptureKit on macOS) |
| `crates/codec` | H.264: graphics card (Media Foundation, VideoToolbox) or OpenH264 in software |
| `crates/input` | Key map and mouse and keyboard injection (SendInput, CGEvent) |
| `crates/render` | GPU rendering (wgpu), YUV → RGB in the shader |
| `crates/session` | Host and viewer pipelines |
| `crates/metrics` | Latency statistics |
| `deploy` | Server: systemd script, Caddy, relay and Docker |
| `installer` | Installers (WiX on Windows, `.dmg` on macOS, Linux packages) |

## Contributing

Suggestions and bug reports are welcome in [Issues](https://github.com/alexpacheli/hext-remote/issues). To send
a change, open a pull request. CI runs on all three systems:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked
cargo test --workspace --locked
```

By contributing, you agree to license your contribution under the project's license.

## License

Hext Remote is free software, under the [GNU Affero General Public License v3.0](LICENSE) (`AGPL-3.0-only`).
Whoever distributes a modified program, or offers a modified server over a network, must publish that version's
source code under the same license.

## Code signing policy

Windows releases are to be signed through [SignPath.io](https://about.signpath.io/), with a certificate
from the [SignPath Foundation](https://signpath.org/) (free code signing for open-source projects). The macOS
app is signed with an Apple Developer ID and notarized.

- **What is signed:** only files built by this repository's GitHub Actions release workflow
  (`.github/workflows/release.yml`) from a version tag. Nothing built on a developer's machine is signed.
- **Team:** committer and approver: [Alex Pacheli](https://github.com/alexpacheli). Every release is
  approved by hand: the pipeline opens a draft, and only a maintainer publishes it.
- **Privacy:** see the [privacy policy](#privacy-policy) below.

## Privacy policy

This program sends data only where its features need it:

- the Hext signaling server: the device's ID, public key, name and version, to find the other computer;
- with an account (optional): the e-mail and the sign-in data, the account's signed list of devices and
  computers and, for a computer bound to the account, its access log (who connected and when), shown on
  the account's Activity screen. Verification codes are sent to that e-mail through an e-mail provider;
- the relay: the session itself, end-to-end encrypted, when a direct connection is not possible (the relay
  cannot see its content);
- GitHub: update checks and downloads.

The screen, the keyboard, the files and the clipboard of a session go only to the other computer, end-to-end
encrypted. Nothing is sold or shared with third parties, and there is no advertising or analytics. Accounts
not confirmed by e-mail are deleted within 24 hours, and an account can be deleted at any time in the app
(Conta → Excluir a conta).

Nobody connects to a computer without the user's acceptance on screen, an unattended password the computer's
owner set, or a device of an account the owner bound. Every session is shown on screen while it lasts, and
the installed app keeps a local log of them.
