# Situação

[English](STATUS.md) | **Português**

Em que pé está o Hext Remote: o que está pronto, o que está em conferência e o que vem a seguir. Atualizado a cada
versão. **Última atualização: novembro de 2026, versão 0.8.1.**

## Pronto

| Fase | O que entregou | Plano |
|---|---|---|
| F0–F2 | Fundação, vídeo na rede local no Windows e no macOS, codificação e decodificação pela GPU | [plano original](planos/plano-original.md#11-roadmap) |
| F3 — Internet | ID de 9 dígitos, servidor de sinalização, QUIC direto pelo `iroh`, relay próprio, pedido de aceite | [F3](planos/F3.md) |
| F4 — Essenciais de suporte | Serviço do Windows (tela de login, UAC, `Ctrl+Alt+Del`), senha de acesso, área de transferência, arquivos, chat, vários monitores, reiniciar e reconectar, técnicos vinculados, vários computadores ao mesmo tempo | [F4](planos/F4.md) |
| F5 — Contas | Contas com 2FA, lista assinada de dispositivos, entrada sem aceite para os dispositivos do dono, compartilhamento, registro de acessos | [F5](planos/F5.md) |
| F6 — Distribuição | `.msi`, `.exe` portátil, `.dmg` notarizado, `.deb`/`.rpm`/`.AppImage`, atualização automática assinada, acesso não assistido no Mac depois do login | [F6](planos/F6.md) |
| Interface no Linux | O app como técnico no Linux: conta, Meus computadores, acessar outros computadores; `.deb` e `.rpm` com as bibliotecas de que precisam (WebKitGTK) (0.8.0) | [arquitetura](../ARCHITECTURE.pt-BR.md#7-plataformas) |
| Monitor virtual | Sessão num computador sem monitor (driver do Windows como opção no instalador, macOS sem instalar nada), resolução pelo `Ctrl+Alt+0`; uma tela que fica preta (monitor desligado no botão) passa para o virtual sozinha | [monitor-virtual](monitor-virtual.md) |

Os nomes em "Meus computadores" seguem o nome atual de cada computador (0.7.1).

## Em conferência no campo

Cada fase fecha com um roteiro em máquinas reais ([roteiros/](roteiros/)):

- [ ] **F3:** casa ↔ 4G (CGNAT), casa ↔ casa e uma rede com UDP totalmente bloqueado ([F3, seção 9](planos/F3.md#9-pendências-testar-no-final)).
- [ ] **F4:** reiniciar pelo técnico e voltar sozinho, no Windows e no Mac; depois, o roteiro de atendimento
      completo de novo ([atendimento-campo](roteiros/atendimento-campo.md)).
- [ ] **F5:** o roteiro das contas ([atendimento-campo-f5](roteiros/atendimento-campo-f5.md)).
- [ ] **F6:** instalar, atualizar e desinstalar em máquinas reais ([instalacao-campo](roteiros/instalacao-campo.md)).
- [ ] **Monitor virtual:** resolução, monitor fantasma (um que perdeu o cabo) e desinstalação em máquinas reais;
      a tela de login só com o monitor virtual; um monitor desligado no botão passando sozinho para o virtual ([monitor-virtual](monitor-virtual.md)).
- [ ] **Linux:** instalar o `.deb` num Ubuntu e o `.rpm` num Fedora, abrir pelo menu, entrar na conta e acessar
      um computador (vídeo, teclado, mouse, arquivo recebido).

## Em aberto

- Em algumas redes domésticas sem CGNAT, a conexão ainda passa pelo relay em vez de ir direto. O app já tenta
  abrir a porta no roteador (UPnP, NAT-PMP e PCP, pelo `iroh`): falta ligar o UPnP no roteador e testar.
- O `.AppImage` do Linux usa o WebKitGTK do sistema (`libwebkit2gtk-4.1`): onde ele não existe, não abre.

## A seguir

- **Tela de login do Mac:** um LaunchDaemon do sistema e agentes por sessão, para alcançar um Mac depois de
  reiniciar, antes de alguém entrar ([F6, seção 3.1](planos/F6.md#31-marco-3-o-que-ficou-pronto-e-o-que-falta)).
- **O visualizador lembrar o último monitor** escolhido em cada computador.
- **Fluidez:** comparar lado a lado com o Parsec e aplicar o que o estudo achou
  ([fluidez-parsec](estudos/fluidez-parsec.md)).
- **F7 — Extras:** host no Linux (ser acessado), áudio, tela de privacidade, gravação de sessão, HEVC/AV1,
  Wake-on-LAN, mobile e web.
