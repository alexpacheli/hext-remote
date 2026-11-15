# Status

**English** | [Português](STATUS.pt-BR.md)

Where Hext Remote stands: what is done, what is being checked and what comes next. Updated with each release.
**Last update: November 2026, version 0.9.2.**

## Done

| Phase | What it delivered | Plan |
|---|---|---|
| F0–F2 | Foundation, LAN video on Windows and macOS, GPU encoding and decoding | [original plan](planos/plano-original.md#11-roadmap) |
| F3 — Internet | 9-digit ID, signaling server, direct QUIC through `iroh`, own relay, consent prompt | [F3](planos/F3.md) |
| F4 — Support essentials | Windows service (sign-in screen, UAC, `Ctrl+Alt+Del`), access password, clipboard, files, chat, several monitors, restart and reconnect, bound technicians, several computers at once | [F4](planos/F4.md) |
| F5 — Accounts | Accounts with 2FA, signed device list, entry without consent for the owner's devices, sharing, access log | [F5](planos/F5.md) |
| F6 — Distribution | `.msi`, portable `.exe`, notarized `.dmg`, `.deb`/`.rpm`/`.AppImage`, signed automatic updates, unattended access on the Mac after login | [F6](planos/F6.md) |
| Linux interface | The app as a technician on Linux: account, My computers, opening other computers; `.deb` and `.rpm` with the libraries they need (WebKitGTK) (0.8.0) | [ARCHITECTURE](../ARCHITECTURE.md#7-platforms) |
| Virtual monitor | Session on a computer with no monitor (Windows driver as an installer option, macOS built in), resolution with `Ctrl+Alt+0`; a screen that stays black (a display turned off at its button) moves to the virtual one by itself; the viewer goes back to the last monitor chosen for each computer (0.8.2) | [monitor-virtual](monitor-virtual.md) |
| Privacy screen (Windows and Mac) | `Ctrl+Alt+P` or (since 0.9.1) the button on the viewer's bar: the person at the computer sees a black screen with a notice and their mouse and keyboard do nothing, while the technician sees and controls everything; `Ctrl+Alt+Del` there ends it (on a Mac, macOS 12.3 or newer: Esc three times) (0.9.0) | [F7](planos/F7.md#marco-1-como-funciona-e-roteiro), [Mac](planos/F7.md#marco-2-como-funciona-e-roteiro) |
| Sound | What the host plays (Windows, and macOS 13+) reaches the viewer, about 70 ms behind on a local network; `Ctrl+Alt+S` mutes it, and nothing is sent while the computer is silent (0.9.0). Since 0.9.1: sessions start without sound and the technician turns it on with `Ctrl+Alt+S` or the speaker button on the bar (while off, the host records and sends nothing); through the Windows service, the sound no longer breaks up while a video plays (0.9.1). In 0.9.2: a glitch of the sound device no longer reopens the Windows capture (the clicks a Mac heard from a Windows host), and the Windows agent stays out of Windows 11's efficiency mode | [F7](planos/F7.md#marco-3-como-funciona-e-roteiro) |
| Linux host (X11) | Linux is accessed too, on an X11 session: capture (XShm, several monitors through XRandR), mouse and keyboard (XTest, ABNT2 by scancode), clipboard and files; VA-API encoding where there is one, OpenH264 otherwise; `service install` (no `sudo`) opens it at login through a systemd user unit shipped in the `.deb`/`.rpm`. No access before login, no virtual monitor, no privacy screen (0.9.0). After a package update, it opens again by itself as soon as it is idle (0.9.2) | [F7](planos/F7.md#marco-4-como-funciona-e-roteiro) |
| Linux host (Wayland) | The same on Wayland, through the desktop's portal: the desktop asks the person at the computer once what to share (a restore token skips it next time), frames by PipeWire, input through the RemoteDesktop portal. The sign-in screen (GDM) stays out (0.9.0) | [F7](planos/F7.md#marco-5-como-funciona-e-roteiro) |

Names in "My computers" follow the computer's current name (0.7.1). In the session window, the monitor buttons come first beside the handle, and `Ctrl+Alt+→`/`←` switch to the next and previous monitor (0.9.2).

## Being checked in the field

Each phase closes with a real-world checklist ([roteiros/](roteiros/), in Portuguese):

- [ ] **F3:** home ↔ 4G (CGNAT), home ↔ home, and a network with UDP fully blocked ([F3, section 9](planos/F3.md#9-pendências-testar-no-final)).
- [ ] **F4:** restart by the technician and automatic return, on Windows and on the Mac; then the whole support
      checklist again ([atendimento-campo](roteiros/atendimento-campo.md)).
- [ ] **F5:** the accounts checklist ([atendimento-campo-f5](roteiros/atendimento-campo-f5.md)).
- [ ] **F6:** install, update and uninstall on real machines ([instalacao-campo](roteiros/instalacao-campo.md)).
- [ ] **Virtual monitor:** resolution, a ghost monitor (one that lost its cable) and uninstall on real machines;
      the sign-in screen with only the virtual monitor ([monitor-virtual](monitor-virtual.md)). A display turned off at
      its button: checked (November 2026).
- [ ] **Linux:** install the `.deb` on Ubuntu and the `.rpm` on Fedora, open it from the menu, sign in and access
      a computer (video, keyboard, mouse, a file received).
- [x] **Session window:** each session draws on a thread of its own. On a Mac, a minimized window comes back
      without freezing; opening another computer no longer stalls the open windows
      ([janela-travada](estudos/janela-travada.md#roteiro-de-campo)). Checked on a Mac (November 2026).
- [ ] **Privacy screen:** the F7 milestone 1 checklist ([F7](planos/F7.md#marco-1-como-funciona-e-roteiro)) and,
      on a Mac, the milestone 2 one ([F7](planos/F7.md#marco-2-como-funciona-e-roteiro)).
- [ ] **Sound:** the F7 milestone 3 checklist ([F7](planos/F7.md#marco-3-como-funciona-e-roteiro)). In the field
      (November 2026), a Mac hearing a Windows host got clicks; the 0.9.2 fix is still to be checked (the agent log
      says how many capture glitches there were).
- [ ] **Linux host (X11):** the F7 milestone 4 checklist on Ubuntu LTS with GNOME on Xorg
      ([F7](planos/F7.md#marco-4-como-funciona-e-roteiro)). Checked so far only on a virtual X server in CI.
- [ ] **Linux host (Wayland):** the F7 milestone 5 checklist on Ubuntu LTS with GNOME (Wayland)
      ([F7](planos/F7.md#marco-5-como-funciona-e-roteiro)). In the field (November 2026), on Fedora and
      Ubuntu: the session works, and the desktop asks what to share only the first time. The session bar opened in the
      middle of the screen; the fix (the interface through XWayland, 0.9.1) worked, but the bar was
      about 200 px tall and did not fold; its right size (0.9.2) is still to be checked.
- [ ] **Linux repositories (apt and dnf):** the `.deb` and `.rpm` subscribe to our repositories and then update
      with the system; tested in CI with a throwaway key. The real key is missing: generate it, put it in the
      secrets of the `linux-repo` environment (`main` only), turn on GitHub Pages, run **Linux repositories** once by
      hand, and only then commit the public part, in that order
      ([repositorios-linux](repositorios-linux.md#o-que-você-precisa-fazer-uma-vez), in Portuguese). Then, on a real
      Ubuntu and Fedora, see a new version arrive through `apt upgrade`/`dnf upgrade` and the software store (on
      Fedora, whether GNOME Software/PackageKit asks to trust the key because of `repo_gpgcheck`; not checked yet).

## Open issues

- On some home networks without CGNAT, the connection still goes through the relay instead of direct. The app
  already tries to open a port on the router (UPnP, NAT-PMP and PCP, through `iroh`): UPnP still has to be turned
  on in the router and tested.
- On a Mac, after a restart Hext comes back only when someone signs in. Left as is on purpose
  ([DECISIONS](DECISIONS.md#mac-no-sign-in-screen)): with FileVault on, macOS asks for a password before it loads.
- The Linux `.AppImage` uses the system's WebKitGTK (`libwebkit2gtk-4.1`): where it is missing, it does not open.
- If the GitHub repository goes private, GitHub Pages (where the Linux repositories live) needs a paid plan.
  Before turning Pages off or changing `HEXT_REPO_URL`, a release built with the new URL must be published at the
  old one, and the old one kept serving for a while; otherwise each user has to delete
  `/etc/apt/sources.list.d/hext-remote.sources` or `/etc/yum.repos.d/hext-remote.repo` by hand
  ([repositorios-linux](repositorios-linux.md#mudar-de-endereço-ou-tornar-o-repositório-privado), in Portuguese).

## Next

- **Linux repositories key** ([repositorios-linux](repositorios-linux.md), in Portuguese): until
  `installer/linux/repo-key.asc` exists, the packages ship without the subscription and Linux keeps updating
  through the app's notice.
- **F7 — Extras** ([plan](planos/F7.md), in Portuguese): the field checklist of each milestone;
  Wake-on-LAN, session recording, HEVC/AV1, several technicians, mobile and web when there is demand.
- **Smoothness, last and with care:** compare with Parsec side by side, on the same network without Tailscale, and
  apply what the study found ([fluidez-parsec](estudos/fluidez-parsec.md)). Already in: the capture keeps step with
  the screen, and the log shows the longest gap between frames on both sides.
