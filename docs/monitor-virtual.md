# Monitor virtual (computador sem monitor)

**Objetivo:** atender um computador sem monitor ligado (servidor, Mac mini, PC com o monitor desligado da tomada)
sem o adaptador HDMI emulador. Quando não houver nenhum monitor para capturar, o Hext cria um monitor virtual
durante a sessão e o remove ao final.

## Situação

Os marcos 0 a 4 (abaixo) estão prontos e publicados desde a 0.7.0. O monitor virtual já funcionou num Windows e
num Mac reais sem monitor; faltam os roteiros de resolução, de monitor fantasma e de desinstalação em máquinas
reais.

Antes dele, até a 0.6.x:

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
- assinado pela SignPath Foundation, **não** pela Microsoft (sem WHQL/attestation);
- resoluções e frequências configuráveis (até 240 Hz), HDR no Windows 11 23H2+;
- última release: 25.7.23 (julho de 2025); o repositório segue ativo.

O que a instalação faz (script `silent-install.ps1` do próprio projeto, usado no spike):

1. extrai os certificados do `mttvdd.cat` e os importa em `LocalMachine\TrustedPublisher` (não em `Root`);
2. cria o dispositivo `Root\MttVDD` e instala o `MttVDD.inf` com o `nefconw` (BSD-3, do projeto nefcon).

O Hext faz o mesmo sem o script nem o nefcon (veja o marco 2), e insere só o certificado que assina o catálogo.

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
  do Windows na sessão (primeira versão), e, desde o marco 4, pelo visualizador com Ctrl+Alt+0 (`ChangeDisplaySettingsEx` no
  agent).
- Tela de login e UAC: o monitor virtual é um monitor de verdade para o Windows, então aparece também no secure
  desktop (falta confirmar num computador real).

### Instalação

- **No MSI, como opção** (desmarcada por padrão): "Monitor virtual para computadores sem tela". Um instalador
  menor para quem não precisa, e nada de certificado novo sem a pessoa escolher.
- Também por linha de comando: `HextRemote.exe service virtual-display install|remove`, para instalar depois em
  servidores.
- Os arquivos do driver vão **dentro** do MSI, com hash conferido no build (`release.yml` baixa a versão fixada
  e confere o SHA-256). Nada baixado da internet na máquina do cliente.
- Desinstalar o Hext remove o dispositivo, o driver (`pnputil /delete-driver … /uninstall`) e os certificados
  que ele inseriu.
- Só x64: o ARM64 fica de fora (veja acima). Como foi feito: marco 2, abaixo.

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

Fica para a F7, com o host no Linux (ser acessado; a interface de técnico já existe desde a 0.8.0). No X11,
`xf86-video-dummy` ou `xrandr --setmonitor`; no Wayland, depende do compositor.

## Marcos

| # | Marco | Pronto quando |
|---|---|---|
| 0 | **Spike** ✅ | Num Windows 11 24H2 limpo, com Secure Boot e Integridade de Memória: o MttVDD instala em silêncio, o serviço habilita e desabilita o dispositivo, o DXGI captura o monitor virtual, inclusive na tela de login. No Mac: um monitor `CGVirtualDisplay` criado por um programa de teste aparece no ScreenCaptureKit, num Mac sem monitor. Relatório em `docs/` |
| 1 | **Windows: liga e desliga na sessão** ✅ | Sessão num PC sem monitor (com o driver instalado à mão) abre com imagem em 1920×1080 (pelo `vdd_settings.xml` do Hext, com cursor por hardware); "Monitor virtual" na lista do visualizador liga o virtual num PC com monitores desligados no botão; ao fim, o dispositivo fica desabilitado; o serviço reiniciado no meio desabilita |
| 2 | **Windows: instalação** ✅ | Opção no MSI e `service virtual-display install/remove`; desinstalar remove driver e certificados; release confere o hash do driver |
| 3 | **macOS** ✅ | Sessão num Mac mini sem monitor abre com imagem; sem a API, mensagem clara em vez de sessão fechando |
| 4 | **Resolução pelo visualizador** ✅ | O técnico troca a resolução do monitor virtual (Ctrl+Alt+0) |

Ordem: 0 → 1 → 2 e 3 em paralelo → 4. Os roteiros manuais ficam em cada marco, abaixo; o [testes.md](roteiros/testes.md)
aponta para eles.

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

## Marco 2: instalação no Windows

**O driver no MSI:** o `release.yml` baixa a release fixada (25.7.23,
`VirtualDisplayDriver-x86.Driver.Only.zip`: o nome diz x86, mas é o build x64, o mesmo que o script do projeto
instala; o build confere que a DLL é x64), confere o SHA-256
(`e24210692b442b39af763536330ce78b423f19342b7a7792c26de3944e418b3a`, o mesmo que o GitHub mostra) e inclui
`MttVDD.inf`, `MttVDD.dll`, `mttvdd.cat` e a licença MIT do projeto (`installer/windows/driver/LICENSE.txt`)
dentro do MSI, em `C:\Program Files\Hext Remote\driver\`. Os arquivos vão sempre (são 270 KB, e assim o
`service virtual-display install` funciona depois, sem reinstalar o MSI); a opção decide se o driver é instalado. Nada é
baixado na máquina do cliente. Para trocar de versão: a URL e o hash no `release.yml` e, se o certificado mudar,
o `SIGNER` em `apps/hext/src/service/windows/virtual_display.rs`.

**A opção:** o MSI agora tem telas (boas-vindas e escolha de recursos, sem a página de licença). A opção
"Monitor virtual para computadores sem tela" vem desmarcada e explica que o certificado da SignPath Foundation
vai para Fornecedores Confiáveis. Marcada, o MSI roda `HextRemote.exe service virtual-display install` como
sistema, depois do `service install`. Se o driver falhar, o Hext instala do mesmo jeito, e o motivo fica no
`service.log`. Sem telas: `msiexec /i HextRemote.msi ADDLOCAL=ALL /qn`.

**`service virtual-display install`** (administrador, com o serviço instalado), em Rust, sem nefcon nem devcon:

1. Confere os arquivos de `C:\Program Files\Hext Remote\driver\` (do sistema ou dos administradores, sem
   outra conta que possa alterar).
2. Extrai do `mttvdd.cat` o certificado da SignPath Foundation, conferido pela impressão digital
   (`3CF8CF26D8BA266C3A483AB7D26D4A818E317D76`, válido até 9/7/2027), e o insere em
   `LocalMachine\TrustedPublisher`, se ainda não estiver lá. Só esse: os certificados da GlobalSign que
   vêm junto no catálogo (intermediário e raiz) não entram.
3. Cria o dispositivo `Root\MttVDD` (SetupAPI, `DIF_REGISTERDEVICE`), grava o `vdd_settings.xml` do Hext e
   instala o INF nele (`UpdateDriverForPlugAndPlayDevices`, da `newdev.dll` do System32).
4. Desabilita o dispositivo: o Hext só o liga durante as sessões (marco 1).

O que ele inseriu fica em `%ProgramData%\HextRemote\monitor-virtual-instalado.txt` (o certificado, se não
estava lá antes, e o `oemNN.inf` do driver). Se um passo falha, ele desfaz os anteriores. Se já existe um
dispositivo do driver (instalado à mão, como no spike), ele não mexe em nada.

**`service virtual-display remove`:** remove os dispositivos `Root\MttVDD` e os drivers deles do repositório
(`pnputil /delete-driver oemNN.inf /uninstall`), os certificados que a instalação inseriu (um que já estava
lá fica) e apaga o `vdd_settings.xml` se for o do Hext. Desmarcar a opção no MSI roda o mesmo; desinstalar o Hext
também (o `service uninstall` remove o monitor virtual se foi o Hext que o instalou).

O `service status` mostra se o monitor virtual está instalado.

**Roteiro** (num PC sem o driver; o do spike já tem: rode antes `HextRemote.exe service virtual-display remove`
num terminal de administrador e remova os três certificados que o script do spike inseriu, em `certlm.msc` >
Fornecedores Confiáveis):

1. Instale o MSI e, na tela de recursos, marque "Monitor virtual para computadores sem tela". Ao fim, o
   Gerenciador de Dispositivos mostra "Virtual Display Driver" em Adaptadores de vídeo, **desabilitado**;
   `HextRemote.exe service status` diz "Monitor virtual: instalado"; `certlm.msc` mostra "SignPath Foundation"
   em Fornecedores Confiáveis; existe `C:\VirtualDisplayDriver\vdd_settings.xml` do Hext.
2. Conecte e clique no **+** (ou Ctrl+Alt+0): o virtual liga em 1920×1080 com um cursor só. Encerre: ele volta
   a ficar desabilitado (como no marco 1).
3. Num terminal de administrador: `& "C:\Program Files\Hext Remote\HextRemote.exe" service virtual-display remove`.
   O dispositivo some do Gerenciador de Dispositivos, `pnputil /enum-drivers` não lista mais `mttvdd.inf`, o
   certificado sai de Fornecedores Confiáveis e a pasta `C:\VirtualDisplayDriver` some. O **+** deixa de aparecer.
4. `& "C:\Program Files\Hext Remote\HextRemote.exe" service virtual-display install`: volta como no passo 1.
5. Desinstale o Hext (Configurações > Aplicativos): o dispositivo, o driver no repositório (`pnputil
   /enum-drivers`) e o certificado somem, como no passo 3.
6. Instale o MSI sem marcar a opção: nada do driver é instalado (só os arquivos em `driver\`), nenhum
   certificado novo.
7. Atualização: com a opção instalada, instale um MSI de versão mais nova. O monitor virtual continua instalado
   (sem duplicar o dispositivo).

O CI (`release.yml`) faz parte disso numa máquina do GitHub: instala o MSI com `ADDLOCAL=ALL`, confere o
dispositivo desabilitado e o certificado, desinstala e confere que nada ficou. É informativo (uma máquina do CI
sem o que o driver precisa não segura a release).

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

## Marco 4: resolução pelo visualizador

O visualizador não tem menu: com o monitor virtual na tela, **Ctrl+Alt+0** (ou um clique no número dele na barra)
passa para a próxima resolução, da menor para a maior e de volta à menor. O título mostra a atual: "monitor 3 de 3
(virtual, 1920×1080; Ctrl+Alt+0 muda a resolução)".

- Windows: 1280×720, 1600×900, 1920×1080 e 2560×1440 (as do `vdd_settings.xml`), pelo `ChangeDisplaySettingsEx`
  no agent. O Windows guarda a escolha: na próxima vez, o virtual liga na última resolução.
- Mac: 1280×720, 1600×900 e 1920×1080, pelo `CGDisplaySetDisplayMode`. Cada sessão começa em 1920×1080.
- Só muda o virtual que a sessão ligou: o do dono do computador fica como está (e o título não oferece).
- Precisa de permissão de controle, como ligar e desligar.

**Roteiro** (Windows e Mac): com o virtual na tela, Ctrl+Alt+0 três vezes. A imagem passa por cada resolução, o
mouse continua acertando onde aponta, e o título acompanha. No log: "monitor virtual: 1600x900".

## Testes automáticos

- Em qual monitor a sessão começa (o pedido, o virtual ligado, o principal), o que o visualizador fica sabendo
  (sem virtual, desligado, ligado como monitor N) e que o único monitor não desliga: testes em `host.rs`.
- O protocolo entre versões (host antigo não manda nada; visualizador antigo ignora o campo novo): teste em
  `crates/proto`.
- O CI não tem GPU nem driver: a parte que liga o dispositivo e a marca do serviço são testadas no spike e no
  roteiro manual.
- A instalação (marco 2): o registro do que foi inserido (lido de volta, linhas desconhecidas ou inválidas
  ignoradas, só `oemNN.inf` chega ao `pnputil`) em `service/windows/virtual_display.rs`; a instalação de verdade,
  no teste de instalação do `release.yml` e no roteiro.
