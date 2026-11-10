# Monitor virtual (computador sem monitor)

**Objetivo:** atender um computador sem monitor ligado (servidor, Mac mini, PC com o monitor desligado da tomada)
sem o adaptador HDMI emulador. Quando não houver nenhum monitor para capturar, o Hext cria um monitor virtual
durante a sessão e o remove ao final.

## Situação hoje

- **Windows:** sem monitor, o DXGI não lista nenhuma saída e `open_first` falha com `NoSuchDisplay`. A sessão
  termina na hora.
- **macOS:** sem monitor, o ScreenCaptureKit não lista nenhum monitor; mesmo resultado. A 0.6.5 só resolve a tela
  **dormindo** (o monitor existe e é acordado), não a tela **ausente**.

O Parsec resolve isso com o próprio driver de monitor virtual (fechado, não podemos usar).

## Quando o monitor virtual liga

1. **Automático:** a sessão começa e `displays()` volta vazio (computador sem cabo de monitor). O Hext liga o
   monitor virtual, espera ele aparecer na lista (como a espera de `open_waking`, até 8 s) e abre a captura nele.
2. **Pelo técnico:** um monitor que perdeu o cabo pode continuar na lista do Windows até a próxima mudança
   de monitores (o spike mostrou), e o Hext não tem como saber que ninguém o vê. Por isso o visualizador tem a
   opção "Monitor virtual" na lista de monitores: liga o virtual e mostra ele.
3. Ao fim da sessão, o Hext desliga o monitor virtual. Um monitor fantasma que ficasse ligado aumentaria a área
   de trabalho e "esconderia" janelas para quem usa o computador com um monitor de verdade depois.
4. Com um monitor de verdade ligado, e sem o técnico pedir, nada muda: o virtual não é criado.

## Windows

### O driver

[Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) (MttVDD), licença MIT:

- driver IddCx (modo usuário, UMDF), Windows 10 e 11, x64 e ARM64;
- assinado pela SignPath Foundation (o mesmo programa que pedimos para o Hext), **não** pela Microsoft (sem
  WHQL/attestation);
- resoluções e frequências configuráveis (até 240 Hz), HDR no Windows 11 23H2+;
- última release: 25.7.23 (julho de 2025); o repositório segue ativo.

O que a instalação faz (script `silent-install.ps1` do próprio projeto):

1. extrai os certificados do `mttvdd.cat` e os importa em `LocalMachine\TrustedPublisher` (não em `Root`);
2. cria o dispositivo `Root\MttVDD` e instala o `MttVDD.inf` com o `nefconw` (BSD-3, do projeto nefcon).

**Pontos de atenção (verificar no spike):**

- **Certificado em TrustedPublisher:** o Windows passa a confiar em drivers assinados pela SignPath Foundation
  sem perguntar. É o que o projeto exige; precisa ser dito na tela de instalação e desfeito na desinstalação.
- **O pipe de controle do driver (`\\.\pipe\MTTVirtualDisplayPipe`) aceita qualquer usuário** (`D:(A;;GA;;;WD)`):
  qualquer programa local pode mudar a configuração. Por isso o plano **não usa o pipe** (veja abaixo).
- **Secure Boot / HVCI:** confirmar que o driver carrega com Secure Boot e com a Integridade de Memória ligada,
  num Windows 11 24H2 limpo. No ARM64 24H2 o projeto avisa que pode exigir test signing: ARM64 fica de fora até
  isso mudar.

### Como o Hext controla

Sem o pipe: o agent (SYSTEM, na sessão do usuário) **habilita e desabilita o dispositivo** com a SetupAPI
(`DIF_PROPERTYCHANGE` com `DICS_ENABLE`/`DICS_DISABLE`, o mesmo que o Gerenciador de Dispositivos faz). Código em
`crates/capture/src/virtual_display.rs`.

- **Configuração:** ao iniciar, o serviço grava o `vdd_settings.xml` do Hext em `C:\VirtualDisplayDriver\`, se o
  driver estiver instalado e não houver arquivo lá: **1 monitor**, 1920×1080 primeiro (o modo em que ele nasce),
  1280×720, 1600×900 e 2560×1440, a 60 Hz, com **cursor por hardware** (sem ele aparecem dois cursores). A pasta é
  criada fechada: só o sistema e os administradores alteram; o driver e os usuários só leem. Um arquivo que já
  esteja lá fica (quem usa o driver para outra coisa mantém a configuração).
- Dispositivo **desabilitado** por padrão. Quem ligou desliga: o Hext só desliga o que ele mesmo ligou; um monitor
  virtual que o dono do computador deixa ligado continua ligado.
- Ao ligar, o Hext grava a marca `%ProgramData%\HextRemote\monitor-virtual-ligado`; ao desligar, apaga. Se o
  serviço ou o agent cair no meio da sessão, o serviço desliga ao reiniciar (a marca diz que foi o Hext), e um
  agent novo que encontra o virtual ligado com a marca também o desliga ao fim da sessão.
- O Windows dá um nome novo ao monitor a cada habilitação (`\\.\DISPLAY46`, 47…): o Hext o reconhece pelo
  dispositivo (`MTT1337` no ID do monitor), nunca pelo nome.
- A resolução: a captura já acompanha a troca de resolução; o técnico escolhe pela própria configuração de vídeo
  do Windows na sessão (primeira versão), e depois por um menu no visualizador (`ChangeDisplaySettingsEx` no
  agent).
- Tela de login e UAC: o monitor virtual é um monitor de verdade para o Windows, então aparece também no secure
  desktop (confirmar no spike).

### Instalação

- **No MSI, como opção** (desmarcada por padrão): "Monitor virtual para computadores sem tela". Um instalador
  menor para quem não precisa, e nada de certificado novo sem a pessoa escolher.
- Também por linha de comando: `HextRemote.exe service virtual-display install|remove`, para instalar depois em
  servidores.
- Os arquivos do driver vão **dentro** do MSI, com hash conferido no build (`release.yml` baixa a versão fixada
  e confere o SHA-256). Nada baixado da internet na máquina do cliente.
- Desinstalar o Hext remove o dispositivo, o driver (`pnputil /delete-driver … /uninstall`) e os certificados
  que ele inseriu.

## macOS

Sem driver: a API `CGVirtualDisplay` (CoreGraphics) cria um monitor virtual pelo próprio processo, sem instalar
nada. É o que DeskPad, BetterDisplay e outros usam.

- **Não documentada pela Apple** (API privada): pode mudar numa versão do macOS. Mitigação: carregar as classes
  em tempo de execução (`NSClassFromString`) e, se não existirem, seguir sem monitor virtual com uma mensagem
  clara ("este macOS não permite monitor virtual; use um adaptador HDMI").
- O monitor vive enquanto o objeto existir: criado pelo app que roda na sessão do usuário (o mesmo que captura),
  liberado no fim da sessão.
- Classes: `CGVirtualDisplayDescriptor` (nome, tamanho físico, resolução máxima), `CGVirtualDisplay`,
  `CGVirtualDisplaySettings` e `CGVirtualDisplayMode` (resoluções e HiDPI).
- O ID que ele devolve é um `CGDirectDisplayID`, o mesmo que o ScreenCaptureKit usa: a captura abre sem mudança.
- Não impede distribuição: o Hext não vai para a Mac App Store.
- **Requisito à parte:** um Mac sem monitor precisa estar com sessão iniciada (login automático) para o app
  rodar e capturar. Isso já vale hoje.

## Linux

Fica para a F7, com a interface gráfica (no X11, `xf86-video-dummy` ou `xrandr --setmonitor`; no Wayland,
depende do compositor).

## Marcos

| # | Marco | Pronto quando |
|---|---|---|
| 0 | **Spike** | Num Windows 11 24H2 limpo, com Secure Boot e Integridade de Memória: o MttVDD instala em silêncio, o serviço habilita e desabilita o dispositivo, o DXGI captura o monitor virtual, inclusive na tela de login. No Mac: um monitor `CGVirtualDisplay` criado por um programa de teste aparece no ScreenCaptureKit, num Mac sem monitor. Relatório em `docs/` |
| 1 | **Windows: liga e desliga na sessão** | Sessão num PC sem monitor (com o driver instalado à mão) abre com imagem em 1920×1080 (pelo `vdd_settings.xml` do Hext, com cursor por hardware); "Monitor virtual" na lista do visualizador liga o virtual num PC com monitores desligados no botão; ao fim, o dispositivo fica desabilitado; o serviço reiniciado no meio desabilita |
| 2 | **Windows: instalação** | Opção no MSI e `service virtual-display install/remove`; desinstalar remove driver e certificados; release confere o hash do driver |
| 3 | **macOS** | Sessão num Mac mini sem monitor abre com imagem; sem a API, mensagem clara em vez de sessão fechando |
| 4 | **Resolução pelo visualizador** | O técnico escolhe a resolução do monitor virtual num menu |

Ordem: 0 → 1 → 2 e 3 em paralelo → 4. Testes manuais entram no roteiro de `docs/testes.md`.

Fica para depois do marco 1: o visualizador lembrar o último monitor escolhido para cada computador (com o
virtual ligado, a sessão já começa nele, o que resolve o caso dos monitores fantasmas).

## Marco 1: como funciona e roteiro

**No visualizador:** quando o computador do outro lado tem o driver, a barra de monitores (ao lado da alça,
também em tela cheia) mostra um botão **+**: "Monitor virtual". Ele liga o virtual e mostra ele; depois, o virtual
vira um número na barra. Atalho: **Ctrl+Alt+0**. Ligar o virtual muda os monitores do computador, então só aparece
com permissão de controle. O título diz "monitor 3 de 3 (virtual)" (fora da tela cheia).

**Sozinho:** a sessão começa, o Windows não lista nenhum monitor, e o Hext liga o virtual e abre nele. Com o
virtual ligado (por ele ou pelo técnico), a sessão começa nele.

**Roteiro** (PC com o driver instalado à mão, como no spike, e o Hext de teste instalado como serviço):

1. Apague o arquivo de exemplo do spike (`C:\VirtualDisplayDriver\vdd_settings.xml`) e a pasta, e desabilite o
   virtual (`vdd-spike.exe disable`). Reinicie o serviço (`Restart-Service HextRemote`). No `service.log`:
   "monitor virtual: configuração gravada em C:\VirtualDisplayDriver\vdd_settings.xml".
2. Com os monitores ligados: conecte. A barra mostra os números e o **+**. Clique no **+** (ou Ctrl+Alt+0): a
   imagem passa para o virtual em 1920×1080, com um cursor só. No `agent.log`: "monitor virtual: ligado".
3. Encerre a sessão: "monitor virtual: desligado"; `vdd-spike.exe status` mostra o dispositivo desabilitado.
4. Sem os cabos dos monitores (depois que o Windows tirar os fantasmas da lista): conecte. A sessão abre direto
   no virtual ("nenhum monitor: ligando o monitor virtual"). Encerre: ele desliga.
5. Com os monitores fantasmas na lista (logo depois de tirar os cabos): conecte, a imagem fica preta; Ctrl+Alt+0
   liga o virtual e a imagem volta.
6. Queda no meio: com o virtual ligado pela sessão, `Stop-Process -Name HextRemote -Force` (o serviço e o agent
   caem) e `Start-Service HextRemote`. No `service.log`: "monitor virtual: desligado (ficou ligado de uma sessão
   anterior)".
7. Sem permissão de controle (só ver): o **+** não aparece e Ctrl+Alt+0 vai para o computador remoto.
8. Virtual ligado pelo dono (`vdd-spike.exe enable` antes de conectar, com os monitores ligados): a sessão abre
   no monitor principal, e o virtual é só mais um número na barra; no fim da sessão, ele continua ligado.

O **+** só aparece com o Hext instalado como serviço (é o agent, como SYSTEM, que pode ligar o dispositivo, e a
pasta do serviço guarda a marca); o app avulso não oferece o monitor virtual. Um dispositivo com outro problema
além de "desabilitado" (código 10, por exemplo) não é ligado: o `agent.log` diz o código.

## Marco 3: macOS

Feito junto com o marco 1 (o spike passou no macOS 27.2: o monitor apareceu no ScreenCaptureKit em 0,5 s, e o
visualizador o mostrou como mais um monitor). O app que captura (o que abre no login) cria o monitor com
`CGVirtualDisplay`: 1920×1080 (também 1600×900 e 1280×720, nos Ajustes do Mac), 24", sem HiDPI, fornecedor
`0x4858` ("HX"), pelo qual o Hext o reconhece na lista. Ele existe enquanto o app o mantém: some no fim da sessão e
some se o app fechar ou cair (não há marca em disco). Nada a instalar: o **+** aparece em qualquer Mac com a API.

Sem a API (um macOS que a removeu) ou sem monitor nenhum, o técnico recebe o motivo ("captura indisponível: …"),
em vez de "capture unavailable".

**Roteiro** (Mac com o Hext de teste, login automático ligado):

1. Com os monitores ligados: conecte. A barra mostra os números e o **+**. Clique no **+** (ou Ctrl+Alt+0): a
   imagem passa para o virtual em 1920×1080. No `ui.log`: "monitor virtual: ligado".
2. Encerre a sessão: "monitor virtual: desligado"; o monitor some dos Ajustes do Mac > Monitores.
3. Sem monitor (cabos desligados): conecte. A sessão abre direto no virtual. Encerre: ele some.
4. Queda no meio: com o virtual ligado, feche o Hext pelo Monitor de Atividade: o monitor some junto.
5. Sem permissão de controle: o **+** não aparece.

## Testes automáticos

- Em qual monitor a sessão começa (o pedido, o virtual ligado, o principal), o que o visualizador fica sabendo
  (sem virtual, desligado, ligado como monitor N) e que o único monitor não desliga: testes em `host.rs`.
- O protocolo entre versões (host antigo não manda nada; visualizador antigo ignora o campo novo): teste em
  `crates/proto`.
- O CI não tem GPU nem driver: a parte que liga o dispositivo e a marca do serviço são testadas no spike e no
  roteiro manual.
