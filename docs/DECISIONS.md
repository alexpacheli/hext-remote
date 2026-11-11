# Decisions

**English** | [Português](DECISIONS.pt-BR.md)

The choices that shape Hext Remote, and why. The full discussion of each is in the phase plans
([planos/](planos/), in Portuguese). New decisions go at the end of their section.

## Product

**Consent and transparency, never a hidden mode.** Every session shows a bar on the host with who is connected
and a button to end it, and each access is logged on the computer. Unattended access is set up once, by the
owner. There is no way to hide a session, skip consent or install silently without the owner's say.
*Why:* this is a support tool, not surveillance. Hiding features would be stalkerware, illegal in many places,
and flagged by antivirus vendors. ([original plan, 7.4](planos/plano-original.md#74-consentimento-e-transparência-princípio-do-produto))

**Free software under AGPL-3.0-only** (November 2026, version 0.6.1). *Why:* anyone may read, run and change
it, and whoever offers a modified server over a network must publish that version too.

**Windows installers are not code-signed** (November 2026). SmartScreen warns on first open. *Why:* since 2023 a
new code-signing certificate only comes on a hardware token or a cloud HSM, and its yearly cost is not justified
yet. The macOS app is signed and notarized (Apple Developer ID), because Gatekeeper blocks unsigned apps.

**Linux as a full app later (F7).** Today the Linux build is a command-line client. *Why:* on Wayland each
screen capture opens a permission prompt, which defeats unattended access; Windows and macOS cover the support
cases first.

## Network and server

**`iroh` instead of our own NAT traversal** (F3). Connections are QUIC through [`iroh`](https://iroh.computer):
direct when possible, through our own `iroh-relay` otherwise. *Why:* a spike between two real networks showed
direct connections and a usable relay, while hole punching, a relay and a TCP fallback of our own would take
much longer. No third-party service is used to find the computers. ([F3, section 2](planos/F3.md#2-a-decisão-principal-travessia-de-nat-própria-ou-iroh))

**SQLite, not PostgreSQL** (F3). *Why:* one VM runs everything; a file database is enough for the IDs and
accounts and is simpler to back up and to self-host.

**The host decides who gets in, never the server** (F5). The account is a list of devices in which each entry is
signed by a device already on it; the host keeps the list and checks it. *Why:* a compromised server or a leaked
database opens no computer. It costs more work than trusting the server.
([F5, section 2.3](planos/F5.md#23-quem-decide-é-o-host-não-o-servidor))

**Accounts: e-mail, password and TOTP, managed in the app only** (F5). *Why:* the usual sign-in, with a second
factor, and no web panel to secure. Passkeys may come later.

## App

**Rust, one native executable.** No runtime, .NET or Java. *Why:* small downloads for a quick support session,
low latency, and the same code on the three systems.

**Tauri for the interface, a native window for the session** (F4). *Why:* the interface is quick to build in
HTML; the video needs `wgpu` without a webview in the way, and a stuck session window must not take the
interface down.

**The interface never runs as SYSTEM** (F4). On Windows the service starts it as the signed-in user and talks
to it over a pipe only the two can open. *Why:* a webview with system rights is a privilege escalation path.

**The machine's ID when installed** (F4). The installed service uses one ID for the computer, kept across
reinstalls; the portable app uses the user's. *Why:* a technician's list must not change when another person
signs in to the computer.

**No administrator on the Mac** (F6). The Mac app keeps the access password and bound devices in the user's
folder. *Why:* whoever already runs as that user could control the screen anyway; asking for sudo would protect
nothing more. On Windows the service is the whole computer's, so it asks for an administrator.

**H.264 only, for now.** *Why:* every graphics card decodes it. HEVC and AV1 can come later, negotiated by what
both sides support.

**Updates signed with our own key** (F6). Each release has a version list signed with an Ed25519 key kept as a
repository secret, and the app accepts only that signature. Releases start as drafts that a person publishes.
*Why:* GitHub only serves the files; replacing one does not get past the check. It works the same whether or not
the installers are code-signed.

**Virtual monitor through an open-source driver, only when asked for** (0.7.0). Windows uses the MIT-licensed
MttVDD driver, an unchecked option in the installer (or `service virtual-display install`). Its files ship
inside the `.msi`, pinned by SHA-256, and the driver's certificate is matched by thumbprint. macOS uses
`CGVirtualDisplay`, a private API loaded at run time. The monitor exists only during the session. *Why:* support
for computers with no monitor without an HDMI dummy plug, without trusting a new certificate unless the owner
chooses, and without a ghost monitor left behind. ([monitor-virtual.md](monitor-virtual.md))

## Process

**Study other tools, never copy their code.** *Why:* RustDesk (AGPL) and Sunshine/Moonlight (GPL) are good
references, but copied code would bring their licenses and history along.

**Languages.** Code comments are in English. The interface is in Portuguese for now. The README, the
architecture, the self-hosting guide and this file are in English and Portuguese; the phase plans, studies and
test checklists are in Portuguese.
