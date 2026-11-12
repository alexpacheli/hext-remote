# Instalação de campo (F6, marco 4)

Instalar, atualizar e desinstalar o Hext Remote nas três plataformas, em máquinas reais, como um usuário
final faria. Serve para fechar a F6. Marque cada passo e anote avisos do sistema, tempo e cliques a mais.

## Antes de começar

- [ ] Uma release publicada no GitHub (o rascunho que o pipeline abre precisa ser publicado). Os exemplos
      usam 0.7.0 e 0.7.1: troque pelas versões do teste.
- [ ] Os instaladores do Windows não têm assinatura de código: o SmartScreen avisa na primeira abertura. O Mac
      precisa dos segredos da Apple (seção 4 do [plano](../planos/F6.md)).
- [ ] Para testar a atualização: uma segunda release (`v0.7.1`), publicada depois de instalar a primeira.

## 1. Windows (.msi)

1. [ ] Baixar `HextRemote-0.7.0-x64.msi` e abrir. O SmartScreen avisa (sem assinatura): **Mais
   informações** → **Executar assim mesmo**. O UAC mostra o editor como desconhecido.
2. [ ] Terminado, o menu Iniciar tem o Hext Remote, e `C:\Program Files\Hext Remote\HextRemote.exe
   service status` (como administrador) mostra o serviço rodando, com ID e nome.
3. [ ] A tela inicial abre sozinha no login, com o ID. Um técnico conecta pelo ID.
4. [ ] Reiniciar o computador: o ID é o mesmo e o serviço volta sozinho.
5. [ ] **Atualização:** publicar a `v0.7.1`. Em até uma hora (sem sessão aberta), o serviço se atualiza: o
   `audit.log` diz "atualização automática: 0.7.0 → 0.7.1", e `service status` mostra a versão nova. Com uma
   sessão aberta, ele espera ela acabar.
6. [ ] **Instalação silenciosa** num segundo computador: `msiexec /i HextRemote-0.7.0-x64.msi /qn` (como
   administrador). Nada aparece, e o serviço fica rodando.
7. [ ] **Desinstalar** em Configurações → Aplicativos. O serviço some. Reinstalar: o ID continua o mesmo.

## 2. Windows (.exe portátil)

1. [ ] Baixar `HextRemote-0.7.0-portable.exe` e abrir, sem instalar. O SmartScreen avisa, como no `.msi`.
2. [ ] Mostra um ID, e o técnico conecta com aceite.
3. [ ] Com a `v0.7.1` publicada, a tela inicial mostra "Versão 0.7.1 disponível". **Baixar** abre a página
   da versão.
4. [ ] **Instalar neste computador** (num computador sem o Hext instalado): o Windows pede administrador, o
   instalador roda, a janela do portátil fecha e a do instalado abre sozinha, com o ID da instalação. O Hext
   Remote aparece em Configurações → Aplicativos. (Num build sem a chave de release, sem `.msi` para baixar,
   ele instala o próprio programa como serviço e não aparece em Aplicativos: `service uninstall` remove.)
5. [ ] Recusar o pedido de administrador: nada muda, e a tela diz que é preciso permitir.
6. [ ] Com o Hext já instalado, o portátil não mostra o botão.

## 3. macOS (.dmg)

1. [ ] Baixar `HextRemote-0.7.0.dmg`, abrir e arrastar para Aplicativos. Abrir pelo Launchpad: **sem aviso
   de desenvolvedor não identificado** (com a notarização).
2. [ ] O app pede Gravação de Tela e Acessibilidade; com as duas, um técnico conecta com aceite.
3. [ ] `"/Applications/Hext Remote.app/Contents/MacOS/HextRemote" service install` (sem sudo): abre sozinho no
   login, com o mesmo ID.
4. [ ] Com a `v0.7.1` publicada, a tela inicial avisa e **Baixar** abre a página.
5. [ ] `service uninstall` e apagar o app: não abre mais no login.

## 4. Linux

Em Ubuntu (ou Debian) e Fedora reais, não só no CI:

1. [ ] `sudo apt install ./hext-remote_0.8.0-1_amd64.deb`, sem instalar mais nada antes, e `hext-remote --version`
   mostra 0.8.0.
2. [ ] Abrir **Hext Remote** pelo menu de aplicativos: a tela inicial abre sem ID e sem a aba Segurança (o Linux
   ainda só acessa outros computadores).
3. [ ] Entrar na conta: Meus computadores mostra quem está online.
4. [ ] Conectar a um computador Windows pela tela inicial (pelo ID ou por Meus computadores): vídeo, mouse,
   teclado, área de transferência e, com dois monitores, `Ctrl+Alt+1` e `Ctrl+Alt+2`.
5. [ ] Arquivos: um arrastado para a janela da sessão chega em Downloads do outro lado. Um enviado do outro lado
   abre a pergunta numa janela à parte, e o título da sessão avisa para respondê-la.
6. [ ] Repetir os passos 2 a 4 numa sessão Wayland (o padrão do Ubuntu e do Fedora) e numa X11; anotar o que muda.
7. [ ] Atualizar: com uma versão mais nova publicada, a tela inicial avisa. `sudo apt install` do `.deb` novo
   substitui a versão.
8. [ ] `sudo apt remove hext-remote` remove, e o app some do menu.
9. [ ] No Fedora: `sudo dnf install ./hext-remote-0.8.0-1.x86_64.rpm` e os mesmos passos 1 a 8, com `dnf`.
10. [ ] `.AppImage`: `chmod +x` e abrir `./HextRemote-0.8.0-x86_64.AppImage` (usa o WebKitGTK do sistema).

## Resultado

| Item | Ok? | Observação |
|---|---|---|
| Windows .msi (instalar, reiniciar, desinstalar) | | |
| Windows atualização automática | | |
| Windows instalação silenciosa | | |
| Windows .exe portátil | | |
| macOS .dmg (notarizado) | | |
| Linux .deb | | |
| Linux .rpm | | |
| Linux .AppImage | | |

Com tudo ok, a F6 fecha.
