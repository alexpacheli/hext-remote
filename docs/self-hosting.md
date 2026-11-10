# Self-hosting

**English** | [Português](self-hosting.pt-BR.md)

The official Hext Remote build uses our server (`signal.hext.com.br` and `relay.hext.com.br`). This guide runs
your own, end to end: the server, an app built for it, and what it takes to keep both running.

You need three things:

- **the server:** signaling (`hext-server`), the relay (`iroh-relay`) and Caddy for TLS;
- **an app built for your server:** the official app only talks to the official server;
- **a few habits:** backups, updates and watching the relay's traffic.

## 1. Prepare

- **A Linux server with a public IP:** Debian 11+ or Ubuntu 22.04+ (systemd 247 or newer). To start, 1 to 2 vCPUs
  and 1 to 2 GB of RAM are enough (Oracle Cloud has a free tier with plenty of traffic). Prefer a server close
  to your users, because sessions without a direct path go through its relay.
- **A domain with two names** pointing to the server's IP: `signal.<domain>` and `relay.<domain>`. On Cloudflare,
  leave both as **DNS only** (grey cloud): Caddy must receive the connections itself to get the certificates.
- **Open ports** in the provider's firewall (e.g. Oracle Cloud's Security List):
  - **80/TCP and 443/TCP**: Caddy (HTTPS, and the Let's Encrypt challenge);
  - **7842/UDP**: the relay's address discovery, which helps direct connections.

  Nothing else needs to be open: `hext-server` and `iroh-relay` listen only on `127.0.0.1`.

## 2. Install the server

Pick one.

### Docker

With [Docker](https://docs.docker.com/engine/install/) 26 or newer:

```sh
git clone https://github.com/alexpacheli/hext-remote.git
cd hext-remote/deploy/docker
cp .env.example .env
nano .env        # HEXT_DOMAIN, and HEXT_RELAY_TOKEN (generate with: openssl rand -hex 32)
docker compose up -d --build
```

### systemd

With [Rust](https://rustup.rs) installed:

```sh
git clone https://github.com/alexpacheli/hext-remote.git
cd hext-remote
HEXT_DOMAIN=<domain> ./deploy/install.sh
```

The script installs Caddy, builds `hext-server`, installs `iroh-relay`, generates the relay's secret
(`/etc/hext/secrets.env`, mode 600), installs the systemd units, opens the ports in iptables and tests both
addresses. If your names do not follow `signal.` and `relay.`, pass them: `SIGNAL_HOST=… RELAY_HOST=…`.

### Check

```sh
curl https://signal.<domain>/health                                   # ok
curl -s -o /dev/null -w '%{http_code}\n' https://relay.<domain>/generate_204   # 204
```

The first time, the relay may restart a few times until Caddy gets its certificate (under a minute).

## 3. Account e-mails

Accounts are optional, but signing up needs a verification code sent by e-mail. **Without SMTP, no
e-mail is sent and nobody can create an account.** Any SMTP provider works (Brevo, Amazon SES, your own); the
connection is always encrypted (STARTTLS on 587, TLS on 465).

- **Docker:** uncomment and fill the `HEXT_SMTP_*` and `HEXT_MAIL_FROM` lines in `.env`, then
  `docker compose up -d`.
- **systemd:** create `/etc/hext/mail.env` empty with mode 600 first, then write it:

  ```sh
  sudo install -m 600 -o root -g root /dev/null /etc/hext/mail.env
  sudo nano /etc/hext/mail.env
  ```

  ```
  HEXT_SMTP_HOST=smtp.example.com
  HEXT_SMTP_PORT=587
  HEXT_SMTP_USER=<SMTP login>
  HEXT_SMTP_PASSWORD=<SMTP password>
  HEXT_MAIL_FROM=Hext Remote <no-reply@your-domain.com>
  ```

  Then `sudo systemctl restart hext-server`.

The server limits e-mails (5 per hour per IP and per address; 100 per hour and 250 per day in total, set with
`HEXT_MAIL_PER_HOUR` and `HEXT_MAIL_PER_DAY` to fit your provider's quota). Details in
[hext-server.md](hext-server.md) (in Portuguese). For development only, `hext-server --dev-mail-log` writes the
codes to the server's log instead of sending them.

## 4. Build the app for your server

The server and relay addresses are built into the app. **The official installers will not find your server.**

### On GitHub (recommended)

1. **Fork** this repository.
2. In your fork, **Settings → Secrets and variables → Actions → Variables**, create:
   - `HEXT_SERVER` = `wss://signal.<domain>`
   - `HEXT_RELAY` = `https://relay.<domain>`
3. **Update key** (without it, the apps never update themselves):
   1. With any Hext Remote build, run `update keygen` (see [the program's name](#the-programs-name)):
      e.g. `HextRemote.exe update keygen` on Windows.
   2. Save the private half as the **secret** `UPDATE_SIGNING_KEY`, and the public half as the **variable**
      `HEXT_UPDATE_KEY`.
   3. Keep the private half safe: whoever has it can push updates to every computer running your build.
4. Set the version in the root `Cargo.toml` (e.g. `0.6.4`) and push a matching tag (`v0.6.4`). The **Release**
   workflow builds the Windows, macOS and Linux installers and opens a **draft** release in your fork. Publish it
   by hand; the apps look for updates in your fork's releases.

**Signing the installers (optional).** Without certificates the installers come out unsigned: Windows SmartScreen
and macOS Gatekeeper warn on first open.

- **Windows:** the workflow signs with a `.pfx` file (secrets `WINDOWS_CERTIFICATE`, the file in base64, and
  `WINDOWS_CERTIFICATE_PASSWORD`). Since June 2023, certificate authorities only issue code-signing keys on a
  hardware token or a cloud HSM, so a new certificate does not come as a `.pfx`. The realistic routes today are
  [SignPath Foundation](https://signpath.org) (free for open-source projects) or Azure Trusted Signing, both of
  which need a change to the signing step in `.github/workflows/release.yml`.
- **macOS:** `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`,
  `APPLE_APP_PASSWORD` and `APPLE_TEAM_ID` (Apple Developer Program; details in section 4 of [F6.md](F6.md)).

The installed Mac app updates itself only when signed with a Developer ID.

### On your machine

```sh
HEXT_SERVER=wss://signal.<domain> HEXT_RELAY=https://relay.<domain> cargo build --release -p hext
```

A local build without `HEXT_UPDATE_KEY` does not update itself.

### Quick test, without building

With the official app, from the command line (`hext-remote` is the Linux name; see
[the program's name](#the-programs-name) on the other systems):

```sh
hext-remote host --server wss://signal.<domain> --relay https://relay.<domain>
hext-remote view <ID> --server wss://signal.<domain> --relay https://relay.<domain>
```

Computers on different servers do not see each other.

### The program's name

The same program has a different name on each system:

| System | Command |
|---|---|
| Windows | `HextRemote.exe` (in `C:\Program Files\Hext Remote` once installed) |
| macOS | `"/Applications/Hext Remote.app/Contents/MacOS/HextRemote"` |
| Linux (.deb, .rpm) | `hext-remote` |
| Linux (AppImage) | `./HextRemote-<version>-x86_64.AppImage` |
| Your own build | `target/release/hext` |

## 5. Operate

**Back up the database.** It holds the link between each computer's ID and its key, plus the accounts. If it is
lost, every computer gets a new ID and every account is gone. Copy it regularly:

- **systemd:** `/var/lib/hext/hext-server.db`;
- **Docker:** the `hext_server` volume.

**Update the server:**

- **systemd:** `cd hext-remote && git pull && ./deploy/install.sh` (the domain is kept in `/etc/hext/hosts.env`);
- **Docker:** `cd hext-remote/deploy/docker && git pull && docker compose up -d --build`.

Computers reconnect by themselves after the restart. Update the server before releasing an app that needs it.

**Watch the relay's traffic.** Signaling is light: a small server handles thousands of connected computers.
What costs is the relay:
- a session through it uses 0.5 to 3 Mbps (about 0.2 to 1.3 GB per hour);
- most sessions are direct and do not touch it;
- each device sends at most 5 Mbit/s through it (`[limits.client.rx]` in `deploy/iroh-relay.toml`), and the app
  lowers the video quality to fit.

`./deploy/uso.sh` (from the `hext-remote` folder) shows the month's traffic against your provider's monthly limit,
and what the relay has passed on. The limit defaults to 10 TB (Oracle's); set yours with
`HEXT_LIMITE_GB=1000 ./deploy/uso.sh`.

**Logs:**

```sh
journalctl -u hext-server -u iroh-relay -u caddy -f      # systemd
docker compose logs -f                                    # Docker
```

**Relay without the limit, for chosen accounts (systemd only).** A second relay without the 5 Mbit/s limit,
only for the accounts you list. See "Relay sem limite" in [deploy.md](deploy.md).

## 6. Security

- Only ports 80/443 TCP and 7842 UDP open (plus 7843 UDP if you set up the relay without the limit). The relay
  only accepts computers registered on your `hext-server`, which it asks with the secret in
  `secrets.env` (or `HEXT_RELAY_TOKEN`).
- Keep `/etc/hext/secrets.env`, `/etc/hext/mail.env` and `.env` private (mode 600), and out of git.
- Harden the VM itself:
  - SSH with keys only (`PasswordAuthentication no`) and `fail2ban`;
  - automatic security updates (`unattended-upgrades`);
  - nothing else listening on public ports.
- Sessions are end-to-end encrypted between the two computers: neither your server nor your relay sees the
  screen, the keyboard or the files.

## 7. License

Hext Remote is under the [AGPL-3.0](../LICENSE). Running it unmodified for your own support is fine. If you
**modify** the app or the server and offer it to others (including over a network), you must publish that
version's source code under the same license.
