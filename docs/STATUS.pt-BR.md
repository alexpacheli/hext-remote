# Situação

[English](STATUS.md) | **Português**

Em que pé está o Hext Remote: o que está pronto, o que está em conferência e o que vem a seguir. Atualizado a cada
versão. **Última atualização: novembro de 2026, versão 0.9.2.**

## Pronto

| Fase | O que entregou | Plano |
|---|---|---|
| F0–F2 | Fundação, vídeo na rede local no Windows e no macOS, codificação e decodificação pela GPU | [plano original](planos/plano-original.md#11-roadmap) |
| F3 — Internet | ID de 9 dígitos, servidor de sinalização, QUIC direto pelo `iroh`, relay próprio, pedido de aceite | [F3](planos/F3.md) |
| F4 — Essenciais de suporte | Serviço do Windows (tela de login, UAC, `Ctrl+Alt+Del`), senha de acesso, área de transferência, arquivos, chat, vários monitores, reiniciar e reconectar, técnicos vinculados, vários computadores ao mesmo tempo | [F4](planos/F4.md) |
| F5 — Contas | Contas com 2FA, lista assinada de dispositivos, entrada sem aceite para os dispositivos do dono, compartilhamento, registro de acessos | [F5](planos/F5.md) |
| F6 — Distribuição | `.msi`, `.exe` portátil, `.dmg` notarizado, `.deb`/`.rpm`/`.AppImage`, atualização automática assinada, acesso não assistido no Mac depois do login | [F6](planos/F6.md) |
| Interface no Linux | O app como técnico no Linux: conta, Meus computadores, acessar outros computadores; `.deb` e `.rpm` com as bibliotecas de que precisam (WebKitGTK) (0.8.0) | [arquitetura](../ARCHITECTURE.pt-BR.md#7-plataformas) |
| Monitor virtual | Sessão num computador sem monitor (driver do Windows como opção no instalador, macOS sem instalar nada), resolução pelo `Ctrl+Alt+0`; uma tela que fica preta (monitor desligado no botão) passa para o virtual sozinha; o visualizador volta ao último monitor escolhido em cada computador (0.8.2) | [monitor-virtual](monitor-virtual.md) |
| Tela de privacidade (Windows e Mac) | `Ctrl+Alt+P` ou (desde a 0.9.1) o botão na barra do visualizador: quem está no computador vê uma tela preta com um aviso e o mouse e o teclado dele não fazem nada, enquanto o técnico vê e controla tudo; `Ctrl+Alt+Del` lá desliga (no Mac, macOS 12.3 ou mais novo: esc três vezes) (0.9.0) | [F7](planos/F7.md#marco-1-como-funciona-e-roteiro), [Mac](planos/F7.md#marco-2-como-funciona-e-roteiro) |
| Som | O que o host toca (Windows, e macOS 13+) chega ao visualizador, uns 70 ms atrás na rede local; `Ctrl+Alt+S` silencia, e nada trafega com o computador em silêncio (0.9.0). Desde a 0.9.1: a sessão começa sem som e o técnico liga com `Ctrl+Alt+S` ou o botão do alto-falante na barra (desligado, o host não captura nem manda nada); pelo serviço do Windows, o som não picota mais com um vídeo tocando (0.9.1). Na 0.9.2: um soluço do dispositivo de som não reabre mais a captura do Windows (o "tac tac tac" do Mac ouvindo um Windows), e o agente do Windows fica fora do modo de eficiência do Windows 11 | [F7](planos/F7.md#marco-3-como-funciona-e-roteiro) |
| Host no Linux (X11) | O Linux também é acessado, numa sessão X11: captura (XShm, vários monitores pelo XRandR), mouse e teclado (XTest, ABNT2 por scancode), área de transferência e arquivos; codificação pelo VA-API onde houver, OpenH264 onde não; `service install` (sem `sudo`) o abre no login por uma unidade de usuário do `systemd` que vem no `.deb`/`.rpm`. Sem acesso antes do login, sem monitor virtual, sem tela de privacidade (0.9.0). Depois de atualizar o pacote, ele se reabre sozinho assim que fica parado (0.9.2) | [F7](planos/F7.md#marco-4-como-funciona-e-roteiro) |
| Host no Linux (Wayland) | O mesmo no Wayland, pelo portal do sistema: o sistema pergunta uma vez a quem está no computador o que compartilhar (um token de restauração dispensa a pergunta da próxima vez), a imagem pelo PipeWire, mouse e teclado pelo portal RemoteDesktop. A tela de login (GDM) fica de fora (0.9.0) | [F7](planos/F7.md#marco-5-como-funciona-e-roteiro) |

Os nomes em "Meus computadores" seguem o nome atual de cada computador (0.7.1). Na janela da sessão, os botões dos monitores vêm primeiro, ao lado da alça, e `Ctrl+Alt+→`/`←` vão para o próximo e o anterior monitor (0.9.2).

## Em conferência no campo

Cada fase fecha com um roteiro em máquinas reais ([roteiros/](roteiros/)):

- [ ] **F3:** casa ↔ 4G (CGNAT), casa ↔ casa e uma rede com UDP totalmente bloqueado ([F3, seção 9](planos/F3.md#9-pendências-testar-no-final)).
- [ ] **F4:** reiniciar pelo técnico e voltar sozinho, no Windows e no Mac; depois, o roteiro de atendimento
      completo de novo ([atendimento-campo](roteiros/atendimento-campo.md)).
- [ ] **F5:** o roteiro das contas ([atendimento-campo-f5](roteiros/atendimento-campo-f5.md)).
- [ ] **F6:** instalar, atualizar e desinstalar em máquinas reais ([instalacao-campo](roteiros/instalacao-campo.md)).
- [ ] **Monitor virtual:** resolução, monitor fantasma (um que perdeu o cabo) e desinstalação em máquinas reais;
      a tela de login só com o monitor virtual ([monitor-virtual](monitor-virtual.md)). Monitor desligado no botão:
      conferido (novembro de 2026).
- [ ] **Linux:** instalar o `.deb` num Ubuntu e o `.rpm` num Fedora, abrir pelo menu, entrar na conta e acessar
      um computador (vídeo, teclado, mouse, arquivo recebido).
- [x] **Janela da sessão:** cada sessão desenha numa thread própria. No Mac, uma janela minimizada volta sem
      travar; abrir outro computador não para mais as janelas abertas
      ([janela-travada](estudos/janela-travada.md#roteiro-de-campo)). Conferido no Mac (novembro de 2026).
- [ ] **Tela de privacidade:** o roteiro do marco 1 da F7 ([F7](planos/F7.md#marco-1-como-funciona-e-roteiro)) e, num
      Mac, o do marco 2 ([F7](planos/F7.md#marco-2-como-funciona-e-roteiro)).
- [ ] **Som:** o roteiro do marco 3 da F7 ([F7](planos/F7.md#marco-3-como-funciona-e-roteiro)). No campo (novembro
      de 2026), o Mac ouvindo um Windows recebia "tac tac tac"; a correção da 0.9.2 falta conferir (o log do agente
      diz quantos soluços a captura teve).
- [ ] **Host no Linux (X11):** o roteiro do marco 4 da F7 num Ubuntu LTS com GNOME no Xorg
      ([F7](planos/F7.md#marco-4-como-funciona-e-roteiro)). Até aqui, conferido só num servidor X virtual no CI.
- [ ] **Host no Linux (Wayland):** o roteiro do marco 5 da F7 num Ubuntu LTS com GNOME (Wayland)
      ([F7](planos/F7.md#marco-5-como-funciona-e-roteiro)). No campo (novembro de 2026), no Fedora e no
      Ubuntu: a sessão funciona, e o sistema pergunta o que compartilhar só na primeira vez. A barra da sessão abria no
      meio da tela; a correção (a interface pelo XWayland, 0.9.1) funcionou, mas a barra ficava com uns
      200 px de altura e não se recolhia; o tamanho certo (0.9.2) falta conferir.
- [ ] **Repositórios Linux (apt e dnf):** o `.deb` e o `.rpm` se inscrevem nos nossos repositórios e passam a se
      atualizar com o sistema; testado no CI com uma chave descartável. A chave de verdade está nos segredos do ambiente
      `linux-repo`, o repositório está no ar (novembro de 2026), e os pacotes se inscrevem a partir da primeira
      versão publicada depois disso. Falta, num Ubuntu e num
      Fedora reais, ver uma versão nova chegar pelo `apt upgrade`/`dnf upgrade` e pela loja de aplicativos (no
      Fedora, ver se o GNOME Software/PackageKit pede para confiar na chave, por causa do `repo_gpgcheck`; ainda
      não conferido).

## Em aberto

- Em algumas redes domésticas sem CGNAT, a conexão ainda passa pelo relay em vez de ir direto. O app já tenta
  abrir a porta no roteador (UPnP, NAT-PMP e PCP, pelo `iroh`): falta ligar o UPnP no roteador e testar.
- No Mac, depois de reiniciar, o Hext só volta quando alguém entra na conta. Fica assim de propósito
  ([decisões](DECISIONS.pt-BR.md#mac-sem-tela-de-login)): com o FileVault ligado, o macOS pede uma senha antes de carregar.
- O `.AppImage` do Linux usa o WebKitGTK do sistema (`libwebkit2gtk-4.1`): onde ele não existe, não abre.
- Se o repositório do GitHub ficar privado, o GitHub Pages (onde estão os repositórios Linux) exige plano pago.
  Antes de desligar o Pages ou mudar `HEXT_REPO_URL`, é preciso publicar no endereço antigo uma release gerada
  com o novo e manter o antigo no ar por um tempo; senão, cada usuário tem de apagar à mão
  `/etc/apt/sources.list.d/hext-remote.sources` ou `/etc/yum.repos.d/hext-remote.repo`
  ([repositorios-linux](repositorios-linux.md#mudar-de-endereço-ou-tornar-o-repositório-privado)).

## A seguir

- **F7 — Extras** ([plano](planos/F7.md)): o roteiro de campo de cada marco;
  Wake-on-LAN, gravação de sessão, HEVC/AV1, vários técnicos, mobile e web quando houver demanda.
- **Fluidez, por último e com calma:** comparar lado a lado com o Parsec, na mesma rede e sem Tailscale, e aplicar o
  que o estudo achou ([fluidez-parsec](estudos/fluidez-parsec.md)). Já entrou: a captura acompanha a cadência da tela,
  e o log mostra o maior intervalo entre quadros nos dois lados.
