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

1. A sessão começa e `displays()` volta vazio (ou só com monitores desligados).
2. O Hext liga o monitor virtual, espera ele aparecer na lista (como a espera de `open_waking`, até 8 s) e abre a
   captura nele.
3. Ao fim da sessão, o Hext desliga o monitor virtual. Um monitor fantasma que ficasse ligado aumentaria a área
   de trabalho e "esconderia" janelas para quem usa o computador com um monitor de verdade depois.
4. Com um monitor de verdade ligado, nada muda: o virtual não é criado.

Opção futura: o técnico liga um monitor virtual extra mesmo com monitor real (por exemplo, para ter uma segunda
tela na sessão). Fica fora deste plano.

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

Sem o pipe: o serviço (SYSTEM) **habilita e desabilita o dispositivo** com a SetupAPI (`SetupDiChangeState` com
`DICS_ENABLE`/`DICS_DISABLE`, o mesmo que o Gerenciador de Dispositivos faz).

- Instalado com `vdd_settings.xml` fixo: **1 monitor**, resoluções comuns (1280×720, 1600×900, 1920×1080,
  2560×1440) a 60 Hz; o arquivo em `C:\VirtualDisplayDriver\` com permissão só para administradores.
- Dispositivo **desabilitado** por padrão. No início de uma sessão sem monitor, o serviço o habilita; no fim,
  desabilita. Se o serviço cair no meio, ao reiniciar ele desabilita (estado guardado em arquivo).
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
| 1 | **Windows: liga e desliga na sessão** | Sessão num PC sem monitor (com o driver instalado à mão) abre com imagem em 1920×1080; ao fim, o dispositivo fica desabilitado; o serviço reiniciado no meio desabilita |
| 2 | **Windows: instalação** | Opção no MSI e `service virtual-display install/remove`; desinstalar remove driver e certificados; release confere o hash do driver |
| 3 | **macOS** | Sessão num Mac mini sem monitor abre com imagem; sem a API, mensagem clara em vez de sessão fechando |
| 4 | **Resolução pelo visualizador** | O técnico escolhe a resolução do monitor virtual num menu |

Ordem: 0 → 1 → 2 e 3 em paralelo → 4. Testes manuais entram no roteiro de `docs/testes.md`.

## Testes automáticos

- A decisão "precisa de monitor virtual?" (lista vazia, só desligados, com monitor real) como função pura, com
  testes.
- O estado guardado do serviço (habilitou, caiu, reiniciou → desabilita) com testes.
- O CI não tem GPU nem driver: a parte que liga o dispositivo é testada no spike e no roteiro manual.
