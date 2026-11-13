# Status

**English** | [Português](STATUS.pt-BR.md)

Where Hext Remote stands: what is done, what is being checked and what comes next. Updated with each release.
**Last update: November 2026, version 0.8.1.**

## Done

| Phase | What it delivered | Plan |
|---|---|---|
| F0–F2 | Foundation, LAN video on Windows and macOS, GPU encoding and decoding | [original plan](planos/plano-original.md#11-roadmap) |
| F3 — Internet | 9-digit ID, signaling server, direct QUIC through `iroh`, own relay, consent prompt | [F3](planos/F3.md) |
| F4 — Support essentials | Windows service (sign-in screen, UAC, `Ctrl+Alt+Del`), access password, clipboard, files, chat, several monitors, restart and reconnect, bound technicians, several computers at once | [F4](planos/F4.md) |
| F5 — Accounts | Accounts with 2FA, signed device list, entry without consent for the owner's devices, sharing, access log | [F5](planos/F5.md) |
| F6 — Distribution | `.msi`, portable `.exe`, notarized `.dmg`, `.deb`/`.rpm`/`.AppImage`, signed automatic updates, unattended access on the Mac after login | [F6](planos/F6.md) |
| Linux interface | The app as a technician on Linux: account, My computers, opening other computers; `.deb` and `.rpm` with the libraries they need (WebKitGTK) (0.8.0) | [ARCHITECTURE](../ARCHITECTURE.md#7-platforms) |
| Virtual monitor | Session on a computer with no monitor (Windows driver as an installer option, macOS built in), resolution with `Ctrl+Alt+0`; a screen that stays black (a display turned off at its button) moves to the virtual one by itself; the viewer goes back to the last monitor chosen for each computer | [monitor-virtual](monitor-virtual.md) |

Names in "My computers" follow the computer's current name (0.7.1).

## Being checked in the field

Each phase closes with a real-world checklist ([roteiros/](roteiros/), in Portuguese):

- [ ] **F3:** home ↔ 4G (CGNAT), home ↔ home, and a network with UDP fully blocked ([F3, section 9](planos/F3.md#9-pendências-testar-no-final)).
- [ ] **F4:** restart by the technician and automatic return, on Windows and on the Mac; then the whole support
      checklist again ([atendimento-campo](roteiros/atendimento-campo.md)).
- [ ] **F5:** the accounts checklist ([atendimento-campo-f5](roteiros/atendimento-campo-f5.md)).
- [ ] **F6:** install, update and uninstall on real machines ([instalacao-campo](roteiros/instalacao-campo.md)).
- [ ] **Virtual monitor:** resolution, a ghost monitor (one that lost its cable) and uninstall on real machines;
      the sign-in screen with only the virtual monitor; a display turned off at its button moving to the virtual one by
      itself ([monitor-virtual](monitor-virtual.md)).
- [ ] **Linux:** install the `.deb` on Ubuntu and the `.rpm` on Fedora, open it from the menu, sign in and access
      a computer (video, keyboard, mouse, a file received).

## Open issues

- On some home networks without CGNAT, the connection still goes through the relay instead of direct. The app
  already tries to open a port on the router (UPnP, NAT-PMP and PCP, through `iroh`): UPnP still has to be turned
  on in the router and tested.
- The Linux `.AppImage` uses the system's WebKitGTK (`libwebkit2gtk-4.1`): where it is missing, it does not open.

## Next

- **Mac sign-in screen:** a system LaunchDaemon plus per-session agents, so a Mac is reachable after a restart,
  before anyone logs in ([F6, section 3.1](planos/F6.md#31-marco-3-o-que-ficou-pronto-e-o-que-falta)).
- **Smoothness:** compare with Parsec side by side and apply what the study found
  ([fluidez-parsec](estudos/fluidez-parsec.md)).
- **F7 — Extras:** Linux host (being accessed), audio, privacy screen, session recording, HEVC/AV1, Wake-on-LAN,
  mobile and web.
