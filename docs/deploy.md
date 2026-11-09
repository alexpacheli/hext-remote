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
  endereço público, o que ajuda na conexão direta. Ela usa o certificado que o Caddy obtém.
- Nenhum dos serviços vê o conteúdo das sessões: o tráfego é cifrado de ponta a ponta entre os dois
  computadores.

## Primeira instalação

1. **DNS** (Cloudflare): crie um registro `A` para `signal` apontando para o IP da VM, igual ao de
   `relay`, e deixe em **"DNS only"** (nuvem cinza). O Caddy precisa receber a conexão direto para obter
   o certificado.
2. **Portas na Oracle** (Security List): TCP 80 e 443 e UDP 7842, as mesmas do relay. Se você abriu a
   TCP 8080 para o teste, pode remover.
3. **Na VM**, com o Rust já instalado (`rustup`):

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

## Segurança

- O `hext-server` e o `iroh-relay` escutam só em `127.0.0.1`. Da internet chegam só o Caddy (80/443) e a
  descoberta de endereço do relay (UDP 7842).
- O `/v1/relay-access` é bloqueado no Caddy e exige o segredo: só o relay da própria VM pergunta.
- Por isso o app só usa o relay padrão com o servidor padrão: ele se registra no servidor antes de chegar ao
  relay. Pelo IP, ou com `--server off` ou outro servidor, a conexão é só direta, a não ser que você informe
  outro relay.
- O Caddy usa só a Let's Encrypt (`acme_ca`), porque o relay lê o certificado da pasta dela.
- Limites contra tentativas de adivinhar IDs: por computador, por IP (pelo `X-Forwarded-For` do Caddy) e
  de computadores novos por IP (veja `docs/hext-server.md`).
- **Backup:** copie `/var/lib/hext/hext-server.db` de vez em quando. Ele guarda a ligação entre IDs e
  chaves; sem ele, os computadores ganham IDs novos.
