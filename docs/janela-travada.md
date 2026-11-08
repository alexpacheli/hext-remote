# Janela do visualizador travada

O sintoma: durante uma sessão pela internet, a janela às vezes para de responder. Não move, não maximiza,
parece congelada, e depois de um tempo volta sozinha.

## A causa

Todas as janelas do visualizador rodam numa thread só, a do winit. Enquanto ela está ocupada, nenhuma janela
move, muda de tamanho ou repinta.

Até aqui, cada quadro decodificado mandava um aviso para essa thread (`EventLoopProxy::send_event`). No Windows,
cada aviso vira uma mensagem *postada* na fila da janela, e o Windows entrega as mensagens postadas **antes**
do mouse e do teclado. Clicar na barra de título, arrastar e maximizar são entradas do mouse: ficam esperando
a fila de quadros esvaziar.

Cada aviso custava um `render`, e o `render` pode esperar a tela (o vsync, no `get_current_texture` do wgpu). Num
notebook isso chega perto de um quadro inteiro (16 ms a 60 Hz). Quando os quadros chegam mais rápido do que a
janela desenha, a fila só cresce. Isso acontece numa rajada depois de a rede segurar pacotes, ou com a tela do
outro lado mexendo muito. Quando os quadros param (tela parada do outro lado, ou a rede melhora), a fila
esvazia e a janela volta.

Então mexer na janela não depende da internet. Mas a internet oscilando produz exatamente as rajadas que
enchem a fila.

## A medição

`HEXT_LOOP_TRACE=1` faz o visualizador escrever, a cada segundo, quanto tempo a thread da janela passou em
cada chamada (`loop da janela: …`). `fila (quadro)` é quanto o aviso de um quadro esperou na fila: o mouse
espera pelo menos isso. Host com a imagem de teste e visualizador na mesma máquina, com o `render` atrasado
de propósito para imitar um notebook esperando o vsync:

| Build | `render` | `fila (quadro)` máxima |
| --- | --- | --- |
| antes | normal (~1 ms, RX 550X) | 0,4 ms |
| antes | 12 ms | 45 ms logo depois de uma rajada (processo parado 2 s) |
| antes | 17 ms | crescendo sem parar: 250 → 300 → … → 540 ms em 5 s |
| depois | 17 ms | 18 ms (um `render`), também logo depois da rajada |

Também apareceu: ao abrir uma sessão, a thread da janela fica de 0,8 a 1,2 s presa criando a GPU
(`Renderer::new`, `sessão aberta` no log). Com vários computadores abertos, as outras janelas param nesse
intervalo. Isso fica para outro PR.

## A correção

- **Um aviso por vez.** O decodificador deixa o quadro mais novo em `Pictures` e só avisa a janela se ela ainda
  não foi avisada. A janela zera o aviso quando pega o quadro. Numa rajada de cem quadros, a fila recebe uma
  mensagem, e a janela desenha o mais novo.
- **Desenhar quando a janela pinta.** O aviso só pede `request_redraw()`. O desenho acontece no
  `RedrawRequested`, que no Windows é o `WM_PAINT`: a mensagem de menor prioridade, que vem depois do mouse e do
  teclado e que o próprio Windows junta. Com a fila vazia, sai na hora. No Mac, o desenho espera a próxima
  pintura da janela, que pode somar até um quadro de atraso: vale conferir no teste do marco 9.
- **Linhas para a interface por outra thread.** `tell_ui` escrevia no stdout direto da thread da janela. Se a
  interface demorasse a ler o pipe, todas as janelas paravam ali. Agora uma thread própria escreve. Ao sair, o
  visualizador espera até 2 s por ela, para não perder as últimas linhas.
- **Qualquer chamada acima de 100 ms vai para o log**, mesmo sem `HEXT_LOOP_TRACE`:
  `janela presa por X ms em <o quê>`.

## Roteiro de campo

No notebook (Guarulhos), com o executável do PR. Feche a interface e rode o visualizador direto, para ver o
log:

```powershell
$env:HEXT_LOOP_TRACE='1'
.\hext.exe view <ID do Windows de Sumaré> 2>&1 | Tee-Object janela.log
```

1. Com a sessão aberta, deixe um vídeo rodando em tela cheia no computador remoto, para chegar quadro o tempo
   todo.
2. Arraste a janela do visualizador pela barra de título por uns 10 s, sem soltar. Ela deve acompanhar o mouse o
   tempo todo.
3. Maximize e restaure pelo botão da barra de título, cinco vezes seguidas. Cada clique deve responder na hora.
4. Faça a rede oscilar: troque o Wi-Fi para o roteador do celular, ou afaste o notebook do roteador até o título
   mostrar perda (fps caindo), e repita os passos 2 e 3.
5. Repita com o Mac de Valinhos, e depois com os dois abertos ao mesmo tempo (marco 11), mexendo numa janela
   enquanto a outra recebe vídeo.
6. Depois, pela interface (sem o PowerShell), repita os passos 2 a 4 uma vez: é o uso de verdade.

Passou se a janela nunca parou. No log, procure:

- `janela presa por …`: qualquer linha fora da abertura da sessão (`sessão aberta`) é um problema novo. Anote o
  que vem depois do `em`.
- `fila (quadro): esperou X ms pela janela`: um aviso de quadro ficou X ms na fila. É o mesmo problema de
  antes, se aparecer sem um `janela presa` logo antes.
- `fila (quadro)` nas linhas `loop da janela`: o máximo deve ficar perto do `render` do mesmo segundo, nunca
  centenas de ms.

Se travar, mande o `janela.log`. Lembre que o `Tee-Object` grava em UTF-16.
