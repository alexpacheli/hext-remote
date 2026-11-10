# Hext Remote

[English](README.md) | **Português**

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

Os instaladores ficam em [Releases](https://github.com/alexpacheli/hext-remote/releases):

| Sistema | Arquivo | Observação |
|---|---|---|
| Windows | `HextRemote-<versão>-x64.msi` | Instala o app e o serviço (acesso não assistido, tela de login). Para muitos computadores: `msiexec /i HextRemote-<versão>-x64.msi /qn`. |
| Windows | `HextRemote-<versão>-portable.exe` | Para um atendimento rápido, sem instalar. |
| macOS | `HextRemote-<versão>.dmg` | Arraste o Hext Remote para Aplicativos. |
| Linux | `.deb`, `.rpm` ou `.AppImage` | Por enquanto, linha de comando (`hext-remote`). |

Assinatura de código gratuita no Windows pelo [SignPath.io](https://about.signpath.io/), com certificado da
[SignPath Foundation](https://signpath.org/). Veja a [política de assinatura](#política-de-assinatura-de-código)
e a [privacidade](#privacidade).

**Primeiro atendimento:**

1. No computador do cliente, abra o Hext Remote. A tela inicial mostra o **ID** dele.
2. No computador do técnico, abra o Hext Remote, digite esse ID e clique em **Conectar**.
3. No cliente, aparece o pedido com o nome do técnico e as permissões. Ao aceitar, a sessão começa.

Para entrar sem ninguém aceitar, instale o serviço (o `.msi` já instala) e defina a senha de acesso na aba
**Segurança**. O roteiro completo de cada recurso, inclusive pela linha de comando, está em
[docs/testes.md](docs/testes.md), e os roteiros de atendimento em campo, em
[docs/atendimento-campo.md](docs/atendimento-campo.md).

**Atualização.** Instalado no Windows (o serviço) ou no Mac (em Aplicativos), o Hext Remote se atualiza
sozinho, entre uma sessão e outra. Nos outros casos, a tela inicial avisa quando há uma versão nova (no Linux:
`hext-remote update check`). Cada versão é assinada pelo pipeline de release, e o app recusa qualquer outra.

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

Com o [Docker](https://docs.docker.com/engine/install/) 26 ou mais novo instalado no servidor (o relay monta só a
pasta do próprio certificado, com `subpath`):

```sh
git clone https://github.com/alexpacheli/hext-remote.git
cd hext-remote/deploy/docker
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
git clone https://github.com/alexpacheli/hext-remote.git
cd hext-remote
HEXT_DOMAIN=<domínio> ./deploy/install.sh
```

O script instala o Caddy, compila o `hext-server` e o `iroh-relay`, gera o segredo do relay, cria os serviços do
systemd, libera as portas no iptables e testa os dois endereços. Para atualizar: `git pull && ./deploy/install.sh`
(o domínio fica guardado). Detalhes, logs e backup em [docs/deploy.md](docs/deploy.md).

### E-mails da conta (opcional)

Para que as contas recebam os códigos de verificação, informe um servidor SMTP: no Docker, as linhas
`HEXT_SMTP_*` do `.env`; no systemd, o arquivo `/etc/hext/mail.env`
([docs/hext-server.md](docs/hext-server.md)). Sem isso, nenhum e-mail é enviado (para desenvolvimento, `--dev-mail-log` grava os códigos no log do servidor).

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
| `installer` | Instaladores (WiX no Windows, `.dmg` no macOS, pacotes do Linux) |

## Contribuir

Sugestões e relatos de problemas são bem-vindos em [Issues](https://github.com/alexpacheli/hext-remote/issues).
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

## Política de assinatura de código

As versões do Windows passam a ser assinadas pelo [SignPath.io](https://about.signpath.io/), com um
certificado da [SignPath Foundation](https://signpath.org/) (assinatura gratuita para projetos de código
aberto). O app do macOS é assinado com um Apple Developer ID e notarizado.

- **O que é assinado:** só os arquivos gerados pelo workflow de release do GitHub Actions deste repositório
  (`.github/workflows/release.yml`) a partir de uma tag de versão. Nada compilado na máquina de um
  desenvolvedor é assinado.
- **Equipe:** quem escreve e quem aprova: [Alex Pacheli](https://github.com/alexpacheli). Cada versão é
  aprovada à mão: o pipeline abre um rascunho, e só um mantenedor o publica.
- **Privacidade:** veja a [política de privacidade](#privacidade) abaixo.

## Privacidade

O programa envia dados só para onde os seus recursos precisam:

- ao servidor de sinalização do Hext: o ID, a chave pública, o nome e a versão do dispositivo, para encontrar o
  outro computador;
- com uma conta (opcional): o e-mail e os dados de entrada, a lista assinada de dispositivos e computadores da
  conta e, para um computador vinculado à conta, o registro de acessos (quem conectou e quando), mostrado na
  tela Atividade da conta. Os códigos de verificação vão para esse e-mail por um provedor de e-mail;
- ao relay: a própria sessão, cifrada de ponta a ponta, quando não há conexão direta (o relay não vê o
  conteúdo);
- ao GitHub: a busca e o download de atualizações.

A tela, o teclado, os arquivos e a área de transferência de uma sessão vão só para o outro computador, cifrados
de ponta a ponta. Nada é vendido ou compartilhado com terceiros, e não há publicidade nem estatísticas de uso.
Contas não confirmadas por e-mail são excluídas em até 24 horas, e uma conta pode ser excluída a qualquer
momento pelo app (Conta → Excluir a conta).

Ninguém conecta a um computador sem o aceite na tela, sem a senha de acesso definida pelo dono ou sem um
dispositivo de uma conta que o dono vinculou. Toda sessão aparece na tela enquanto dura, e o app instalado
guarda um registro local delas.
