# Hext Remote

Acesso e suporte remoto para Windows, macOS e Linux, focado em fluidez. Software livre ([AGPL-3.0](LICENSE)),
escrito em Rust.

- **Conexão pelo ID:** cada computador tem um ID de 9 dígitos. O técnico digita o ID, e quem está do outro lado
  aceita ou recusa.
- **Direto quando possível:** os dois computadores se conectam direto, mesmo em redes diferentes. Quando não dá
  (um firewall corporativo, por exemplo), a sessão passa por um relay, sempre cifrada de ponta a ponta.
- **Vídeo pela placa de vídeo:** captura, codificação H.264 e exibição pela GPU, com volta automática para o
  processador.
- **Acesso não assistido:** com o serviço instalado, o computador fica online desde o boot. O técnico entra com
  uma senha definida pelo dono ou, depois, sem senha, pela chave dele ou pela conta. No Windows, isso vale
  também na tela de login e no UAC.
- **Na sessão:** vários monitores, área de transferência, arquivos, chat, reinício remoto e até 8 sessões ao
  mesmo tempo.
- **Contas (opcionais):** lista de computadores sincronizada, compartilhamento com outras contas e registro dos
  acessos.
- **Transparente:** toda sessão aparece na tela enquanto dura, e cada acesso fica registrado no computador.

## Como funciona

```
                    ┌──────────────────────────────┐
         ┌─────────►│  Servidor de sinalização     │◄─────────┐
         │          │  (hext-server): IDs,         │          │
         │          │  presença e pedidos          │          │
         │          └──────────────────────────────┘          │
         │                                                    │
  ┌──────┴───────┐     sessão direta (P2P, QUIC)       ┌──────┴───────┐
  │   Técnico    │◄───────────────────────────────────►│   Cliente    │
  │ Hext Remote  │                                     │ Hext Remote  │
  └──────┬───────┘                                     └──────┬───────┘
         │          ┌──────────────────────────────┐          │
         └─────────►│  Relay (iroh-relay): só sem  │◄─────────┘
                    │  conexão direta; não vê o    │
                    │  conteúdo                    │
                    └──────────────────────────────┘
```

1. Ao abrir, o app se registra no **servidor de sinalização** com a chave do computador (Ed25519, criada na
   primeira execução) e recebe o seu ID.
2. O técnico pede para conectar a um ID. O servidor repassa o pedido, e o outro lado aceita, ou confere a senha
   ou a chave do técnico.
3. Os dois abrem uma conexão **QUIC** entre si, atravessando o NAT. Se não conseguirem, usam o **relay**.
4. A sessão é cifrada de ponta a ponta entre os dois computadores: nem o servidor nem o relay veem a tela, o
   teclado ou os arquivos. A senha de acesso nunca trafega (SPAKE2).

Detalhes em [ARCHITECTURE.md](ARCHITECTURE.md) e no plano de cada fase ([F3](docs/F3.md) a [F6](docs/F6.md)).

## Baixar e usar

Os instaladores ficam em [Releases](https://github.com/alexpacheli/remote-control/releases):

| Sistema | Arquivo | Observação |
|---|---|---|
| Windows | `HextRemote-<versão>-x64.msi` | Instala o app e o serviço (acesso não assistido, tela de login). Para muitos computadores: `msiexec /i HextRemote-<versão>-x64.msi /qn`. |
| Windows | `HextRemote-<versão>-portable.exe` | Para um atendimento rápido, sem instalar. |
| macOS | `HextRemote-<versão>.dmg` | Arraste o Hext Remote para Aplicativos. |
| Linux | `.deb`, `.rpm` ou `.AppImage` | Por enquanto, linha de comando (`hext-remote`). |

**Primeiro atendimento:**

1. No computador do cliente, abra o Hext Remote. A tela inicial mostra o **ID** dele.
2. No computador do técnico, abra o Hext Remote, digite esse ID e clique em **Conectar**.
3. No cliente, aparece o pedido com o nome do técnico e as permissões. Ao aceitar, a sessão começa.

Para entrar sem ninguém aceitar, instale o serviço (o `.msi` já instala) e defina a senha de acesso na aba
**Segurança**. O roteiro completo de cada recurso, inclusive pela linha de comando, está em
[docs/testes.md](docs/testes.md), e os roteiros de atendimento em campo, em
[docs/atendimento-campo.md](docs/atendimento-campo.md).

**Atualização.** Com o serviço do Windows, o Hext Remote se atualiza sozinho, entre uma sessão e outra. Nos
outros casos, a tela inicial avisa quando há uma versão nova (no Linux: `hext-remote update check`). Cada versão é
assinada pelo pipeline de release, e o app recusa qualquer outra.

## Servidor próprio

A versão oficial usa o nosso servidor (`signal.hext.com.br` e `relay.hext.com.br`). Quem atende muitos
computadores, quer os dados na própria infraestrutura ou distribui uma versão modificada sobe o próprio servidor
e compila o app apontando para ele. O servidor e o relay são deste repositório e do projeto
[iroh](https://github.com/n0-computer/iroh), sem nenhum serviço pago.

**Do que precisa:**

- Um servidor Linux com IP público. Para começar, 1 a 2 vCPU e 1 a 2 GB de RAM bastam (a Oracle Cloud tem
  um plano gratuito com bastante tráfego). Prefira um servidor perto dos usuários, por causa do relay.
- Um domínio com dois nomes apontando para o IP do servidor: `signal.<domínio>` e `relay.<domínio>`. Com a
  Cloudflare, deixe os dois em **DNS only** (nuvem cinza).
- As portas **80 e 443 TCP** e **7842 UDP** abertas no firewall do provedor.

**Quanto consome.** A sinalização é leve: ela só guarda IDs e presença e repassa os pedidos de conexão, e um
servidor pequeno atende milhares de computadores conectados. O que custa é o **relay**: uma sessão que passa por
ele usa de 0,5 a 3 Mbps (cerca de 0,2 a 1,3 GB por hora). A maioria das sessões é direta e não passa por ele. Cada dispositivo envia no máximo 5 Mbit/s pelo relay, e o app ajusta a
qualidade a isso. Para acompanhar o tráfego do mês e o uso do relay: `./deploy/uso.sh`, no servidor.

### Opção 1: Docker

Com o [Docker](https://docs.docker.com/engine/install/) instalado no servidor:

```sh
git clone https://github.com/alexpacheli/remote-control.git
cd remote-control/deploy/docker
cp .env.example .env
nano .env        # HEXT_DOMAIN e HEXT_RELAY_TOKEN (gere com: openssl rand -hex 32)
docker compose up -d --build
```

Isso sobe três contêineres: o `hext-server`, o `iroh-relay` e o Caddy, que obtém os certificados da Let's Encrypt
sozinho. Na primeira vez, a compilação leva alguns minutos, e o relay reinicia algumas vezes até o Caddy obter o
certificado (menos de um minuto). Para conferir:

```sh
curl https://signal.<domínio>/health        # ok
docker compose logs -f
```

Para atualizar: `git pull && docker compose up -d --build`. O banco fica no volume `hext_server`: faça backup dele.
Sem ele, os computadores ganham IDs novos.

### Opção 2: direto no sistema (systemd)

Num Debian ou Ubuntu, com o [Rust](https://rustup.rs) instalado:

```sh
git clone https://github.com/alexpacheli/remote-control.git
cd remote-control
HEXT_DOMAIN=<domínio> ./deploy/install.sh
```

O script instala o Caddy, compila o `hext-server` e o `iroh-relay`, gera o segredo do relay, cria os serviços do
systemd, libera as portas no iptables e testa os dois endereços. Para atualizar: `git pull && ./deploy/install.sh`
(o domínio fica guardado). Detalhes, logs e backup em [docs/deploy.md](docs/deploy.md).

### E-mails da conta (opcional)

Para que as contas recebam os códigos de verificação, informe um servidor SMTP: no Docker, as linhas
`HEXT_SMTP_*` do `.env`; no systemd, o arquivo `/etc/hext/mail.env`
([docs/hext-server.md](docs/hext-server.md)). Sem isso, os códigos vão só para o log do servidor.

### O app para o seu servidor

O endereço do servidor e do relay entra no app na compilação, pelas variáveis `HEXT_SERVER` e `HEXT_RELAY`:

- **Pelo GitHub (recomendado):** faça um fork deste repositório e, em **Settings → Secrets and variables →
  Actions → Variables**, crie `HEXT_SERVER` = `wss://signal.<domínio>` e `HEXT_RELAY` = `https://relay.<domínio>`.
  Cada tag `v*` gera os instaladores em Releases do seu fork, e o app procura atualizações lá. Para assinar as
  atualizações e os instaladores, veja a seção 4 de [docs/F6.md](docs/F6.md).
- **Na sua máquina:**

  ```sh
  HEXT_SERVER=wss://signal.<domínio> HEXT_RELAY=https://relay.<domínio> cargo build --release -p hext
  ```

Para um teste rápido, sem compilar: `HextRemote host --server wss://signal.<domínio> --relay https://relay.<domínio>`
e o mesmo no `view`. Os computadores de um servidor não enxergam os de outro.

## Compilar

Precisa do [Rust](https://rustup.rs). A versão é fixada em `rust-toolchain.toml` e instalada sozinha. No Windows,
precisa também do Visual Studio Build Tools com a carga "Desenvolvimento para desktop com C++".

```sh
cargo run -p hext              # a interface
cargo run -p hext -- --help    # a linha de comando
cargo run -p hext-server       # o servidor, em http://127.0.0.1:8080 (teste: --server ws://127.0.0.1:8080)
cargo test --workspace         # testes (inclui um de ponta a ponta host → cliente)
```

Variáveis da compilação, todas opcionais:

| Variável | O que define | Sem ela |
|---|---|---|
| `HEXT_SERVER` | Servidor de sinalização | `wss://signal.hext.com.br` |
| `HEXT_RELAY` | Relay | `https://relay.hext.com.br`; com outro `HEXT_SERVER`, nenhum (só conexão direta) |
| `HEXT_RELEASES` | Página das versões, onde o app procura atualizações | as Releases deste repositório |
| `HEXT_UPDATE_KEY` | Chave pública que assina as atualizações | sem atualização automática |

## Estrutura

| Pasta | Conteúdo |
|---|---|
| `apps/hext` | O app: interface (Tauri), `host`, `view` e `service` |
| `apps/hext-server` | Servidor: IDs, presença, sinalização e contas ([docs/hext-server.md](docs/hext-server.md)) |
| `crates/proto` | Mensagens do protocolo: handshake, vídeo, input |
| `crates/crypto` | Identidade do computador (Ed25519) e senha de acesso (SPAKE2) |
| `crates/signal` | Sinalização: ID de 9 dígitos, mensagens entre o app e o servidor, cliente WebSocket |
| `crates/net` | Transporte (`iroh`/QUIC): direto quando possível, relay senão |
| `crates/capture` | Captura de tela (DXGI no Windows, ScreenCaptureKit no macOS) |
| `crates/codec` | H.264: placa de vídeo (Media Foundation, VideoToolbox) ou OpenH264 em software |
| `crates/input` | Mapa de teclas e injeção de mouse e teclado (SendInput, CGEvent) |
| `crates/render` | Renderização na GPU (wgpu), YUV → RGB no shader |
| `crates/session` | Pipelines do host e do cliente |
| `crates/metrics` | Estatísticas de latência |
| `deploy` | Servidor: script do systemd, Caddy, relay e Docker |
| `installer` | Instalador do Windows (WiX) |

## Contribuir

Sugestões e relatos de problemas são bem-vindos em [Issues](https://github.com/alexpacheli/remote-control/issues).
Para enviar uma mudança, abra um pull request. O CI roda nos três sistemas:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked
cargo test --workspace --locked
```

Ao contribuir, você concorda em licenciar a sua contribuição sob a mesma licença do projeto.

## Licença

O Hext Remote é software livre, sob a [GNU Affero General Public License v3.0](LICENSE) (`AGPL-3.0-only`).
Quem distribuir o programa modificado, ou oferecer uma versão modificada do servidor pela rede, precisa
publicar o código-fonte dessa versão sob a mesma licença.

## Code signing policy

Windows releases are to be signed through [SignPath.io](https://about.signpath.io/), with a certificate
from the [SignPath Foundation](https://signpath.org/) (free code signing for open-source projects). The macOS
app is signed with an Apple Developer ID and notarized.

- **What is signed:** only files built by this repository's GitHub Actions release workflow
  (`.github/workflows/release.yml`) from a version tag. Nothing built on a developer's machine is signed.
- **Team:** committer and approver: [Alex Pacheli](https://github.com/alexpacheli). Every release is
  approved by hand: the pipeline opens a draft, and only a maintainer publishes it.
- **Privacy:** this program sends data only where its features need it:
  - the Hext signaling server: the device's ID, public key, name and version, to find the other computer;
  - with an account (optional): the e-mail and the sign-in data, the account's signed list of devices and
    computers and, for a computer bound to the account, its access log (who connected and when), shown on
    the account's Activity screen. Verification codes are sent to that e-mail through an e-mail provider;
  - the relay: the session itself, end-to-end encrypted, when a direct connection is not possible;
  - GitHub: update checks and downloads.

  Nobody connects to a computer without the user's acceptance on screen, an unattended password the
  computer's owner set, or a device of an account the owner bound. Every session is shown on screen while it
  lasts, and the installed app keeps a local log of them.
