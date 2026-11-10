# Spike do monitor virtual: roteiro

Marco 0 de [monitor-virtual.md](monitor-virtual.md). Responde: o driver instala e carrega com a segurança do
Windows ligada? O Hext captura o monitor virtual, inclusive na tela de login? No Mac, o ScreenCaptureKit enxerga
um `CGVirtualDisplay`?

Ferramentas (branch `spike/monitor-virtual`, compiladas pelo workflow "Spike monitor virtual"):

- **Windows:** `vdd-spike.exe` (já em `Documentos\HextRemote-monitor-virtual\`): `status`, `enable`, `disable`,
  `capture [N]`, `cycle [N]`. Liga e desliga o dispositivo como o serviço faria (SetupAPI), sem o pipe do driver.
- **Mac:** `vd`: cria um monitor virtual de 1920×1080, confere o ScreenCaptureKit, salva `vd-captura.png`, mantém
  ligado alguns segundos e remove.

## Windows

Num PC de teste, se tiver: a instalação insere um certificado da SignPath Foundation em "Fornecedores
Confiáveis". O passo 6 remove tudo.

**1. Segurança ligada?** No PowerShell como administrador:

```powershell
Confirm-SecureBootUEFI
(Get-CimInstance -Namespace root\Microsoft\Windows\DeviceGuard -ClassName Win32_DeviceGuard).SecurityServicesRunning
winver
```

Anote: Secure Boot `True`? A lista do DeviceGuard contém `2` (Integridade de Memória ligada)? Versão do Windows
(24H2 ou mais nova)?

**2. Instalar o driver** com o script do próprio projeto (baixa o nefcon e o driver 25.7.23, insere o
certificado e instala), no PowerShell como administrador:

```powershell
iwr "https://raw.githubusercontent.com/VirtualDrivers/Virtual-Display-Driver/master/Community%20Scripts/silent-install.ps1" -OutFile "$env:TEMP\vdd-install.ps1"; powershell -ExecutionPolicy Bypass -File "$env:TEMP\vdd-install.ps1"
```

Logo depois, um monitor novo aparece (a área de trabalho aumenta). Anote se apareceu algum aviso do Windows.

**3. A ferramenta**, no PowerShell como administrador, na pasta `Documentos\HextRemote-monitor-virtual`:

```powershell
.\vdd-spike.exe status
.\vdd-spike.exe disable
.\vdd-spike.exe enable
.\vdd-spike.exe capture
.\vdd-spike.exe cycle 5
.\vdd-spike.exe enable
.\vdd-spike.exe resolution 2 1920x1080
.\vdd-spike.exe capture
```

Anote: o tempo de "monitor apareceu no DXGI" e se os 5 ciclos passam. Se `capture` disser "SEM IMAGEM", arraste
uma janela para o monitor virtual e repita. (`2` é a posição do monitor virtual na lista do `status`; `modes 2`
mostra as resoluções que ele aceita.)

**4. Sem monitor, com o Hext 0.6.5** (o caso real):

1. `.\vdd-spike.exe enable`, `.\vdd-spike.exe resolution 2 1920x1080` e desligue o cabo do monitor físico.
2. Do Mac, conecte com o Hext: a imagem aparece? Em que resolução?
3. Na sessão, `Win+L`: aparece a tela de bloqueio? Entre com a senha pela sessão.
4. `Ctrl+Alt+Del` pelo menu do visualizador: aparece?

**5. Depois de reiniciar, sem monitor:** com o dispositivo habilitado e o cabo desligado, reinicie o PC. Do Mac,
conecte: aparece a tela de login?

**6. Desfazer tudo** (PowerShell como administrador):

```powershell
.\vdd-spike.exe disable
pnputil /enum-drivers | Select-String -Context 1,6 "mttvdd"
```

Com o "Nome Publicado" que aparecer (`oemNN.inf`):

```powershell
pnputil /delete-driver oemNN.inf /uninstall /force
Get-ChildItem Cert:\LocalMachine\TrustedPublisher | Where-Object Subject -like "*SignPath*"
```

Confira a lista e, se só tiver o certificado da SignPath:

```powershell
Get-ChildItem Cert:\LocalMachine\TrustedPublisher | Where-Object Subject -like "*SignPath*" | Remove-Item
```

## Mac

**1. Compilar** (precisa das ferramentas de linha de comando: `xcode-select --install`, se pedir), no Terminal:

```bash
mkdir -p ~/hext-vd && cd ~/hext-vd
curl -fsSLO https://raw.githubusercontent.com/alexpacheli/hext-remote/spike/monitor-virtual/apps/vdd-spike/macos/vd.m
clang -fobjc-arc -framework Foundation -framework CoreGraphics -framework ScreenCaptureKit -framework ImageIO -framework UniformTypeIdentifiers vd.m -o vd
```

**2. Rodar:** `./vd 120`. Na primeira vez, dê ao Terminal a permissão de Gravação de Tela e rode de novo.

Anote: "apareceu no ScreenCaptureKit em … s"? O `vd-captura.png` abre (fundo de tela do monitor virtual)? "depois
de remover: removido"?

**3. Com o Hext:** durante os 120 s, conecte do PC: o monitor virtual aparece na lista de monitores do
visualizador? Troque para ele: a imagem aparece?

**4. Sem monitor (opcional, se der para acessar o Mac por SSH):** desligue o monitor do Mac, entre por SSH
(`ssh usuario@ip-do-mac`), `cd ~/hext-vd && ./vd 300` e conecte com o Hext.

## Resultados

| Teste | Resultado |
|---|---|
| Windows: versão, Secure Boot, Integridade de Memória | |
| Driver instala em silêncio, sem aviso | |
| `enable` → monitor no DXGI (tempo) | |
| `cycle 5` | |
| Sem monitor: imagem no Hext (resolução) | |
| Tela de bloqueio e `Ctrl+Alt+Del` | |
| Tela de login depois de reiniciar sem monitor | |
| Desinstalação limpa | |
| Mac: versão do macOS | |
| `CGVirtualDisplay` no ScreenCaptureKit (tempo) | |
| `vd-captura.png` | |
| Hext vê e mostra o monitor virtual | |
| Mac sem monitor (SSH) | |

## O que já se sabe (10/11/2026, PC de desenvolvimento)

- O script do projeto instalou o driver 25.7.23; o dispositivo aparece como "Virtual Display Driver".
- `enable` pela SetupAPI: o monitor aparece no DXGI em **284 ms**; `disable` o remove. Sem o pipe do driver.
- A captura do Hext abre em 98 ms e recebe imagens (41 em 3 s).
- O monitor virtual nasce em **800×600 a 30 Hz** (o primeiro modo da lista). Aceita 800×600, 1280×720, 1366×768,
  1920×1080 e 2560×1440, de 30 a 165 Hz. O Hext precisa definir 1920×1080 a 60 Hz ao ligar.
- O `vdd_settings.xml` não está em `C:\VirtualDisplayDriver\` nesta versão (onde fica: a confirmar).
- `cycle 5` passou: o monitor aparece em 174 a 203 ms e sai em 138 a 257 ms; a captura abre em 83 a 118 ms e recebe
  50 a 57 imagens em 3 s.
- **A cada habilitação o Windows dá um nome novo** ao monitor (`\.\DISPLAY46`, 47, 48…): o Hext deve achar o
  monitor virtual pelo dispositivo, nunca guardar o nome.
- `resolution 2 1920x1080` muda para 1920×1080 a 60 Hz na hora, e a captura acompanha.
- **Dois cursores na sessão sem monitor:** sem o `vdd_settings.xml`, o driver não declara cursor por hardware
  (`hardwareCursor = false` no código) e o Windows desenha o cursor dentro da imagem; o Hext já desenha o próprio
  cursor no visualizador, então aparecem dois, um atrasado. O arquivo de exemplo do projeto liga
  `<HardwareCursor>true</HardwareCursor>` e começa a lista em 1920×1080 a 60 Hz. O driver lê o arquivo de
  `HKLM\SOFTWARE\MikeTheTech\VirtualDisplayDriver\VDDPATH` ou, sem essa chave, de `C:\VirtualDisplayDriver`. O
  instalador do Hext deve gravar um arquivo próprio (cursor por hardware, 1920×1080 a 60 Hz primeiro), só com
  permissão de administrador.
- Com o `vdd_settings.xml` de exemplo em `C:\VirtualDisplayDriver`, o monitor nasce em **1920×1080 a 60 Hz** e só
  oferece os modos do arquivo (1920×1080, 2560×1440 e 3840×2160, de 60 a 240 Hz): o arquivo controla a resolução
  inicial sem precisar do `ChangeDisplaySettingsEx`.
- **Monitor desligado no botão continua na lista.** Com os dois monitores físicos desligados, o log da sessão das
  16:16 mostra o técnico trocando entre os monitores 1, 2 e 3: o Windows (e o DXGI) ainda listam os dois
  desligados, e a sessão abriu num deles (imagem preta). Sem erro no log. Consequência para o plano: "a lista
  está vazia" só pega o computador **sem cabo** de monitor; com um monitor ligado no cabo e desligado no botão, o
  Hext não sabe que ninguém está vendo. Por isso o monitor virtual precisa também ser uma opção do técnico no
  visualizador ("Monitor virtual"), não só automático.
- Com o arquivo de configuração e os dois monitores desligados no botão, a sessão no monitor virtual roda normal,
  com **um cursor só**: o cursor por hardware resolve o cursor duplicado.
- **Cada sessão começa no monitor 1** (o físico, desligado no botão): a imagem aparece e logo fica preta, e o
  técnico reconecta (sessões de 18 a 68 s no log das 16:28 às 16:32); trocando para o 3, a imagem volta. Uma
  abertura falhou com `Acesso negado (0x80070005)` do DXGI (16:28:42), que a espera de `open_waking` da 0.6.5
  repete. Para o marco 1: com o virtual ligado, a sessão começa nele; e o visualizador lembra o último monitor
  escolhido para cada computador. O "1 de N" no título não aparece em tela cheia: a escolha precisa estar na
  interface.
