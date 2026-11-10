# Servidor próprio

[English](self-hosting.md) | **Português**

A versão oficial do Hext Remote usa o nosso servidor (`signal.hext.com.br` e `relay.hext.com.br`). Este guia
monta o seu, de ponta a ponta: o servidor, um app feito para ele e o que é preciso para manter os dois.

São três partes:

- **o servidor:** a sinalização (`hext-server`), o relay (`iroh-relay`) e o Caddy para o TLS;
- **um app feito para o seu servidor:** o app oficial só conversa com o servidor oficial;
- **alguns hábitos:** backup, atualização e acompanhar o tráfego do relay.

## 1. Preparar

- **Um servidor Linux com IP público:** Debian 11+ ou Ubuntu 22.04+ (systemd 247 ou mais novo). Para começar,
  1 a 2 vCPUs e 1 a 2 GB de RAM bastam. Prefira um servidor perto dos seus usuários, porque as sessões sem
  caminho direto passam pelo relay dele.
- **Um domínio com dois nomes** apontando para o IP do servidor: `signal.<domínio>` e `relay.<domínio>`. No
  Cloudflare, deixe os dois como **somente DNS** (nuvem cinza): o Caddy precisa receber as conexões para obter
  os certificados.
- **Portas abertas** no firewall do provedor (por exemplo, a Security List da Oracle Cloud):
  - **80/TCP e 443/TCP**: o Caddy (HTTPS e o desafio do Let's Encrypt);
  - **7842/UDP**: a descoberta de endereço do relay, que ajuda as conexões diretas.

  Nada mais precisa ficar aberto: o `hext-server` e o `iroh-relay` escutam só em `127.0.0.1`.

## 2. Instalar o servidor

Escolha um dos dois.

### Docker

Com o [Docker](https://docs.docker.com/engine/install/) 26 ou mais novo:

```sh
git clone https://github.com/alexpacheli/hext-remote.git
cd hext-remote/deploy/docker
cp .env.example .env
nano .env        # HEXT_DOMAIN, e HEXT_RELAY_TOKEN (gere com: openssl rand -hex 32)
docker compose up -d --build
```

### systemd

Com o [Rust](https://rustup.rs) instalado:

```sh
git clone https://github.com/alexpacheli/hext-remote.git
cd hext-remote
HEXT_DOMAIN=<domínio> ./deploy/install.sh
```

O script instala o Caddy, compila o `hext-server`, instala o `iroh-relay`, gera o segredo do relay
(`/etc/hext/secrets.env`, modo 600), instala as unidades do systemd, abre as portas no iptables e testa os dois
endereços. Se os seus nomes não seguem `signal.` e `relay.`, informe cada um: `SIGNAL_HOST=… RELAY_HOST=…`.

### Conferir

```sh
curl https://signal.<domínio>/health                                   # ok
curl -s -o /dev/null -w '%{http_code}\n' https://relay.<domínio>/generate_204   # 204
```

Na primeira vez, o relay pode reiniciar algumas vezes até o Caddy obter o certificado (menos de um minuto).

## 3. E-mails da conta

A conta é opcional para os usuários, mas o cadastro recebe um código de verificação por e-mail. **Sem SMTP,
nenhum e-mail é enviado e ninguém consegue criar uma conta.** Qualquer provedor SMTP serve (Brevo, Amazon SES,
o seu); a conexão é sempre criptografada (STARTTLS na 587, TLS na 465).

- **Docker:** descomente e preencha as linhas `HEXT_SMTP_*` e `HEXT_MAIL_FROM` do `.env` e rode
  `docker compose up -d`.
- **systemd:** crie primeiro o `/etc/hext/mail.env` vazio, com modo 600, e depois preencha:

  ```sh
  sudo install -m 600 -o root -g root /dev/null /etc/hext/mail.env
  sudo nano /etc/hext/mail.env
  ```

  ```
  HEXT_SMTP_HOST=smtp.exemplo.com.br
  HEXT_SMTP_PORT=587
  HEXT_SMTP_USER=<login do SMTP>
  HEXT_SMTP_PASSWORD=<senha do SMTP>
  HEXT_MAIL_FROM=Hext Remote <nao-responda@seu-dominio.com.br>
  ```

  Depois, `sudo systemctl restart hext-server`.

O servidor limita os e-mails (5 por hora por IP e por endereço; no total, 100 por hora e 250 por dia, ajustáveis
com `HEXT_MAIL_PER_HOUR` e `HEXT_MAIL_PER_DAY` conforme a cota do seu provedor). Detalhes em
[hext-server.md](hext-server.md).

## 4. Gerar o app para o seu servidor

Os endereços do servidor e do relay ficam dentro do app. **Os instaladores oficiais não encontram o seu
servidor.**

### No GitHub (recomendado)

1. Faça um **fork** deste repositório.
2. No fork, em **Settings → Secrets and variables → Actions → Variables**, crie:
   - `HEXT_SERVER` = `wss://signal.<domínio>`
   - `HEXT_RELAY` = `https://relay.<domínio>`
3. **Chave de atualização** (sem ela, os apps nunca se atualizam sozinhos):
   1. Compile ou baixe qualquer `hext` e rode `hext update keygen` (no Windows: `HextRemote.exe update keygen`).
   2. Guarde a metade privada no **secret** `UPDATE_SIGNING_KEY` e a metade pública na **variável**
      `HEXT_UPDATE_KEY`.
   3. Proteja a metade privada: quem a tiver consegue enviar atualizações para todos os computadores com a sua
      versão.
4. Defina a versão no `Cargo.toml` da raiz (por exemplo, `0.6.4`) e envie uma tag igual (`v0.6.4`). O workflow
   **Release** gera os instaladores de Windows, macOS e Linux e abre uma release em **rascunho** no seu fork.
   Publique-a manualmente; os apps procuram atualizações nas releases do seu fork.

**Assinar os instaladores (opcional).** Sem certificados, os instaladores saem sem assinatura: o SmartScreen do
Windows e o Gatekeeper do macOS avisam na primeira abertura. Para assinar, crie os secrets listados na seção 4
do [F6.md](F6.md):

- **Windows:** `WINDOWS_CERTIFICATE` e `WINDOWS_CERTIFICATE_PASSWORD`;
- **macOS:** `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`,
  `APPLE_APP_PASSWORD` e `APPLE_TEAM_ID` (Apple Developer Program).

O app instalado no Mac só se atualiza sozinho quando assinado com um Developer ID.

### Na sua máquina

```sh
HEXT_SERVER=wss://signal.<domínio> HEXT_RELAY=https://relay.<domínio> cargo build --release -p hext
```

Uma compilação local sem `HEXT_UPDATE_KEY` não se atualiza sozinha.

### Teste rápido, sem compilar

```sh
HextRemote host --server wss://signal.<domínio> --relay https://relay.<domínio>
HextRemote view <ID> --server wss://signal.<domínio> --relay https://relay.<domínio>
```

Computadores em servidores diferentes não se enxergam.

## 5. Operar

**Faça backup do banco de dados.** Ele guarda a ligação entre o ID de cada computador e a chave dele, além das
contas. Se ele se perder, todos os computadores recebem um ID novo e as contas somem. Copie com frequência:

- **systemd:** `/var/lib/hext/hext-server.db`;
- **Docker:** o volume `hext_server`.

**Atualizar o servidor:**

- **systemd:** `cd hext-remote && git pull && ./deploy/install.sh` (o domínio fica guardado em
  `/etc/hext/hosts.env`);
- **Docker:** `git pull && docker compose up -d --build`.

Os computadores se reconectam sozinhos depois do reinício. Atualize o servidor antes de publicar um app que
dependa dele.

**Acompanhe o tráfego do relay.** A sinalização é leve: um servidor pequeno atende milhares de computadores
conectados. O que custa é o relay:
- uma sessão por ele usa de 0,5 a 3 Mbps (cerca de 0,2 a 1,3 GB por hora);
- a maioria das sessões é direta e não passa por ele;
- cada dispositivo envia no máximo 5 Mbit/s por ele (`[limits.client.rx]` em `deploy/iroh-relay.toml`), e o app
  reduz a qualidade do vídeo para caber.

O `./deploy/uso.sh` mostra o tráfego do mês em relação ao limite do provedor e o quanto o relay já repassou.

**Logs:**

```sh
journalctl -u hext-server -u iroh-relay -u caddy -f      # systemd
docker compose logs -f                                    # Docker
```

**Relay sem limite, para contas escolhidas (só systemd).** Um segundo relay, sem o limite de 5 Mbit/s, apenas
para as contas que você listar. Veja "Relay sem limite" no [deploy.md](deploy.md).

## 6. Segurança

- Apenas as portas 80/443 TCP e 7842 UDP abertas. O relay só aceita computadores registrados no seu
  `hext-server`, que ele consulta com o segredo do `secrets.env` (ou `HEXT_RELAY_TOKEN`).
- Mantenha `/etc/hext/secrets.env`, `/etc/hext/mail.env` e o `.env` privados (modo 600) e fora do git.
- Proteja também a VM:
  - SSH só com chave (`PasswordAuthentication no`) e `fail2ban`;
  - atualizações de segurança automáticas (`unattended-upgrades`);
  - nada mais escutando em portas públicas.
- As sessões são criptografadas de ponta a ponta entre os dois computadores: nem o seu servidor nem o seu relay
  veem a tela, o teclado ou os arquivos.

## 7. Licença

O Hext Remote está sob a [AGPL-3.0](../LICENSE). Usar sem modificações para o seu próprio suporte não exige
nada. Se você **modificar** o app ou o servidor e oferecê-lo a outras pessoas (inclusive pela rede), precisa
publicar o código-fonte dessa versão sob a mesma licença.
