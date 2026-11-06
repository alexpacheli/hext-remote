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
  1 s, impossível numa LAN limpa. Essa rodada não registrava o caminho, então a causa ficou em aberto.

Daí vieram os pedidos de todos os quadros parados de uma vez, `MAX_WAITING` em 48 com histórico de 64 quadros no
host, 3 tentativas, a fila de datagramas de 64 kB e o registro de caminho e RTT.

**Para a próxima rodada, olhe primeiro `caminho:` e `RTT`:** relay ou endereço `100.x` (Tailscale) em computadores
na mesma rede mudam a conclusão. Compare também o total de pacotes perdidos com os `no caminho em uso`: a diferença
é de caminhos testados e abandonados, não do vídeo.

## A terceira medição

Em 51 s de rede limpa: 17 de 36.181 pacotes perdidos (0,05%), nenhum quadro perdido, um único keyframe (o inicial),
nada preso no controle de congestionamento e 9,95 Mbps de pico. A imagem ficou bem melhor para quem usou.

Ela **não valida o reenvio**: nada foi reenviado, e o host rodou o build anterior, sem `caminho:` nem `RTT`. Mostra
que, sem perda, o vídeo aguenta o estresse, e que a piora da segunda rodada veio da rede, ainda sem causa conhecida.

Ficou uma queda sem relação com a rede: por cerca de 10 s (`:42` a `:51`) a `captura` teve só 7 a 11 imagens novas
por segundo, com quadros de 70 a 90 kB e 0 ms de espera pela rede.
