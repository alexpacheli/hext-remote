# Documentação

[English](README.md) | **Português**

Por onde começar e para que serve cada documento.

## Comece aqui

| Documento | Para |
|---|---|
| [README](../README.pt-BR.md) | O que é o Hext Remote, download, primeiro atendimento, compilar |
| [Arquitetura](../ARCHITECTURE.pt-BR.md) | Como funciona hoje: peças, conexão, vídeo, quem entra |
| [Decisões](DECISIONS.pt-BR.md) | As principais escolhas e por quê |
| [Situação](STATUS.pt-BR.md) | O que está pronto, em conferência e a seguir |

## Servidor próprio

| Documento | Para |
|---|---|
| [self-hosting](self-hosting.pt-BR.md) | O seu servidor e o seu app, de ponta a ponta |
| [deploy](deploy.md) | O servidor instalado com systemd, atualização, acompanhamento, limites do relay |
| [hext-server](hext-server.md) | O servidor de sinalização: protocolo, contas, configuração, e-mail |

## Recursos

| Documento | Para |
|---|---|
| [monitor-virtual](monitor-virtual.md) | Monitor virtual para computadores sem tela: como funciona, instalação, roteiros |

## Roteiros de teste — [roteiros/](roteiros/)

| Documento | Para |
|---|---|
| [testes](roteiros/testes.md) | Testar cada recurso, e cada opção da linha de comando |
| [atendimento-campo](roteiros/atendimento-campo.md) | Um atendimento real do começo ao fim (fecha a F4) |
| [atendimento-campo-f5](roteiros/atendimento-campo-f5.md) | As contas usadas de verdade (fecha a F5) |
| [instalacao-campo](roteiros/instalacao-campo.md) | Instalar, atualizar e desinstalar em máquinas reais (fecha a F6) |

## Planos das fases — [planos/](planos/)

Como cada fase foi planejada e feita, marco a marco, com o que se decidiu no caminho. Ficam como registro: o
retrato atual está na Arquitetura e na Situação.

| Documento | Fase |
|---|---|
| [plano-original](planos/plano-original.md) | O primeiro plano, antes do código |
| [F3](planos/F3.md) | Internet: ID, sinalização, `iroh`, relay |
| [F4](planos/F4.md) | Essenciais de suporte: serviço do Windows, senha, canais, reinício |
| [F5](planos/F5.md) | Contas: lista assinada de dispositivos, compartilhamento, registro de acessos |
| [F6](planos/F6.md) | Distribuição: instaladores, atualização, Mac sem aceite |

## Estudos e medições — [estudos/](estudos/)

Notas de trabalho por trás de mudanças específicas. Descrevem o código da época em que foram escritas.

| Documento | Sobre |
|---|---|
| [fluidez-parsec](estudos/fluidez-parsec.md) | O que o Parsec faz pela fluidez, e o que aplicar aqui |
| [medicao-video](estudos/medicao-video.md) | Medir o vídeo com a tela mudando; oito rodadas de resultados |
| [janela-travada](estudos/janela-travada.md) | A janela da sessão que travava pela internet: causa e correção |
| [spike-monitor-virtual](estudos/spike-monitor-virtual.md) | Primeiro teste do driver do monitor virtual no Windows e no macOS |
