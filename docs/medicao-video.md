# Medir o vídeo com a tela mudando

Como repetir a medição de minimizar, maximizar e arrastar janelas, e como comparar com o Parsec.

## Rodar

Use o **mesmo build** nos dois computadores (a versão do protocolo precisa ser igual) e feche a interface do
Hext Remote nos dois. O serviço pode continuar instalado.

No **host**, no PowerShell, na pasta do executável:

```powershell
$env:RUST_LOG='info,hext_video=debug,wgpu_core=warn,wgpu_hal=warn,naga=warn,noq_udp=error'
.\hext.exe host | Tee-Object host-video.log
```

Anote o ID. É nesse terminal que se aceita a conexão.

No **visualizador**:

```powershell
$env:RUST_LOG='info,hext_video=debug,wgpu_core=warn,wgpu_hal=warn,naga=warn,noq_udp=error'
.\hext.exe view <ID> | Tee-Object viewer-video.log
```

Com senha, acrescente `--password-stdin` e digite no terminal.

## O teste

1. Confira no título da janela que o caminho é **P2P** (ou anote se for relay).
2. 10 s com a tela parada.
3. 30 s de estresse: minimizar e maximizar a mesma janela em tela cheia, arrastar uma janela grande.
4. Feche a sessão.

Repita o mesmo roteiro com o Parsec, na mesma rede e nos mesmos computadores, anotando o que as estatísticas
dele mostram (latência, taxa, codec). A comparação é do mesmo gesto, não de números de sessões diferentes.

## Ler os logs

Sem `RUST_LOG`, só os segundos com problema vão para o log (no serviço, em `C:\ProgramData\HextRemote`).

| Linha | Lado | O que dizer dela |
| --- | --- | --- |
| `quadro N salvo por reenvio em X ms` | visualizador | a perda custou X ms e nenhum keyframe |
| `quadro N perdido por …` | visualizador | o reenvio falhou; `reenvio sem resposta` ou `MAX_WAITING` |
| `quadro N perdido cedo demais` | visualizador | esperar mais teria salvo: rever `RESEND_TRIES` e a espera por resposta |
| Log em UTF-16 | ambos | o `Tee-Object` grava assim: para usar `grep`, converta antes, ou ele não acha nada |
| `imagem voltou após X ms` | visualizador | tempo parado até o keyframe, com tamanho e tempo de chegada dele |
| `fila do decodificador cheia` | visualizador | o decodificador não acompanhou |
| `chegada: …` | visualizador | por segundo: quadros, datagramas, maior intervalo e pacotes tardios |
| `envio: …` | host | por segundo: quadros, taxa, tempo enviando, tempo preso, perda real do QUIC, cwnd, RTT |
| `caminho: …` | host | endereço do caminho em uso, ao começar e a cada troca |
| `captura: …` | host | por segundo: imagens novas, codificadas, seguradas e espera pela rede |

Para achar a causa de uma queda de fps num segundo:

- `captura` com poucas imagens novas: a tela não mudou tanto, ou a captura atrasou.
- Muitas imagens novas e poucas codificadas: o limite está no codificador ou na espera pela rede.
- `envio` com muito tempo preso no controle de congestionamento: a janela (`cwnd`) está pequena.
- `QUIC: N de M pacotes perdidos` com N alto: perda real na rede.
- `chegada` com menos quadros que `envio`: perda ou fila no visualizador.

## O que a primeira medição mostrou

Em 5/11, com o host DESKTOP-U6JQMAA (Intel Quick Sync) e o visualizador ALEX-LIN-DESKTO, em 85 s:

- **A perda era real, não declarada cedo demais:** nenhum pacote tardio, e `MAX_WAITING` nunca foi acionado. O
  maior intervalo entre pacotes de um quadro foi 34 ms.
- **O FEC cobre o miúdo, não a rajada:** 14 pacotes perdidos no estresse foram recuperados, mas uma rajada de
  21 derrubou um quadro com 15 pacotes faltando.
- **O custo vinha depois da perda:** cada uma virava um keyframe de 215 a 271 kB. Um deles levou 494 ms para
  chegar, com a janela do QUIC em 66 kB, e a imagem parou por 639 ms.
- **Havia uma cascata:** depois desse keyframe, a fila de 2 quadros do decodificador encheu e pediu outro de
  271 kB, mais 194 ms parado.
- **8 Mbps não era o gargalo:** o envio ocupou no máximo 27% de cada segundo no estresse, mas as quedas de fps
  sem perda ficaram sem explicação nessa rodada, por isso existe a linha `captura`.

Daí vieram o reenvio dos pacotes que faltam, antes de recorrer ao keyframe, e a fila maior do decodificador.

## O que a segunda medição mostrou

Mesmos computadores, com reenvio, em 54 s. As rodadas **não são comparáveis**: a perda real do QUIC subiu de
0,18% (64 de 34.747 pacotes) para 5,21% (1.815 de 34.834), e o reenvio não explica isso: em um segundo foram 248
pacotes perdidos e 39 reenviados.

- **A cascata acabou:** a fila do decodificador não encheu nenhuma vez.
- **O reenvio funciona:** 14 quadros salvos, em 16 a 636 ms, sem keyframe.
- **A perda vem em rajada:** 86% dela caiu em segundos com um quadro de 100 kB ou mais, e nas duas rodadas o
  primeiro keyframe já perdia de 3 a 6% dos pacotes.
- **Sobraram 11 quadros perdidos, com 4 s parados na soma,** por defeitos do reenvio: pedir um quadro por vez fazia
  as esperas somarem, `MAX_WAITING` em 16 desistia de quadros ainda não pedidos, e duas tentativas desistiram
  cedo de 5 quadros cujos pacotes chegaram até 175 ms depois.
- **Houve segundos parados sem perda:** 873 ms presos a 1,7 Mbps com janela de 199 kB, o que daria um RTT perto de
  1 s. Essa rodada não registrava o caminho, então a causa ficou em aberto.

Daí vieram os pedidos de todos os quadros parados de uma vez, `MAX_WAITING` em 48 com histórico de 64 quadros no
host, 3 tentativas, a fila de datagramas de 64 kB e o registro de caminho e RTT.

**Para a próxima rodada, olhe primeiro `caminho:` e `RTT`:** relay ou endereço `100.x` (Tailscale) mudam a
conclusão. Compare também o total de pacotes perdidos com os `no caminho em uso`: a diferença
é de caminhos testados e abandonados, não do vídeo.

## A terceira medição

Em 51 s de rede limpa: 17 de 36.181 pacotes perdidos (0,05%), nenhum quadro perdido, um único keyframe (o inicial),
nada preso no controle de congestionamento e 9,95 Mbps de pico. A imagem ficou bem melhor para quem usou.

Ela **não valida o reenvio**: nada foi reenviado, e o host rodou o build anterior, sem `caminho:` nem `RTT`. Mostra
que, sem perda, o vídeo aguenta o estresse.

Ficou uma queda sem relação com a rede: por cerca de 10 s (`:42` a `:51`) a `captura` teve só 7 a 11 imagens novas
por segundo, com quadros de 70 a 90 kB e 0 ms de espera pela rede.

## A quarta medição

Primeira com o build novo nos dois lados, em 57 s, com rede tão ruim quanto a da segunda rodada:

| | Rodada 2 | Rodada 4 |
| --- | --- | --- |
| Pacotes perdidos no QUIC | 5,21% | 4,33% |
| Quadros salvos por reenvio | 14 | 47 |
| Quadros perdidos | 11 | 2 |
| Tempo parado até o keyframe | 4,06 s | 1,19 s |

- **O reenvio aguenta perda real:** rajadas de 320 e 417 pacotes perdidos por segundo não viraram keyframe em cascata.
- **Os 2 quadros perdidos foram cedo demais:** os pacotes vieram 184 e 173 ms depois, de 530 a 560 ms após o último.
  Daí veio o piso de 600 ms sem pacote antes de desistir.
- **O caminho é o Tailscale:** `Ip(100.107.63.111)` do início ao fim, com RTT de 18 a 472 ms (mediana 76). O host está
  em Wi-Fi em `192.168.1.x` e o visualizador em cabo em `192.168.0.x`, sem rota entre as sub-redes.
- **Falta separar as causas:** o RTT que oscila pode vir do Wi-Fi, do túnel ou da internet entre os dois. Só outra
  rodada, com o Tailscale desligado em um dos lados, mostra qual.

## Comparar o bitrate adaptativo

O host baixa a taxa quando a rede piora e a linha `taxa:` mostra cada mudança e o motivo. Só o host muda: o visualizador
continua igual, no mesmo protocolo. Para comparar com a taxa fixa, rode o host com `$env:HEXT_TAXA_FIXA='1'`.

- `taxa: 8.0 para 5.6 Mbps (RTT subiu)`: desceu. Os motivos são `preso no controle de congestionamento`, `RTT subiu` e
  `perda alta`.
- `taxa: 1.0 para 1.1 Mbps (rede boa)`: subiu, depois de 5 segundos bons com o vídeo usando pelo menos metade da taxa.
- `não aceitou mudar a taxa`: o driver recusou. Confira também se `Mbps` em `envio:` acompanha a taxa pedida, porque um
  driver pode aceitar e ignorar.

## A sexta medição

Três rodadas seguidas no `DESKTOP-U6JQMAA`, com a mesma ação. E e G com taxa adaptativa, F com taxa fixa.

| | E: adaptativa | F: fixa | G: adaptativa |
| --- | --- | --- | --- |
| Quadros por segundo, média | 32 | 53 | 58 |
| Segundos abaixo de 30 quadros | 20 de 45 | 6 de 61 | 0 de 52 |
| Tempo parado até o keyframe | 0 | 2,88 s | 0 |
| Quadros perdidos | 0 | 3 | 0 |
| Preso no controle de congestionamento | 3,2 s | 3,8 s | 0,2 s |
| Perda no QUIC | 2,46% | 1,15% | 0,76% |
| RTT mediano e máximo | 25 e 483 ms | 16 e 48 ms | 17 e 22 ms |
| Caminho | Tailscale | IP público | Tailscale |

- **O Intel aceitou e ignorou a taxa:** nenhum `não aceitou mudar a taxa`, mas em E, com alvo abaixo de 5 Mbps, os quadros
  tinham 16,0 kB, o mesmo que 8 Mbps a 60 quadros. Em G, com alvo de 5,6 Mbps, saíram 6,3 a 9 Mbps.
- **O pacing derrubou os quadros de E:** ele acompanhava a taxa-alvo, a 4 vezes ela. Com o encoder ignorando o alvo, o
  envio bateu nesse teto (com alvo de 1,3 Mbps, ficou em 4,2 Mbps) e a captura chegou a esperar 1157 ms em um segundo.
  Foi erro nosso: a qualidade não caiu, e os quadros sim. O pacing voltou a ser fixo.
- **G não prova a taxa adaptativa:** o encoder não obedeceu e a rede estava melhor. Mesmo assim mostra perda de 129 e 270
  pacotes em um segundo sem nenhum quadro perdido.
- **Keyframe depois de perda é o que trava:** em F, um keyframe de 207 kB com janela de 28 kB deu a parada de 2,6 s.
- **Falso atraso do decodificador:** não apareceu nas 3 rodadas, mesmo com até 20 quadros passando à frente.

`hext encoders` nunca mudou taxa; para testar isso sem rede, use `hext encoder-rates`. Ele compara quatro formas de
mudar a taxa em cada codificador e mostra o que realmente saiu, quanto demorou o primeiro quadro depois da mudança e o
maior quadro.

| Com alvo de 1 Mbps | AMD (visualizador) | Intel Quick Sync (`DESKTOP-U6JQMAA`) |
| --- | --- | --- |
| Taxa média e máxima | 8,8 para 1,1 Mbps | 8,1 para 8,1 Mbps |
| Também o buffer | 8,8 para 1,1 Mbps | 8,1 para 8,1 Mbps |
| Também um keyframe | 8,8 para 1,0 Mbps | 8,1 para 8,1 Mbps |
| Reabrindo o codificador | 8,8 para 0,9 Mbps | 8,1 para 2,1 Mbps |

NVIDIA não entrou: nesse notebook ela falha ao abrir (`Falha catastrófica`) e o host cai para o Intel.

Por isso o host passou a observar a saída: dois segundos seguidos 50% acima do alvo marcam o codificador como um que
ignora a taxa. A partir daí, cada mudança de 25% ou mais abre uma sessão nova, com 5 s de intervalo. Cada sessão nova
custa um keyframe e o tempo de abrir: no AMD, 410 ms e 16 kB a 1 Mbps. O número do Intel ainda falta medir.

## A quinta medição

Quatro rodadas no mesmo dia, todas com `HEXT_VBV_MS=100`. A e B no LD-NB-01, que está na mesma rede do visualizador
(`192.168.0.x`) e com Wi-Fi muito ruim. C e D no `DESKTOP-U6JQMAA`, pelo Tailscale. Nessa rede, o Parsec também ficou
lento, segundo o usuário.

| | A: LD-NB-01, Cubic | B: LD-NB-01, BBR | C: DESKTOP, Cubic | D: DESKTOP, BBR |
| --- | --- | --- | --- | --- |
| Duração no visualizador | 57 s | 44 s | 73 s | 50 s |
| Quadros por segundo, média | 9 | 3 | 47 | 57 |
| Segundos abaixo de 30 quadros | 51 | 44 | 14 | 0 |
| Tempo parado até o keyframe | 5,88 s | 12,58 s | 1,20 s | 0 |
| Quadros salvos (mediana) | 99 (155 ms) | 43 (151 ms) | 109 (444 ms) | 21 (22 ms) |
| Perdidos por reenvio sem resposta | 4 | 1 | 0 | 0 |
| Keyframes pedidos pela fila do decodificador | 0 | 0 | 3 | 0 |
| Perda no QUIC | sem log | 1,82% | 3,32% | 0,43% |
| RTT mediano e máximo | sem log | 95 e 418 ms | 24 e 420 ms | 14 e 22 ms |

- **O BBR não está provado:** a rede de C melhorou a partir de `12:29:50`, ainda com Cubic: 58 quadros por segundo, RTT
  de 12 a 16 ms e nada preso. D rodou logo depois, nessa fase boa, e não dá para separar os dois efeitos.
- **O BBR pode travar:** em B, um keyframe de 239 kB levou 11,3 s, com a janela em 6 kB. O host mandou 349 pacotes em
  12,6 s e perdeu só 6. A perda não explica essa taxa, mas a rodada não mostra se o Wi-Fi caiu junto.
- **O buffer não limitou os quadros:** o driver aceitou 100 ms, ou 100 kB a 8 Mbps, e C teve 161 kB num segundo sem
  keyframe. É a mesma faixa de antes (100 a 160 kB).
- **Falso atraso do decodificador:** os 3 keyframes de C, de até 313 kB, vieram logo depois de rajadas de quadros
  salvos. `Backlog` guardava como mínimo o começo da rajada, então uma fila ainda drenando contava como atrasada. A
  correção veio desta rodada, mas o log não amostra a fila: ainda falta confirmar em campo.
- **Menos quadros nem sempre é rede:** em B houve segundos com 5 a 8 imagens novas e 0 ms esperando a rede. Compare a
  linha `captura:` antes de atribuir tudo ao Wi-Fi.
- **Falta um log:** `host-a.log` não chegou, então A não mostra perda, RTT nem o buffer aceito.
