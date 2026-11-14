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

## A segunda parte: o Mac que travava depois de minimizado

O sintoma: com a janela da sessão minimizada no Mac por um tempo, ao abrir de novo ela não responde mais (a
bolinha colorida gira), e só fechando à força.

A causa está no wgpu para Metal. Ele configura a camada da janela (`CAMetalLayer`) com
`allowsNextDrawableTimeout = false`: o `nextDrawable()`, que entrega a próxima imagem para desenhar, espera
**para sempre** se nenhuma voltar. O wgpu pula o desenho quando a janela está coberta naquele instante, mas
uma imagem enviada logo antes de minimizar pode ficar presa esperando a tela
([gfx-rs/wgpu#8309](https://github.com/gfx-rs/wgpu/issues/8309)). Com a camada configurada para guardar duas
imagens, basta isso acontecer duas vezes. Ao abrir a janela, o primeiro desenho fica preso na thread da janela,
e todas as janelas do visualizador param de vez.

Não há um Mac aqui para reproduzir. A causa vem da leitura do código do wgpu 30 (`metal/surface.rs`): é a
única espera sem fim no caminho do desenho, e encaixa com o sintoma (só depois de minimizar, sem volta).

### A correção

- **Cada sessão desenha numa thread própria** (`apps/hext/src/screen.rs`). O decodificador deixa o quadro mais
  novo e acorda essa thread direto (um `Condvar`): os quadros nem passam mais pela thread da janela. A janela só
  informa o tamanho, os botões por cima do vídeo, se está à vista e quando o sistema pede para repintar.
- **A GPU é criada nessa thread.** A superfície continua sendo feita na thread da janela (no Mac, o AppKit só
  responde lá), e o resto (`Pending::finish`: placa, dispositivo, pipelines), que leva quase um segundo, fica
  na thread de desenho. Abrir outro computador não para mais as janelas abertas.
- **No Mac, uma imagem espera a tela por no máximo 1 s.** Depois de cada `configure`, o
  `allowsNextDrawableTimeout` volta a `true`. Se a imagem não vier, o quadro é pulado (`Timeout`) e o próximo
  tenta de novo.
- **Minimizada ou coberta, a janela não desenha.** O `Occluded` do winit (Mac) e o tamanho zero (Windows
  minimizado) param o desenho. Ao voltar, o quadro mais novo é desenhado na hora.
- **Sem atraso no Mac.** A primeira correção desenhava no `RedrawRequested`, que no Mac espera a próxima pintura
  da janela. Agora a thread de desenho desenha assim que o quadro chega, nos dois sistemas.
- Um desenho acima de 100 ms vai para o log como `quadro preso por X ms no render (as janelas seguem
  respondendo)`.

Mesmo que uma imagem ainda fique presa, o pior caso agora é o vídeo parar por um segundo. A janela continua
movendo, fechando e respondendo.

## Roteiro de campo

No notebook, com o executável do PR. Feche a interface e rode o visualizador direto, para ver o
log:

```powershell
$env:HEXT_LOOP_TRACE='1'
.\hext.exe view <ID do Windows> 2>&1 | Tee-Object janela.log
```

1. Com a sessão aberta, deixe um vídeo rodando em tela cheia no computador remoto, para chegar quadro o tempo
   todo.
2. Arraste a janela do visualizador pela barra de título por uns 10 s, sem soltar. Ela deve acompanhar o mouse o
   tempo todo.
3. Maximize e restaure pelo botão da barra de título, cinco vezes seguidas. Cada clique deve responder na hora.
4. Faça a rede oscilar: troque o Wi-Fi para o roteador do celular, ou afaste o notebook do roteador até o título
   mostrar perda (fps caindo), e repita os passos 2 e 3.
5. Com uma sessão aberta, abra outro computador: a primeira janela deve continuar movendo enquanto a segunda
   aparece.
6. Depois, pela interface (sem o PowerShell), repita os passos 2 a 4 uma vez: é o uso de verdade.

No Mac, como técnico, com o app instalado. Feche a interface e rode o visualizador pelo Terminal, para ver o
log (aberto pela interface, ele não guarda log):

```sh
HEXT_LOOP_TRACE=1 "/Applications/Hext Remote.app/Contents/MacOS/HextRemote" view <ID> 2>&1 | tee janela.log
```

1. Abra uma sessão com vídeo rodando do outro lado e minimize a janela por 10 minutos. Abra de novo: a janela
   responde na hora e o vídeo volta em até 1 s.
2. Repita deixando minimizada por mais tempo (uma hora, ou com o Mac em repouso no meio).
3. Cubra a janela da sessão com outra janela por inteiro por alguns minutos e volte para ela.
4. Com a janela à vista, confira que o vídeo não ficou mais atrasado que antes (mexa o mouse no computador
   remoto e compare com a versão anterior).

Passou se nenhuma janela parou e não foi preciso fechar nada à força. No log, procure:

- `janela presa por …`: qualquer linha é um problema. Anote o que vem depois do `em`.
- `quadro preso por X ms no render`: o vídeo parou X ms, mas a janela seguiu. No Mac, perto de 1000 ms ao
  voltar de minimizada é a espera limitada fazendo o trabalho. Anote quantas vezes aparece.

Se travar, mande o `janela.log`. No Windows, lembre que o `Tee-Object` grava em UTF-16.
