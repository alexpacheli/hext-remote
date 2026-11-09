# Testes e uso pela linha de comando

Roteiros para testar cada parte do Hext Remote e o uso de cada recurso pela linha de comando. A visão geral, os
instaladores e o servidor próprio ficam no [README](../README.md).

## Baixar os executáveis do CI

Cada push gera executáveis em **GitHub → Actions → CI → (execução) → Artifacts**:

- `HextRemote-windows-x64` → `HextRemote.exe`
- `HextRemote-macos-universal` → `HextRemote` (Intel e Apple Silicon)
- `hext-remote-linux-x64` → `hext-remote`

Os binários ainda não são assinados (isso entra na F6):

- **Windows:** o SmartScreen pode avisar. Clique em "Mais informações" → "Executar assim mesmo".
- **macOS:** antes de rodar, no Terminal, na pasta do arquivo:

  ```sh
  chmod +x HextRemote && xattr -d com.apple.quarantine HextRemote
  ```

## Testar: controlar o Windows a partir do Mac (mesma rede)

1. **No Windows** (o computador que será acessado), no PowerShell, na pasta do arquivo:

   ```sh
   .\HextRemote.exe host
   ```

   Ele mostra o **endereço**, um **código de acesso** e a **chave** deste computador, além do comando
   pronto para o outro computador. Permita o acesso quando o Firewall perguntar (porta UDP 7420).
   A chave é criada na primeira execução em `%APPDATA%\HextRemote\device.key` (no Mac,
   `~/Library/Application Support/HextRemote`) e não muda: o comando do outro computador pode ser guardado.

2. **No Mac**, cole o comando mostrado pelo Windows, trocando o nome do programa:

   ```sh
   ./HextRemote view 192.168.0.10 --code 482913557 --key <chave>
   ```

   Abre uma janela com a tela do Windows. Mouse e teclado da janela controlam o Windows.
   O título da janela mostra fps, banda, tempos de captura/encode/decode e a latência (RTT).
   O visualizador decodifica na placa de vídeo quando pode (`(GPU)` ao lado do decode);
   `view ... --decoder software` força o processador.

- **Tecla Command:** por padrão funciona como Ctrl (Cmd+C copia no Windows). Use `--cmd-key win` para que ela seja a tecla Windows.
- **Fora de casa:** conecte pelo ID (abaixo); pelo IP, os dois precisam estar na mesma rede (ou no Tailscale).
- **Outros monitores:** o técnico troca de monitor durante a sessão com **Ctrl+Alt+1..9** ou pelos botões
  numerados no topo da janela. `host --display 1` começa no segundo monitor.
- **Ajustes:** `host --bitrate-mbps 15 --fps 60`.
- **Codificador:** por padrão usa a placa de vídeo (NVIDIA, AMD ou Intel, via Media Foundation) e volta
  para o processador se ela não existir ou falhar. O título da janela mostra `(GPU)` ou `(CPU)` ao lado
  do tempo de encode. Para forçar: `host --encoder software` ou `host --encoder hardware`.

## Testar: conectar pelo ID (servidor de sinalização)

Em vez de IP, código e chave, o técnico digita o **ID de 9 dígitos** do host, e quem está no
host aceita ou recusa o pedido. O app usa o servidor definido na compilação (o nosso, `wss://signal.hext.com.br`, na versão oficial; veja
[deploy.md](deploy.md)) por padrão:

1. **No Windows** (host): `.\HextRemote.exe host`. Ele mostra `Seu ID: 482 913 557`.
2. **No Mac** (técnico): `./HextRemote view 482913557`.
3. **No Windows**, o host pergunta `"Mac" (ID …) quer acessar este computador. Aceitar? [s/N]`. Digite
   `s` e Enter.

Outro servidor (o `hext-server` vem junto nos artefatos, para testes): `--server ws://<IP>:8080` nos dois;
`--server off` desliga os IDs (só conexão pelo IP). O nosso relay só atende computadores registrados no
nosso servidor: com outro servidor, sem servidor ou pelo IP, a conexão é só direta, a não ser que você
informe outro relay (`--relay <url>`).

Na primeira conexão, o técnico guarda a chave do host (`known_hosts`, na mesma pasta da chave). Se outro
computador aparecer com o mesmo ID, a conexão é bloqueada com um aviso.

Depois do aceite, os dois tentam se conectar **direto**, mesmo em redes diferentes (atravessando o NAT);
quando não dá (firewall corporativo, por exemplo), a conexão passa pelo **relay** em São Paulo
(`relay.hext.com.br`). O título da janela mostra `(P2P)` ou `(relay)`. `--relay off` desliga o relay;
`--relay <url>` usa outro.

## Testar: Hext Remote instalado como serviço (Windows)

Instalado, o computador fica online com o **ID da máquina** desde o boot, sem ninguém abrir o app
(F4, marco 1; veja [F4.md](F4.md)).

1. **No Windows**, num terminal **como administrador**: `.\HextRemote.exe service install`. Ele copia o
   app para `C:\Program Files\Hext Remote`, inicia o serviço e mostra o ID deste computador.
2. **No Mac**: `./HextRemote view <ID>`.
3. **No Windows**, aparece uma janela perguntando se aceita. Clique em **Sim**.

Para dar um nome ao computador, que é o que o técnico vê na sessão e na lista de computadores, clique em
**Alterar** ao lado do nome, na tela inicial (vazio volta ao nome do sistema). Pelo terminal:
`service install --name "Recepção"` ou, depois, `service name "Recepção"` (`--reset` volta ao nome do sistema).
No Windows, com o serviço, o Windows pede permissão de administrador e o serviço reinicia.

`service status` mostra o estado e o ID; `service uninstall` remove o serviço (`--purge` apaga também a
identidade, e o ID deixa de valer). Os logs ficam em `C:\ProgramData\HextRemote` (`service.log` e
`agent.log`), uma pasta que só administradores abrem.

Com o serviço, o técnico vê e controla também a **tela de login, o bloqueio e os prompts do UAC**, e manda
`Ctrl+Alt+Del` com **Ctrl+Alt+End** (ou Ctrl+Alt+Delete no Mac). Com a permissão de reiniciar, aperte
**Ctrl+Alt+R** duas vezes em até 5 segundos: o computador reinicia, fecha os programas abertos sem pedir para
salvar, e o técnico volta sozinho quando ele estiver online, sem novo aceite. Quedas de rede também
reconectam sozinhas. O acesso pelo IP com código fica
desligado no serviço: só pelo ID, com aceite.

Com a permissão de arquivos, arraste um arquivo para a janela da sessão: ele vai para a pasta Downloads do
usuário logado no host. No host, **Enviar arquivo** na barra manda um arquivo para o técnico, que confirma
antes de receber em Downloads. Nada é substituído: um nome repetido chega como `a (1).txt`. Pastas não são
enviadas, e uma transferência interrompida não é retomada.

### Acesso não assistido

Com a senha, o técnico entra sem que ninguém aceite no computador. Quem liga é o dono, como administrador:
na aba **Segurança**, **Definir a senha de acesso** (digite a senha duas vezes no próprio cartão e salve; o Windows pede
administrador. O mesmo cartão troca e desliga). Pela linha de comando:

```sh
HextRemote.exe service password        # define a senha (ou troca)
HextRemote.exe service password --off  # desliga
```

Quem entra com a senha também pode copiar e colar texto, assim como quem foi aceito com **Área de transferência** marcada. Só passa o que for copiado durante a sessão, até 512 KB.

No técnico, preencha a senha na tela inicial ou use `view <ID> --password-stdin`. A senha nunca trafega
(SPAKE2) e, depois de 5 erros, o computador espera 1, 5 e 15 minutos. Durante a sessão, a barra mostra
quem está conectado, também na tela de login e com o computador bloqueado, e cada acesso fica registrado
em `C:\ProgramData\HextRemote\audit.log`. A tela inicial guarda o nome e o ID dos computadores acessados,
nunca a senha. Uma bolinha ao lado de cada um mostra se ele está online (verde) ou não (cinza), com o
serviço instalado ou com o app aberto à mão. Ela só aparece para os computadores que aceitaram você, ou em
que você entrou com a senha, nos últimos 90 dias.

**Técnico vinculado.** Ao entrar com a senha, o técnico pode marcar **Entrar sem senha da próxima vez** (ou
usar `view <ID> --password-stdin --remember`). Se a senha estiver certa, o computador passa a confiar na
chave daquele técnico: nas próximas vezes ele conecta pelo ID, sem senha e sem ninguém aceitar. Nenhuma senha
fica guardada, só a chave do técnico. Na tela inicial do computador, **Técnicos que entram sem senha** lista
cada um com **Revogar**, e o dono também pode usar:

```sh
HextRemote.exe service technicians                  # lista
HextRemote.exe service technicians --revoke <ID>    # revoga um
HextRemote.exe service technicians --revoke-all     # revoga todos
```

Trocar ou desligar a senha revoga todos. Com a senha desligada, ninguém entra sem aceite, nem os vinculados.
Cada vínculo, entrada e revogação vai para o `audit.log`.

**Conta vinculada** (Windows, com o serviço). Com o computador em **Meus computadores** da sua conta, o
botão **Deixar os dispositivos da conta entrarem sem aceite** (ou a linha de comando, como administrador)
faz os dispositivos da lista da conta entrarem sem aceite e sem senha. Quem sai da lista deixa de entrar, e a
sessão dele cai em até 30 segundos. O computador confere a lista ele mesmo: o servidor não consegue inserir
ninguém nela.

```sh
HextRemote.exe service account               # mostra a conta vinculada
HextRemote.exe service account <ID da lista> # vincula (o ID está no app, em Dispositivos)
HextRemote.exe service account --off         # desvincula
```

**Compartilhar com outra conta.** Em Dispositivos, **Compartilhar** ao lado do computador abre o acesso aos
dispositivos de outra conta: cole o **ID da lista** dela (ela o vê em Dispositivos), dê um nome, escolha as
permissões e a validade. **Revogar** fecha na hora (uma sessão em andamento cai em até 30 segundos). Vale
com o computador vinculado à sua conta.

**Atividade.** Em Dispositivos, **Ver os acessos** mostra quem entrou nos computadores vinculados à conta, como
a auditoria de cada um registrou. O `audit.log` de cada computador continua sendo o registro completo.

Dá para atender vários computadores ao mesmo tempo (até 8), cada um na sua janela: conecte em um e depois em
outro, pela mesma tela inicial. A seção **Sessões abertas** lista cada um, com **Mostrar** (traz a janela para
a frente) e **Encerrar** (ou **Cancelar**, enquanto o outro lado ainda não aceitou). A área de transferência
vale só para a janela usada por último, para que o texto de um cliente nunca chegue a outro.

Na janela do visualizador, **Ctrl+Alt+Enter** entra e sai da tela cheia, com a imagem ocupando a tela
inteira sem distorcer. Na borda esquerda da janela, no meio da altura, fica sempre uma faixa fina: passar o mouse
nela abre os botões ao lado dela, que recolhem pouco depois de o mouse sair. (A borda de cima fica para a barra
de sessão do outro computador, que recolhe lá.) Um deles faz o mesmo que o atalho. Se o outro computador
tiver mais de um monitor, os botões numerados ficam ao lado: o 1 é o monitor principal, e os outros seguem da
esquerda para a direita. **Ctrl+Alt+1..9** faz o mesmo.

Para conversar durante a sessão, o técnico usa o botão com um balão ou **Ctrl+Alt+C**, e quem está no outro
computador usa o botão **Chat** da barra de sessão. A janela abre sozinha quando chega uma mensagem, sem tirar
o foco. O chat funciona nas conexões abertas pela tela inicial do app, e as mensagens não ficam salvas.

## Testar: Hext Remote instalado no Mac

Instalado, o Mac abre o Hext Remote sozinho a cada login, sem janela, com o mesmo ID do app (F4, marco 9).

1. **No Mac**, no Terminal, **sem sudo**: `./HextRemote service install`. Ele monta o app em
   `~/Library/Application Support/HextRemote/HextRemote.app`, inicia e mostra o ID.
2. Na primeira vez, o macOS pede **Gravação de Tela** e **Acessibilidade** para o **Hext Remote**. Libere as
   duas em **Ajustes do Sistema → Privacidade e Segurança** e rode `service install` de novo, para ele reabrir
   com a Gravação de Tela.
3. **No técnico**, conecte pelo ID. No Mac, aparece a janela de aceite, agora com **Reiniciar**.

Com a permissão de reiniciar, **Ctrl+Alt+R** duas vezes reinicia o Mac. Os apps recebem o pedido para fechar, e
um app com algo sem salvar pode segurar o reinício. O técnico volta sozinho quando alguém entrar no Mac, se for
em até 10 minutos.

**Senha de acesso no Mac (F6, marco 3).** Na aba **Segurança** do app instalado, **Definir a senha de acesso**
(digite a senha no próprio cartão, sem administrador; no Terminal, `./HextRemote service password`, sem sudo). Com ela, um
técnico entra sem aceite, pode marcar **Entrar sem senha da próxima vez** e inserir o Mac na conta dele, como no
Windows. Vale depois que o usuário entra no Mac: a tela de login ainda fica de fora. `service technicians` e
`service account` também valem no Mac, sem sudo.

`service status` mostra o estado e o ID. `service uninstall` tira do login (`--purge` apaga também a chave, e o
ID muda). O log fica em `~/Library/Application Support/HextRemote/ui.log`.

## Testar: controlar o Mac a partir do Windows

1. **No Mac**, no Terminal: `./HextRemote host`.
   Na primeira vez o macOS pede duas permissões para o **Terminal** (é ele que "abre" o Hext Remote):
   - **Gravação de Tela** — para enviar a imagem;
   - **Acessibilidade** — para receber mouse e teclado.

   Libere as duas em **Ajustes do Sistema → Privacidade e Segurança**, **feche o Terminal por completo
   (Cmd+Q)** e abra de novo. O host avisa no terminal se alguma faltar.
2. **No Windows**: `.\HextRemote.exe view <IP-do-Mac> --code ... --key ...` (o comando aparece no Mac).

- **Teclado:** no Windows, **Ctrl funciona como Command** (Ctrl+C copia no Mac) e a tecla Windows vira Control.
  Para manter o Ctrl normal: `view ... --ctrl-key ctrl`.
- **Codificador:** o Mac usa o próprio chip de vídeo (VideoToolbox) e volta para o processador se ele
  falhar; `host --encoder software` força o processador, para comparar. O título da janela no Windows
  mostra `(GPU)` ou `(CPU)` ao lado do tempo de encode.
- A imagem é capturada no tamanho "lógico" da tela (ex.: 1920×1080 num monitor 4K Retina), o que deixa
  tudo mais leve.

### Limitações atuais

- No Linux o host ainda transmite só uma imagem de teste (`--test-pattern`).
- No Mac a imagem ainda passa pela memória do sistema (1,5 bytes por pixel, já em NV12); no Windows ela vai direto da captura para o encoder pela GPU (zero-copy), exceto em placas sem processador de vídeo D3D11 ou com o encoder em outra placa (ex.: notebooks com duas GPUs), que voltam ao caminho pela memória.
- Sem o serviço, janelas abertas como administrador, a tela de UAC e a tela de bloqueio não recebem controle.
