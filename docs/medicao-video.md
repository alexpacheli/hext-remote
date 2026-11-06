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
| `quadro N perdido cedo demais` | visualizador | esperar mais teria salvo: rever `RESEND_AFTER` e `RESEND_TRIES` |
| `imagem voltou após X ms` | visualizador | tempo parado até o keyframe, com tamanho e tempo de chegada dele |
| `fila do decodificador cheia` | visualizador | o decodificador não acompanhou |
| `chegada: …` | visualizador | por segundo: quadros, datagramas, maior intervalo e pacotes tardios |
| `envio: …` | host | por segundo: quadros, taxa, tempo enviando, tempo preso, perda real do QUIC, cwnd |
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
