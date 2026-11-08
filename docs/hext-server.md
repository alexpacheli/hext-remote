# hext-server

O servidor dá a cada computador o seu **ID de 9 dígitos**, sabe quem está online e repassa os pedidos de
conexão entre eles (sinalização). Ele **nunca vê o conteúdo das sessões**: os computadores se conectam
direto entre si (ou por um relay), cifrados de ponta a ponta com as próprias chaves.

## Protocolo

Cada app mantém um WebSocket aberto em `wss://<servidor>/v1/signal`. As mensagens são JSON, marcadas por
`"type"`, e os tipos ficam em `crates/signal` (`ServerMsg` e `ClientMsg`).

```
App                                   Servidor
 │◄── challenge {protocol, nonce} ──────│   32 bytes aleatórios
 │─── hello {chave, assinatura, nome,   │   assinatura Ed25519 de "hext-signal-auth-v1\0" + nonce
 │           host ou técnico} ─────────►│
 │◄── welcome {id, aceita attempt,      │   ID atribuído no primeiro login, o mesmo para sempre
 │       aceita presence, aceita        │
 │       account} ──────────────────────│

Técnico                  Servidor                   Host
 │── connect {1, id} ──────►│                          │
 │                          │── request {r, de, chave, nome} ─►│  (o host pergunta ao usuário)
 │                          │◄── accept {r, endereços, código} ─│  ou reject {r}
 │◄─ accepted {1, chave do host, endereços, código} ─│         │  ou rejected {1, motivo}
 │                          │◄── attempt {técnico, entrou} ────│  depois de um pedido com senha
```

- **Login:** o app prova que tem a chave assinando o desafio. O servidor associa a chave pública a um ID
  aleatório no primeiro login (não sequencial) e devolve sempre o mesmo ID para a mesma chave. Não há senha
  nem token guardado.
- **Host e técnico:** no login, o app diz se recebe pedidos (host) ou só faz pedidos (técnico). Um novo
  login de host com a mesma chave encerra o anterior (o computador reconectou); logins de técnico convivem
  com o host do mesmo computador.
- **Pedidos:** expiram em 60 s sem resposta (`rejected: timeout` para o técnico, `cancelled` para o host).
  Se o técnico sai, o host recebe `cancelled`; se o host sai, o técnico recebe `rejected: offline`. Só o
  host pedido pode responder.
- **Proteção:**
  - pedidos de conexão: até 10 por minuto por computador e 30 por endereço IP (`too_many_requests`). O
    limite não zera ao reconectar;
  - um pedido esperando por técnico e host, e até 5 por host (`busy`);
  - bloqueio temporário do ID alvo: depois de 10 pedidos em 10 minutos para o mesmo host, vindos de
    computadores em que ele não confia (de quaisquer IPs), ele fica fechado para
    desconhecidos por 15 minutos (`too_many_requests`). Cada desconhecido conta no máximo 3 pedidos por
    host nessa janela (os seguintes são recusados só para ele), então um computador sozinho não fecha
    o host; repetir o pedido enquanto um espera (`busy`) não conta. Quem o host aceitou nos últimos 90
    dias continua passando, para que uma enxurrada de pedidos não tire o acesso do técnico de sempre. A
    tabela `accepted` guarda esses pares;
  - quem entrou pela senha: o servidor não vê a senha, então o host conta como foi (`attempt`). O servidor
    só aceita "entrou" de um host que deixou passar um pedido com senha desse técnico nos últimos 5 minutos.
    Quem entrou também fica de fora desses limites, por 7 dias desde a última entrada e só em pedidos com
    senha, e perde isso na primeira senha errada. A tabela `entered` guarda esses pares;
  - logins: até 30 por minuto por endereço IP; computadores novos (IDs entregues): até 10 por hora por IP;
  - ID inexistente responde igual a ID offline, para não dar para sondar quais IDs existem;
  - nomes sem caracteres de controle nem invisíveis (como os que invertem a direção do texto), com até
    64 caracteres, porque o nome do técnico aparece para quem está no host.
- **Auditoria:** a tabela `audit` registra quem pediu, para qual ID, quando e o resultado (`requested`,
  `accepted`, `accepted_unattended`, `unattended_off`, `entered`, `wrong_password`, `declined`, `timeout`,
  `cancelled`, `offline`, `busy`, `rate_limited`, `host_locked`).
- **Quem está online:** o técnico pergunta por uma lista de IDs (`presence {ids}`, até 50) e recebe os que
  estão online (`presence {online}`). Só aparecem os computadores que aceitaram esse técnico, ou em que ele
  entrou com a senha, nos últimos 90 dias (as tabelas `accepted` e `entered`). Qualquer outro ID aparece
  sempre offline, como num pedido, para não dar para sondar IDs. Até 6 perguntas por minuto por computador:
  as seguintes ficam sem resposta. Um servidor antigo não diz `aceita presence`, e o app não pergunta.
- **Contas (F5):** ver [Contas](#contas), abaixo.
- **Conexão viva:** o servidor manda ping a cada 20 s e desconecta quem fica 60 s em silêncio.

## Contas

A conta é opcional (F5, [plano](F5.md)). As chamadas vão no mesmo WebSocket, já autenticado pela chave do
computador: `account {request, call}` e a resposta `account {request, reply}`. Os tipos ficam em
`crates/signal/src/account.rs` (`AccountCall`, `AccountReply`), e o servidor em
`apps/hext-server/src/accounts.rs`. Um servidor antigo não diz `aceita account`, e o app não pergunta.

| Chamada | O que faz |
|---|---|
| `register {email, password}` | Cria a conta e manda um código de 6 dígitos para o e-mail. Responde `ok` mesmo se o e-mail já tem conta (aí o dono recebe um aviso, e a senha não muda) |
| `verify {email, code, password}` | Confirma o e-mail e define a senha (a de quem tem o código, não a de quem cadastrou primeiro). Sem isso, o login responde `not_verified` |
| `login {email, password, second?}` | Entra **neste computador**. Com 2FA, sem `second` responde `needs_second_factor`. `second` é o código do app autenticador ou um código de recuperação |
| `resume {token}` | Numa conexão nova, volta ao login anterior. O token só vale para o computador que fez o login, e cada computador tem um login só |
| `logout`, `me` | Sai neste computador; mostra a conta (e-mail e se tem 2FA) |
| `totp_setup`, `totp_enable {code}` | Liga o 2FA: o segredo e o link `otpauth://` (para o QR), depois o primeiro código, que devolve os 10 códigos de recuperação |
| `forgot_password {email}`, `reset_password {email, code, password}` | Troca a senha por um código no e-mail. Sai de todos os computadores |
| `change_password {old, new}` | Troca a senha sabendo a antiga. Sai dos outros computadores |
| `chain_get`, `chain_append {entry}` | A lista de dispositivos da conta (a cadeia assinada, abaixo) e mais uma entrada no fim. A primeira exige 2FA e é assinada pelo próprio dispositivo |
| `chain_join {device, name, proof}`, `chain_pending` | Um dispositivo pede para entrar na lista (só por si mesmo), e o que mostra o código vê os pedidos (10 minutos, até 5) |

- **Guardado:** a senha como hash Argon2id; códigos, tokens e códigos de recuperação como SHA-256 (são
  aleatórios e longos). Tabelas `accounts`, `account_codes`, `recovery_codes` e `account_sessions`.
- **Códigos:** o do e-mail vale 30 minutos e 5 tentativas; o TOTP (RFC 6238, 30 s, 6 dígitos) aceita um
  passo de diferença no relógio e cada código vale uma vez; os de recuperação valem uma vez cada.
- **Tentativas:** 10 erros por e-mail e 30 por IP em 15 minutos (`too_many_attempts`). Um acerto não conta,
  e cada e-mail enviado conta. Um e-mail desconhecido leva o mesmo tempo e a mesma resposta que uma senha
  errada.
- **Cadeia de dispositivos** (`crates/signal/src/chain.rs`): cada entrada é assinada por um dispositivo da
  conta (ou pela chave de recuperação, só para pôr ou tirar dispositivos) e cita o hash da anterior. O
  servidor guarda (`account_chain`) e confere, mas quem confia é o app e o host, que conferem tudo de novo.
  Ele não consegue forjar uma entrada, e o app percebe se ele esconder entradas que já tinha visto
  (`chain.json`, ao lado da conta).
- **Entrar na lista:** o dispositivo novo prova a sua chave com um HMAC do código de 12 caracteres (cerca de
  60 bits) que aparece no dispositivo que já está nela. O código não passa pelo servidor, e vale 10 minutos.
- **E-mail:** por enquanto vai só para o log do servidor (com um aviso ao iniciar), até a decisão 3 do
  plano (domínio e serviço de envio).

## Rodar

```sh
cargo run -p hext-server -- --listen 127.0.0.1:8080 --db hext-server.db
curl http://127.0.0.1:8080/health   # ok
```

O servidor fala HTTP simples. O TLS fica num proxy na frente, para os apps usarem `wss://`. Atrás do
proxy, use `--behind-proxy`: os limites por IP passam a usar o endereço que o proxy coloca em
`X-Forwarded-For`. Nesse caso, a porta do servidor precisa ficar acessível **só** para o proxy
(`127.0.0.1`), ou qualquer um poderia mandar um `X-Forwarded-For` falso.

### Docker

```sh
docker build -f apps/hext-server/Dockerfile -t hext-server .
docker run -d --name hext-server -p 127.0.0.1:8080:8080 -v hext-data:/data hext-server
```

A imagem já roda com `--behind-proxy`, por isso a porta é publicada só em `127.0.0.1`.

O banco fica no volume `/data` (`hext-server.db`). O backup é copiar esse arquivo.

### Na VM, atrás do Caddy

A instalação na VM (Caddy com TLS automático, `hext-server` e `iroh-relay`, que só aceita computadores
registrados) está em [deploy.md](deploy.md): um script faz tudo.

### Acesso ao relay

`POST /v1/relay-access` é o "porteiro" do `iroh-relay` (`access.http` na configuração dele). O relay manda
a chave do computador em `X-Iroh-Endpoint-Id` e o segredo em `Authorization: Bearer …`, o mesmo de
`--relay-token` ou `HEXT_RELAY_TOKEN`. A resposta é `200 true` se o computador tem um ID, `403` se não tem
e `401` com o segredo errado. Sem `--relay-token`, a rota responde `404`.
