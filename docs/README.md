# Documentation

**English** | [Português](README.pt-BR.md)

Where to start, and what each document is for. Files marked *(pt)* are only in Portuguese.

## Start here

| Document | For |
|---|---|
| [README](../README.md) | What Hext Remote is, download, first session, building |
| [ARCHITECTURE](../ARCHITECTURE.md) | How it works today: pieces, connection, video, who gets in |
| [DECISIONS](DECISIONS.md) | The main choices and why |
| [STATUS](STATUS.md) | What is done, being checked and next |

## Running your own server

| Document | For |
|---|---|
| [self-hosting](self-hosting.md) | Your own server and app build, end to end |
| [deploy](deploy.md) *(pt)* | The server installed with systemd, updates, monitoring, relay limits |
| [hext-server](hext-server.md) *(pt)* | The signaling server: protocol, accounts, configuration, e-mail |

## Features

| Document | For |
|---|---|
| [monitor-virtual](monitor-virtual.md) *(pt)* | Virtual monitor for computers without a screen: how it works, install, test checklists |

## Test checklists — [roteiros/](roteiros/) *(pt)*

| Document | For |
|---|---|
| [testes](roteiros/testes.md) | Testing each feature, and every command-line option |
| [atendimento-campo](roteiros/atendimento-campo.md) | A real support session from start to end (closes F4) |
| [atendimento-campo-f5](roteiros/atendimento-campo-f5.md) | Accounts used for real (closes F5) |
| [instalacao-campo](roteiros/instalacao-campo.md) | Install, update and uninstall on real machines (closes F6) |

## Phase plans — [planos/](planos/) *(pt)*

How each phase was planned and built, milestone by milestone, with what was decided along the way. Kept as a
record: the current picture is in ARCHITECTURE and STATUS.

| Document | Phase |
|---|---|
| [plano-original](planos/plano-original.md) | The first plan, before the code |
| [F3](planos/F3.md) | Internet: ID, signaling, `iroh`, relay |
| [F4](planos/F4.md) | Support essentials: Windows service, password, channels, restart |
| [F5](planos/F5.md) | Accounts: signed device list, sharing, access log |
| [F6](planos/F6.md) | Distribution: installers, updates, Mac without consent prompts |

## Studies and measurements — [estudos/](estudos/) *(pt)*

Working notes behind specific changes. They describe the code at the time they were written.

| Document | About |
|---|---|
| [fluidez-parsec](estudos/fluidez-parsec.md) | What Parsec does for smoothness, and what to apply here |
| [medicao-video](estudos/medicao-video.md) | Measuring video with the screen changing; eight rounds of results |
| [janela-travada](estudos/janela-travada.md) | The session window that froze during internet sessions: cause and fix |
| [spike-monitor-virtual](estudos/spike-monitor-virtual.md) | First test of the virtual monitor driver on Windows and macOS |
