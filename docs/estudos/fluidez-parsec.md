# Fluidez: o que o Parsec faz e o que aplicar no Hext Remote

Estudo para decidir, antes da F7, o que mudar para o Hext Remote ficar tão fluido quanto o Parsec. Nenhum código foi
alterado. As referências de código são deste repositório em `6e73a52`.

## Fontes

- Blog do Parsec, "A Networking Protocol Built For Low Latency Interactive Game Streaming" (o protocolo BUD).
- Blog do Parsec, "The Technology Behind A Low Latency Cloud Gaming Service" (captura, codificação, zero-copy, V-sync).
- Conversa do usuário com o ChatGPT (`Tecnologia do Parsec.pdf`). Serve de roteiro, não de prova: o que ela afirma sem
  citar o Parsec ficou de fora ou está marcado como hipótese.
- [medicao-video.md](medicao-video.md), com as oito medições anteriores, e o código em `crates/` e `apps/hext`.

## Resumo

1. **O Hext já tem a arquitetura do Parsec:** captura DXGI na GPU, H.264 por hardware, datagramas sem retransmissão
   automática, FEC, reenvio seletivo, pacing e taxa adaptativa. Trocar o QUIC por um UDP próprio não é o primeiro passo.
2. **A diferença percebida vem provavelmente de cinco pontos, nesta ordem:** o caminho de rede das medições (Tailscale e
   Wi-Fi), o teto de 8 Mbps, o ritmo da captura (que deriva e pula quadros), a ida do quadro decodificado à CPU no
   visualizador e o controle de congestionamento Cubic do QUIC, que segura o envio a cada perda.
3. **Falta uma medição de ponta a ponta.** Os logs mostram cada etapa por segundo, mas nenhum número diz quantos
   milissegundos separam a tela do host da tela do visualizador, nem no Hext nem no Parsec. Sem ela, não se sabe quanto
   cada mudança rende.

A ordem recomendada está em [Mudanças priorizadas](#mudanças-priorizadas): primeiro medir e igualar o caminho (custo
quase nulo), depois três mudanças pequenas no host, depois o visualizador sem a CPU e, só então, o controle de
congestionamento próprio.

## O que o Parsec faz

### Prioridades

O Parsec declara a ordem: **latência, depois quadros por segundo, depois qualidade da imagem**. Diante de um problema de
rede, ele baixa a qualidade antes de aceitar atraso ou quadros perdidos. Também afirma que **não tem buffer de nenhum tipo
no vídeo**: por isso precisa detectar o congestionamento antes que ele comece.

### Transporte: BUD

- Protocolo próprio sobre UDP, cifrado com DTLS 1.2.
- Confiabilidade "como a do TCP", mas com **controle de congestionamento próprio e muito ajustado**. O primeiro Parsec usava
  TCP, que reagia mal à perda; UDP puro deixava a imagem corrompida. WebRTC também foi testado e descartado.
- **Rede e codificador trabalham juntos:** o protocolo informa o codificador e vice-versa, e as decisões de taxa são
  tomadas "em frações de segundo". Com muita perda, a resposta é mudar a taxa do codificador.
- O objetivo é ocupar a banda disponível até o limite, sem provocar perda nem atraso. O próprio Parsec admite que o
  algoritmo era conservador demais com RTT alto, e que é otimizado para menos de 50 ms de RTT e 30 Mbps.
- Diante de uma interrupção, o Parsec pausa o vídeo e mostra que a rede está ruim, em vez de acumular atraso; encerra a
  sessão depois de 60 s.

O artigo não detalha FEC, pacing nem o formato dos pacotes. O que se sabe é que há confiabilidade (retransmissão) e que o
controle de congestionamento é a peça central.

### Captura, codificação e exibição

- **Somente hardware:** codificação e decodificação H.264 nos blocos dedicados das GPUs. O Parsec usa diretamente as APIs
  de cada fabricante (NVIDIA, AMD e Intel), sem camadas intermediárias, e relata codificação mais decodificação abaixo
  de 10 ms.
- **Captura sem cópia:** Desktop Duplication entrega o quadro na memória de vídeo, e a regra é que **o quadro bruto nunca
  passe pela memória do sistema**, nem no host (captura → codificador) nem no cliente (decodificador → tela). Qualquer
  cópia para a memória do sistema tem "impacto perceptível na latência".
- **Conversão de cor na GPU:** o NV12 decodificado vira RGB num pixel shader, desenhado direto no back buffer.
- **V-sync ligado, sem fila:** os quadros chegam em intervalos irregulares; o Parsec mantém o V-sync, mas descarta os
  quadros acumulados quando o host envia um pouco mais rápido do que o monitor do cliente mostra. No Windows usa o
  efeito de troca "flip sequential".
- O material do ChatGPT cita, com base na documentação do Parsec, cerca de **7 ms de latência adicionada em rede local
  cabeada**. É a referência a perseguir.

## O que o Hext faz hoje, ponto a ponto

| Ponto | Parsec | Hext Remote hoje | Onde |
| --- | --- | --- | --- |
| Transporte | UDP próprio (BUD) com DTLS | Datagramas QUIC (iroh/noq), sem retransmissão automática | `crates/net/src/video.rs:517` |
| Controle de congestionamento | Próprio, antecipa o congestionamento | Cubic, o padrão do noq, baseado em perda; `send_datagram_wait` espera a janela abrir | `noq-proto/src/config/transport.rs:613`, `crates/net/src/endpoint.rs:38` |
| Fila de envio | Nenhuma no vídeo | 64 kB de datagramas no QUIC, mais um quadro no canal da captura | `crates/net/src/endpoint.rs:18`, `crates/session/src/host.rs:684` |
| Taxa | Ajustada em frações de segundo | Decidida por segundo, só depois de 2 segundos ruins; teto de 8 Mbps | `crates/net/src/rate.rs:15-35`, `crates/codec/src/lib.rs:86` |
| FEC | Não documentado | Reed-Solomon fixo: 20%, 40% nos keyframes | `crates/net/src/video.rs:41-47` |
| Retransmissão | Sim | Reenvio seletivo pedido pelo visualizador após 40 ms, 3 tentativas, desiste com 600 ms sem pacote | `crates/net/src/video.rs:69-105` |
| Pacing | Não documentado | 4 vezes a taxa, com rajada livre de 4 ms; timer de 1 ms no Windows | `crates/net/src/video.rs:52-60`, `crates/net/src/timer.rs` |
| Perda irrecuperável | Não documentado | Keyframe (IDR) completo, de 200 a 300 kB nas medições | `crates/net/src/video.rs:109`, `crates/session/src/client.rs:26` |
| Captura no Windows | Desktop Duplication na GPU | Desktop Duplication, conversão para NV12 na GPU (Video Processor) | `crates/capture/src/windows.rs:297`, `crates/capture/src/gpu_nv12.rs` |
| Ritmo da captura | Quadros na cadência da tela | `sleep` até completar 1/60 s desde o início do quadro anterior | `crates/session/src/host.rs:1160-1168` |
| Codificador no Windows | API nativa de cada fabricante | Media Foundation (MFT do fabricante), CBR, perfil Baseline, sem quadros B, GOP de 1 h, modo de baixa latência | `crates/codec/src/mf.rs:316-382` |
| Codificador → host | Entrega imediata | Se o MFT pede nova entrada antes de entregar a saída, o quadro só sai no ciclo seguinte | `crates/codec/src/mf.rs:630-652`, `crates/session/src/host.rs:1243` |
| Zero-copy no host | Sim | Sim no Windows (textura D3D11 direto ao MFT); no Mac, cópia para a memória e de novo para o pool do VideoToolbox | `crates/session/src/host.rs:1295`, `crates/capture/src/macos.rs:84-143`, `crates/codec/src/vt.rs:9` |
| Decodificação no Windows | Hardware, quadro fica na GPU | Hardware (DXVA), mas o quadro é **lido de volta para a memória**, convertido de NV12 para I420 na CPU e enviado de novo à GPU em três texturas | `crates/codec/src/mf_decode.rs:267-293`, `crates/codec/src/d3d11.rs:60-86`, `crates/render/src/lib.rs:451`, `:613` |
| Conversão de cor | Shader | Shader (`yuv.wgsl`), a partir de três planos I420 | `crates/render/src/lib.rs:3`, `:558-602` |
| Fila no visualizador | Descarta os acumulados | Só o quadro mais novo vai para a tela | `apps/hext/src/viewer.rs:196-215` |
| Apresentação | V-sync, flip sequential | Mailbox ou Immediate, latência máxima de 1 quadro | `crates/render/src/lib.rs:264-269` |
| Cursor | Desenhado no cliente | Desenhado no visualizador, sem esperar o vídeo | `crates/capture/src/windows.rs:8` |

O que está bem resolvido e não precisa mudar: captura e conversão na GPU no host Windows, zero-copy até o codificador,
codificador sem quadros B e em baixa latência, fila de apresentação de um quadro, descarte dos quadros acumulados no
visualizador, cursor local e timer de 1 ms durante o pacing (inclusive com o host minimizado no Windows 11).

## A diferença provável

O usuário descreve as três sensações ao mesmo tempo: travadas ao mover janelas, resposta atrasada e menos quadros por
segundo. Cada uma tem causas diferentes. Abaixo, as causas em ordem de probabilidade, com a evidência de cada uma.

### 1. O caminho de rede não era o mesmo

Na quarta medição, o Hext passou o tempo todo pelo Tailscale (um IP `100.x`), com RTT de 18 a 472 ms (mediana de
76 ms): o host estava em Wi-Fi em `192.168.1.x` e o visualizador em cabo em `192.168.0.x`, sem rota entre as sub-redes.
Na sexta, a rodada pelo IP público teve RTT de 16 a 48 ms, contra 25 a 483 ms de uma rodada pelo Tailscale. "Mesma rede"
não garante o mesmo caminho: o Parsec abre o próprio caminho e pode ter ido pelo IP público, sem o túnel.

Um RTT que oscila até 470 ms explica sozinho a resposta lenta e, com o Cubic (item 5), boa parte das travadas. **Nenhuma
mudança no código se compara a isso enquanto o caminho não for o mesmo.**

### 2. O teto de 8 Mbps limita os quadros por segundo nas mudanças grandes

A taxa padrão é 8 Mbps em CBR (`crates/codec/src/lib.rs:86`, `crates/codec/src/mf.rs:366`). A 60 quadros por segundo,
isso dá cerca de 16,7 kB por quadro. Maximizar ou arrastar uma janela grande gera quadros de 70 a 160 kB. Para respeitar a
taxa, o codificador entrega menos quadros: na terceira medição, com rede limpa e **0 ms de espera pela rede**, a captura
teve só 7 a 11 imagens novas por segundo com quadros de 70 a 90 kB. Isso é o que 8 Mbps comporta (80 kB × 10 ≈ 6,4 Mbps,
mais o FEC).

O Parsec, numa rede local, costuma trabalhar com bem mais banda (a configuração dele aceita dezenas de Mbps); a taxa que
ele usou na comparação deve ser anotada nas estatísticas dele. Se for bem acima de 8 Mbps, esta é parte direta da
diferença em quadros por segundo.

### 3. O ritmo da captura deriva e pula quadros da tela

O laço da captura dorme até completar 1/60 s desde o início do quadro anterior (`crates/session/src/host.rs:1160-1168`).
Mesmo com o timer de 1 ms, cada `sleep` acorda um pouco depois, e o período fica em 16,7 ms mais esse atraso. O
Desktop Duplication entrega a tela na cadência do monitor, também 60 Hz. Com um período de captura sempre um pouco maior
que o da tela, a cada 15 a 20 quadros um quadro da tela é pulado: o visualizador recebe cerca de 57 quadros por segundo,
com um intervalo de 33 ms a cada um quarto de segundo. É exatamente a micro-travada que aparece ao rolar uma página ou
arrastar uma janela, mesmo com tudo "a 60". A idade da imagem capturada também oscila de 0 a 16 ms.

O Parsec captura na cadência da tela. Esta causa é dedução pelo código; os logs atuais não a mostram, porque contam
quadros por segundo, não o intervalo entre eles.

### 4. Latência somada no codificador e no visualizador

- **Codificador:** `Session::encode` volta assim que o MFT pede nova entrada (`crates/codec/src/mf.rs:647-651`). Um MFT
  assíncrono, como o do Intel Quick Sync, costuma pedir a próxima entrada antes de entregar a saída da atual. Nesse caso o
  quadro N só sai junto com o N+1, 16,7 ms depois, e a linha `captura:` conta isso como "seguradas"
  (`crates/session/src/host.rs:1243-1245`). Se "seguradas" ficar perto de "codificadas" no log, **todo quadro sai um
  quadro atrasado**.
- **Visualizador:** cada quadro decodificado na GPU é copiado para uma textura de leitura, mapeado (o que espera a GPU
  terminar), convertido de NV12 para I420 na CPU e enviado de novo em três texturas (`crates/codec/src/mf_decode.rs:267`,
  `crates/codec/src/d3d11.rs:60-86`, `crates/render/src/lib.rs:451`). É o caminho GPU → memória → CPU → GPU que o Parsec
  proíbe. Em 1080p, são cerca de 3 MB por quadro em cada sentido; a estimativa é de 3 a 8 ms por quadro, a confirmar com
  os tempos que o visualizador já registra (`timed("upload")` em `apps/hext/src/viewer.rs:740` e o tempo de decodificação
  em `Stats`).

Somando o pior caso desses dois pontos com a captura, o Hext pode acrescentar de 20 a 40 ms antes mesmo da rede, contra
os cerca de 7 ms do Parsec.

### 5. O controle de congestionamento segura o vídeo a cada perda

Os datagramas do QUIC obedecem à janela do Cubic, que corta a janela pela metade a cada perda. No Wi-Fi, as perdas vêm em
rajadas sem que a rede esteja realmente cheia. Quando a janela fecha, `send_datagram_wait` espera
(`crates/net/src/video.rs:517`), o canal de um quadro enche (`crates/session/src/host.rs:684`) e a captura para. As
medições mostram o efeito: segundos "presos no controle de congestionamento" (21,4 s em 80 s na oitava medição) e
keyframes de 200 kB levando de 0,5 a 2,6 s com a janela entre 28 e 66 kB.

O Parsec usa um controle próprio, feito para vídeo, que lê vários sinais (não só a perda), antecipa o congestionamento e
responde baixando a taxa do codificador. O Hext responde por segundo e só depois de dois segundos ruins
(`crates/net/src/rate.rs:15-35`), e o codificador Intel ignora a mudança de taxa ao vivo.

A recuperação de perdas também pesa: um quadro faltando segura os seguintes por até 600 ms (`GIVE_UP_AFTER`, até 48
quadros), e o último recurso é um keyframe completo. Na rede boa isso quase não acontece (terceira medição), mas no Wi-Fi
do host é o que produz as paradas longas.

## Mudanças priorizadas

Cada item traz o ganho esperado, o custo, o risco e como medir antes e depois. Os itens 1 a 3 não mudam o comportamento
e devem vir primeiro: sem eles, as demais mudanças não têm como ser comparadas.

**Já aplicado (novembro de 2026, antes da F7):**

- **Item 2, em parte:** o maior intervalo entre quadros, contando só enquanto a imagem se move (de 250 ms para cima é tela
  parada), e quantos passaram de 25 ms em cada segundo. No host, no fim da linha `captura:`; no visualizador, numa linha
  `apresentação:` (as duas com `RUST_LOG=hext_video=debug`). O instante da captura dentro do `VideoFrame` ainda não entrou:
  muda o protocolo e fica para quando a medição pedir.
- **Item 4:** cada prazo da captura vem do prazo anterior (`próximo += intervalo`), não do instante em que o quadro
  começou; depois de uma tela parada, conta de novo a partir do quadro que a quebrou. O atraso de cada `sleep` não se
  acumula mais, e a 60 por segundo a captura acompanha a tela de 60 Hz. O limite continua valendo num monitor de 144 Hz.

Os itens 1 e 3 continuam pedindo uma rodada em máquinas reais (o roteiro está no fim).

### 1. Igualar o caminho e o cenário da comparação

- **O quê:** repetir Hext e Parsec com o host no cabo (ou, ao menos, os dois na mesma sub-rede), o Tailscale desligado em
  um dos lados e a mesma resolução nos dois programas. Conferir no título da janela do Hext e na linha `caminho:` que o
  caminho é P2P direto com endereço local, não `100.x` nem relay.
- **Ganho esperado:** pode ser o maior de todos; o RTT do túnel chegou a 470 ms.
- **Custo:** nenhum código. **Risco:** nenhum.
- **Medir:** RTT mediano e máximo (`envio:`), tempo preso, quadros por segundo; no Parsec, latência e taxa das
  estatísticas dele.

### 2. Medir a latência de ponta a ponta, por fora e por dentro

- **Por fora (vale para o Parsec também):** um cronômetro com milissegundos na tela do host e as duas telas filmadas
  juntas pelo celular em câmera lenta (240 quadros por segundo dão resolução de 4 ms). Dez leituras por programa, com o
  mesmo gesto. É a única comparação justa com o Parsec, que é uma caixa preta.
- **Por dentro:** inserir no `VideoFrame` o instante da captura (relógio do host) e, no visualizador, registrar chegada
  do último pacote, fim da decodificação, fim do upload e apresentação. A diferença de relógio sai do RTT (metade dele) ou
  basta comparar intervalos de cada lado. Uma linha `latência:` por segundo, com mediana e p95 de cada etapa.
- **Inserir também:** o maior intervalo entre quadros capturados e entre quadros apresentados, por segundo. Quadros por
  segundo médios escondem a micro-travada do item 4.
- **Ganho:** nenhum direto; é o que permite decidir os demais. **Custo:** baixo (o protocolo ganha um campo, o que exige
  build igual nos dois lados, como já é a regra). **Risco:** baixo.

### 3. Conferir se o codificador entrega um quadro atrasado

- **O quê:** ler `seguradas` na linha `captura:` de uma rodada com movimento contínuo, no Intel do host de teste e no
  AMD. Se o valor for próximo ao de `codificadas`, mudar `Session::encode` para, depois do `ProcessInput`, esperar
  alguns milissegundos pela saída desse quadro mesmo quando o MFT já pediu nova entrada.
- **Ganho esperado:** até 16,7 ms de latência por quadro, se o atraso existir.
- **Custo:** baixo. **Risco:** baixo; cuidado com o prazo da espera para não travar um driver lento.
- **Medir:** `seguradas` antes e depois; tempo de codificação; latência de ponta a ponta.

### 4. Captura na cadência da tela

- **O quê:** quando o limite de quadros por segundo for igual ou maior que a frequência do monitor, remover o `sleep` do
  laço e deixar o `AcquireNextFrame` ditar o ritmo. Para limites menores, calcular o próximo prazo a partir do prazo
  anterior (`próximo += intervalo`), não do instante em que o quadro começou, para que o atraso do `sleep` não se acumule.
- **Ganho esperado:** fim da micro-travada periódica ao rolar e arrastar; 60 quadros por segundo de fato; idade da imagem
  mais estável.
- **Custo:** baixo (poucas linhas em `crates/session/src/host.rs:1160-1168`). **Risco:** baixo; num monitor de 144 Hz o
  host passaria a codificar mais de 60 quadros por segundo, o que pede manter o limite nesse caso.
- **Medir:** maior intervalo entre quadros capturados (item 2) e contagem de intervalos acima de 25 ms por segundo.

### 5. Taxa inicial pelo caminho, com teto maior na rede local

- **O quê:** num caminho direto com RTT baixo (por exemplo, abaixo de 10 ms), abrir o codificador em 20 a 30 Mbps; pela
  internet, manter os 8 Mbps ou medir a banda no início. Como o Intel ignora a mudança de taxa ao vivo e reabrir custa de
  100 a 300 ms, **a taxa de abertura é a que vale** nesse codificador. O controle de taxa continua só para baixo.
- **Ganho esperado:** quadros por segundo mantidos ao maximizar e arrastar janelas (a queda para 7 a 11 da terceira
  medição); imagem mais nítida.
- **Custo:** baixo a médio (a taxa precisa ser decidida depois que o caminho é conhecido, antes de abrir o codificador).
  **Risco:** médio no Wi-Fi: quadros maiores perdem mais pacotes em rajada; keyframes maiores. Testar no Wi-Fi do host
  antes de adotar como padrão.
- **Medir:** `captura:` (imagens novas e codificadas por segundo) com a mesma ação de estresse; taxa e perda em `envio:`.

### 6. Visualizador sem passar pela CPU (Windows)

- **Etapa A, barata:** enviar o NV12 como está, em duas texturas (Y em `R8Unorm`, UV em `Rg8Unorm`), com o shader lendo
  NV12. Remove a conversão para I420 na CPU e uma das três cópias. Vale também para o VideoToolbox, que já entrega NV12.
- **Etapa B, completa:** o decodificador entrega a textura D3D11 e o `wgpu` a usa diretamente, por uma textura
  compartilhada (handle NT) aberta no dispositivo do `wgpu`, sem leitura para a memória. É o que o Parsec faz.
- **Ganho esperado:** de 3 a 8 ms por quadro (a confirmar pelo item 2) e menos CPU no visualizador; a espera do `Map`
  pela GPU some.
- **Custo:** A baixo; B alto (interoperabilidade D3D11 com o `wgpu`, que usa D3D12 ou Vulkan; sincronização entre os
  dois dispositivos). **Risco:** A baixo; B médio, com diferenças entre drivers, e precisa de recuo para o caminho atual.
- **Medir:** tempos de `decode` e `upload` já registrados; uso de CPU do visualizador; latência de ponta a ponta.

### 7. Controle de congestionamento próprio para o vídeo

- **O quê:** o iroh aceita um controlador próprio (`congestion_controller_factory` em `iroh/src/endpoint/quic.rs:420`).
  Implementar um baseado em atraso (crescimento do RTT, como o GCC do WebRTC), que não corte a janela pela metade a cada
  perda isolada, e ligar o controle de taxa a ele em janelas de 100 a 250 ms, não de um segundo. É o equivalente ao BUD
  sem abandonar o QUIC, que continua servindo ao controle, à entrada e aos arquivos.
- **Ganho esperado:** fim dos segundos "presos no controle de congestionamento" no Wi-Fi; resposta à rede em frações de
  segundo.
- **Custo:** alto (algoritmo, testes com perda e atraso simulados). **Risco:** alto: um controle agressivo demais
  congestiona a rede do cliente e prejudica outros usos; o BBR já testado travou um keyframe por 11 s na quinta medição.
- **Medir:** tempo preso, RTT e perda em `envio:`, paradas até o keyframe, em rodadas pareadas com o Cubic no mesmo Wi-Fi
  (alternando A/B, como nas medições 6 a 8).
- **Fazer só depois dos itens 1 a 6**, e só se o item 2 mostrar que a rede ainda é a maior parcela.

### 8. Recuperação de perda sem keyframe completo

- **O quê:** em vez de um IDR de 200 a 300 kB, usar intra-refresh (o keyframe espalhado por vários quadros) ou quadros de
  referência de longo prazo (`CODECAPI_AVEncVideoLTRBufferControl` no Media Foundation; nativo no NVENC e no AMF): o
  visualizador informa o último quadro bom e o codificador referencia esse quadro. Com isso, desistir de um quadro fica
  barato, e a espera de 600 ms com até 48 quadros parados pode cair para algo próximo de 2 a 3 RTTs.
- **Ganho esperado:** fim das paradas de 0,5 a 2,6 s depois de perda em rajada.
- **Custo:** alto e dependente do fabricante (o Intel ignora várias configurações ao vivo, como as medições mostraram).
  **Risco:** médio; precisa de `hext encoder-rates` estendido para descobrir o que cada driver aceita.
- **Medir:** "tempo parado até o keyframe" e tamanho dos quadros de recuperação.

### 9. Ajustes menores

- **FEC adaptativo:** 20% fixos custam banda numa rede sem perda (terceira medição: 0,05%). Ajustar a paridade pela perda
  medida no último segundo, com um mínimo. Ganho pequeno; custo baixo; risco de piorar as rajadas, que o FEC já não cobre.
- **Datagrama maior no caminho direto:** 1100 bytes servem ao Tailscale (MTU 1280), mas num caminho direto sem túnel
  caberiam cerca de 1400, com 20% menos pacotes. Ganho pequeno; risco de perda silenciosa se o caminho mudar.
- **Mac (para a F7 e o host Mac):** a captura copia o quadro para a memória (`crates/capture/src/macos.rs:84-143`) e o
  codificador copia de novo para o pool do VideoToolbox. Entregar o `IOSurface` direto ao codificador é o zero-copy do
  Mac. Fora do problema relatado no Windows.

## O que não recomendo agora

- **Escrever um UDP próprio como o BUD.** O próprio Parsec relata o custo: meses de erros de NAT, firewall e conexão, com
  85% dos usuários vendo erro num dia em certa fase. Os datagramas do QUIC já são UDP sem retransmissão automática, com
  criptografia; o que o BUD tem de diferente, o controle de congestionamento, cabe dentro do QUIC (item 7).
- **Desligar o FEC ou o reenvio para "ganhar latência".** Na rede boa eles quase não agem; na ruim, são o que evita o
  keyframe.
- **Aumentar filas.** O Parsec opera sem buffer de vídeo; o Hext já segue essa linha no visualizador e deve manter.

## Roteiro da próxima medição

1. Itens 1 e 2: mesma sub-rede, sem Tailscale, host no cabo; Hext e Parsec com o mesmo gesto, filmados.
2. Ler `seguradas` (item 3) e o maior intervalo entre quadros: `captura:` no host e `apresentação:` no visualizador.
3. Repetir no Wi-Fi do host, para separar o efeito da rede.
4. Registrar aqui, como as medições anteriores: latência de ponta a ponta de cada programa, quadros por segundo, maior
   intervalo, RTT, taxa e o que se viu na tela.

Com esses números, a decisão entre os itens 4 a 8 deixa de ser estimativa.
