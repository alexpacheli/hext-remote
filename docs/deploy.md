# Servidor (systemd)

Uma VM roda tudo o que o Hext Remote precisa na internet. Este guia instala direto no sistema, com o
`deploy/install.sh`; com Docker, veja a seção "Servidor próprio" do [README](../README.md). Os exemplos usam o
domínio do servidor oficial, `hext.com.br`: troque pelo seu.

```
Internet ──443/TCP──► Caddy ─┬─ signal.<domínio> ─► hext-server (127.0.0.1:8080)
                             └─ relay.<domínio>  ─► iroh-relay  (127.0.0.1:3340)
         ──7842/UDP─────────────────────────────────► iroh-relay  (descoberta de endereço, QUIC)

iroh-relay ──"este computador pode usar o relay?"──► hext-server (/v1/relay-access)
```

- **Caddy** atende a porta 443 para os dois nomes, com certificados da Let's Encrypt renovados sozinhos.
- **`hext-server`** cuida dos IDs, da presença e dos pedidos de conexão (sinalização). O banco SQLite fica
  em `/var/lib/hext/hext-server.db`.
- **`iroh-relay`** repassa as sessões que não conseguem conexão direta. Ele **só aceita computadores
  registrados no `hext-server`**: a cada conexão, pergunta ao `hext-server`, apresentando um segredo
  guardado em `/etc/hext/secrets.env`. A porta UDP 7842 serve para cada computador descobrir o próprio
  endereço público, o que ajuda na conexão direta. Ela usa o certificado que o Caddy obtém: o relay roda com
  um usuário próprio do systemd e recebe só o certificado e a chave do nome dele (`LoadCredential`), sem acesso
  às demais chaves do Caddy. Quando o Caddy renova o certificado, a unidade `iroh-relay-cert.path` reinicia o
  relay com o novo.
- Nenhum dos serviços vê o conteúdo das sessões: o tráfego é cifrado de ponta a ponta entre os dois
  computadores.

## Primeira instalação

1. **DNS** (Cloudflare): crie um registro `A` para `signal` apontando para o IP da VM, igual ao de
   `relay`, e deixe em **"DNS only"** (nuvem cinza). O Caddy precisa receber a conexão direto para obter
   o certificado.
2. **Portas na Oracle** (Security List): TCP 80 e 443 e UDP 7842, as mesmas do relay. Se você abriu a
   TCP 8080 para o teste, pode remover.
3. **Na VM** (Debian 11, Ubuntu 22.04 ou mais novos: o script exige o systemd 247), com o Rust já instalado
   (`rustup`):

   ```sh
   git clone https://github.com/alexpacheli/remote-control.git   # se ainda não clonou
   cd remote-control
   git pull
   HEXT_DOMAIN=hext.com.br ./deploy/install.sh
   ```

   (Se os nomes não seguirem o padrão `signal.` e `relay.`, informe cada um:
   `SIGNAL_HOST=… RELAY_HOST=… ./deploy/install.sh`.) Os nomes ficam guardados em `/etc/hext/hosts.env`.

   O script:
   - instala o Caddy;
   - compila o `hext-server` e instala o `iroh-relay` 1.4.1;
   - gera o segredo do relay;
   - copia as configurações e os serviços do systemd;
   - libera as portas no iptables (e remove a 8080 do teste);
   - sobe tudo e testa os dois endereços.

   No fim, ele deve mostrar:

   ```
   ok    https://signal.hext.com.br/health
   ok    https://relay.hext.com.br/generate_204
   ```

   Na primeira vez, o `iroh-relay` pode reiniciar algumas vezes, até o Caddy obter o certificado (menos
   de um minuto).

A versão oficial do app usa o servidor de `hext.com.br`. Para o seu domínio, compile o app com `HEXT_SERVER` e
`HEXT_RELAY` (seção "O app para o seu servidor" do [README](../README.md)), ou teste com
`--server wss://signal.<domínio> --relay https://relay.<domínio>` no `host` e no `view`.

## Atualizar

```sh
cd remote-control && git pull && ./deploy/install.sh
```

O domínio, o banco e o segredo continuam os mesmos. Os computadores reconectam sozinhos depois do reinício.
(Num servidor instalado antes de o domínio ser uma opção, o script reconhece os nomes de `hext.com.br`.)

## Acompanhar

```sh
journalctl -u hext-server -u iroh-relay -u caddy -f      # logs
sudo systemctl status hext-server iroh-relay caddy        # estado
# auditoria (sudo apt install sqlite3)
sudo sqlite3 /var/lib/hext/hext-server.db \
  "SELECT datetime(at,'unixepoch'), from_id, to_id, event FROM audit ORDER BY id DESC LIMIT 20"
```

### Uso e tráfego

```sh
./deploy/uso.sh
```

Mostra o tráfego da rede no mês (pelo `vnstat`, que o `install.sh` instala) comparado com o limite do provedor,
e o relay desde que iniciou: quanto repassou, quantos dispositivos estão conectados e quantas conexões bateram no
limite por dispositivo. O limite do provedor é de 10 TB por mês por padrão (o da Oracle);
`HEXT_LIMITE_GB=5000 ./deploy/uso.sh` usa outro. Com Docker, o mesmo script funciona no servidor (o `vnstat`
é opcional: `sudo apt install vnstat`).

**Limite por dispositivo.** Cada dispositivo envia no máximo 5 Mbit/s pelo relay (`[limits.client.rx]` em
`deploy/iroh-relay.toml`), para que uma sessão pesada não ocupe o link inteiro. O app baixa a qualidade do
vídeo para caber. O limite vale para tudo o que passa pelo relay, inclusive arquivos. As sessões diretas (P2P) não têm limite. Se `conexões que bateram no limite` crescer muito e
houver folga de tráfego, aumente o valor.

## Relay sem limite para contas escolhidas

O limite de 5 Mbit/s vale para todos no relay normal. Para liberar o relay sem limite a contas escolhidas
(as suas, por exemplo), o servidor roda um segundo `iroh-relay`, sem `[limits]`, que só aceita essas contas.

1. Crie o nome `relay-full.<domínio>` apontando para este servidor (o mesmo IP dos outros dois).
2. Se o provedor tiver firewall próprio (ex.: a Security List da Oracle), libere a porta UDP 7843.
3. No servidor:

   ```sh
   RELAY_FULL_HOST=relay-full.hext.com.br ./deploy/install.sh
   ```

   O nome fica guardado em `/etc/hext/hosts.env`, então as próximas atualizações continuam só com
   `./deploy/install.sh`. Para remover o relay sem limite: `RELAY_FULL_HOST= ./deploy/install.sh`.
4. Insira os e-mails das contas em `/etc/hext/relay-livre.txt`, um por linha (`#` começa um comentário):

   ```sh
   sudoedit /etc/hext/relay-livre.txt
   ```

   O arquivo é lido a cada consulta: incluir ou remover uma conta vale na hora, sem reiniciar nada. Só o
   `hext-server` o lê (dono `root`, grupo `hext`, permissão 640; o `install.sh` cria o grupo e acerta as
   permissões).

Como funciona:

- Ao entrar no servidor, cada dispositivo e computador de uma conta da lista recebe o endereço desse relay e
  passa a usá-lo no lugar do normal. Contam os dispositivos da lista da conta (Conta → Dispositivos), os
  computadores em Meus computadores e os dispositivos com a conta aberta. Quem sai da lista volta ao relay
  normal no próximo login (o serviço do computador reconecta sozinho).
- Quem abre uma sessão com uma dessas contas (um técnico de outra conta, ou alguém aceito na tela) pode usar
  esse relay durante a sessão: o `hext-server` libera os dois lados por 2 minutos, o tempo de se conectarem a
  ele, quando o pedido é aceito.
- O limite só atrapalha quem envia o vídeo, o computador acessado. Por isso os computadores da conta também
  precisam estar em Meus computadores para usar o relay sem limite.
- `./deploy/uso.sh` mostra os contadores dos dois relays.

Só na instalação com systemd por enquanto: o Docker continua com um relay só.

## Segurança

- O `hext-server` e o `iroh-relay` escutam só em `127.0.0.1`. Da internet chegam só o Caddy (80/443) e a
  descoberta de endereço do relay (UDP 7842; e 7843 para o relay sem limite, se houver).
- O `/v1/relay-access` e o `/v1/relay-access-full` são bloqueados no Caddy e exigem o segredo: só os relays
  da própria VM perguntam.
- Por isso o app só usa o relay padrão com o servidor padrão: ele se registra no servidor antes de chegar ao
  relay. Pelo IP, ou com `--server off` ou outro servidor, a conexão é só direta, a não ser que você informe
  outro relay.
- O Caddy usa só a Let's Encrypt (`acme_ca`), porque o relay recebe o certificado da pasta dela.
- Cada relay roda com um usuário próprio (`DynamicUser`) e só recebe o certificado e a chave do próprio nome;
  não lê as chaves dos outros nomes do Caddy. No Docker, o contêiner do relay monta só a pasta do certificado
  dele, sem nenhuma capacidade (`cap_drop`) e com o sistema de arquivos somente leitura.
- Em HTTP simples (porta 80), o Caddy só repassa ao relay o `/generate_204`, usado pelo iroh para detectar
  portais cativos; o resto é redirecionado para HTTPS.
- `/etc/hext/secrets.env` e `/etc/hext/mail.env` têm permissão 600 (só o root; o systemd os lê para os
  serviços).
- Limites contra tentativas de adivinhar IDs: por computador, por IP (pelo `X-Forwarded-For` do Caddy) e
  de computadores novos por IP (veja `docs/hext-server.md`).
- **Backup:** copie `/var/lib/hext/hext-server.db` de vez em quando. Ele guarda a ligação entre IDs e
  chaves; sem ele, os computadores ganham IDs novos.
