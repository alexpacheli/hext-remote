# Decisões

[English](DECISIONS.md) | **Português**

As escolhas que dão forma ao Hext Remote, e por quê. A discussão completa de cada uma está nos planos das fases
([planos/](planos/)). Decisões novas entram no fim da sua seção.

## Produto

**Consentimento e transparência, nunca um modo oculto.** Toda sessão mostra no host uma barra com quem está
conectado e um botão para encerrar, e cada acesso fica registrado no computador. O acesso não assistido é
configurado uma vez, pelo dono. Não há como esconder uma sessão, dispensar o consentimento ou instalar escondido
sem o dono saber. *Por quê:* é uma ferramenta de suporte, não de vigilância. Recursos para esconder seriam
stalkerware, ilegal em muitos lugares, e marcado pelos antivírus.
([plano original, 7.4](planos/plano-original.md#74-consentimento-e-transparência-princípio-do-produto))

**Software livre, sob a AGPL-3.0-only** (novembro de 2026, versão 0.6.1). *Por quê:* qualquer um pode ler, rodar e
mudar, e quem oferecer um servidor modificado pela rede precisa publicar essa versão também.

**Os instaladores do Windows não têm assinatura de código** (novembro de 2026). O SmartScreen avisa na primeira
abertura. *Por quê:* desde 2023, um certificado novo de assinatura de código só vem num token físico ou num HSM na
nuvem, e o custo anual ainda não se justifica. O app do macOS é assinado e notarizado (Apple Developer ID),
porque o Gatekeeper bloqueia apps sem assinatura.

**Linux primeiro como técnico; ser acessado depois (F7).** O app do Linux acessa outros computadores, com a mesma
interface; ainda não é host. *Por quê:* no Wayland cada
captura de tela abre um pedido de permissão, o que impede o acesso não assistido; Windows e macOS cobrem
primeiro os casos de suporte.

## Rede e servidor

**`iroh` em vez de uma travessia de NAT própria** (F3). As conexões são QUIC pelo [`iroh`](https://iroh.computer):
diretas quando possível, pelo nosso próprio `iroh-relay` senão. *Por quê:* um spike entre duas redes reais mostrou
conexão direta e um relay aceitável, enquanto hole punching, relay e alternativa por TCP feitos por nós
levariam muito mais tempo. Nenhum serviço de terceiros é usado para achar os computadores.
([F3, seção 2](planos/F3.md#2-a-decisão-principal-travessia-de-nat-própria-ou-iroh))

**SQLite, não PostgreSQL** (F3). *Por quê:* uma VM roda tudo; um banco em arquivo basta para os IDs e as contas e
é mais simples de copiar e de hospedar por conta própria.

**Quem decide quem entra é o host, nunca o servidor** (F5). A conta é uma lista de dispositivos em que cada
entrada é assinada por um dispositivo que já está nela; o host guarda a lista e a confere. *Por quê:* um servidor
invadido ou um banco vazado não abre nenhum computador. Custa mais trabalho do que confiar no servidor.
([F5, seção 2.3](planos/F5.md#23-quem-decide-é-o-host-não-o-servidor))

**Contas: e-mail, senha e TOTP, gerenciadas só no app** (F5). *Por quê:* a entrada de sempre, com um segundo
fator, e sem um painel web para proteger. Passkeys podem vir depois.

**Uma cópia corrigida do `netwatch` (o monitor de rede do iroh), sem WMI** (0.8.1).
No Windows, o `netwatch` 0.19.3 achava a rota padrão com uma consulta WMI, via COM, na thread de cada endpoint.
Com vários endpoints abrindo ao mesmo tempo, o COM do processo usava memória já liberada: os testes de
sinalização travavam ou caíam no runner Windows do CI, e sob page heap toda execução parava dentro dessas
consultas. A cópia em `vendor/netwatch` (por `[patch.crates-io]`) lê a tabela de rotas do IP Helper e devolve o
mesmo valor; nada mais nela mudou. *Por quê:* o defeito estava no uso do COM pela dependência, e preparar o COM
por fora não o resolveu. Para largar a cópia, o `netwatch` precisa deixar o WMI de vez (a 0.20.0 ainda usa).

<a id="mac-sem-tela-de-login"></a>**Mac: sem tela de login** (novembro de 2026, 0.8.2). No Mac, o Hext roda na
conta do usuário (um LaunchAgent): depois de reiniciar, volta quando alguém entra na conta. Um LaunchDaemon do
sistema com agentes por sessão alcançaria antes a lista de usuários, mas com o FileVault ligado (o padrão do macOS)
o Mac para antes numa senha pedida antes de o macOS carregar, onde nenhum programa roda. Alguém no local digita
essa senha de qualquer jeito. *Por quê:* muito código nativo, que só compila e roda num Mac, para um ganho que o
FileVault quase anula. As atualizações não são afetadas: depois de uma, o macOS entra sozinho na conta uma vez e
bloqueia a tela, e o Hext está lá. O desenho fica no [F6](planos/F6.md#31-marco-3-o-que-ficou-pronto-e-o-que-falta),
caso Macs sem FileVault peçam.

## App

**Rust, um executável nativo.** Sem runtime, .NET ou Java. *Por quê:* download pequeno para um atendimento rápido,
baixa latência e o mesmo código nos três sistemas.

**Tauri na interface, janela nativa na sessão** (F4). *Por quê:* a interface sai rápido em HTML; o vídeo precisa do
`wgpu` sem um webview no meio, e uma janela de sessão travada não pode derrubar a interface.

**A interface nunca roda como SYSTEM** (F4). No Windows, o serviço a abre como o usuário logado e fala com ela por
um pipe que só os dois abrem. *Por quê:* um webview com direitos do sistema é um caminho de escalada de
privilégio.

**O ID da máquina quando instalado** (F4). O serviço instalado usa um ID do computador, mantido numa
reinstalação; o app portátil usa o do usuário. *Por quê:* a lista do técnico não pode mudar quando outra pessoa
entra no computador.

**Sem administrador no Mac** (F6). O app do Mac guarda a senha de acesso e os dispositivos vinculados na pasta do
usuário. *Por quê:* quem já roda como esse usuário poderia controlar a tela de qualquer forma; pedir sudo não
protegeria mais nada. No Windows, o serviço é do computador inteiro, por isso pede administrador.

**Só H.264, por enquanto.** *Por quê:* toda placa de vídeo decodifica. HEVC e AV1 podem vir depois, negociados
pelo que os dois lados suportam.

**Atualizações assinadas com uma chave nossa** (F6). Cada release tem uma lista de versões assinada com uma chave
Ed25519 guardada como segredo do repositório, e o app só aceita essa assinatura. As releases nascem em rascunho,
e uma pessoa as publica. *Por quê:* o GitHub só entrega os arquivos; trocar um deles não passa da conferência.
Funciona igual com ou sem assinatura de código nos instaladores.

**Monitor virtual por um driver de código aberto, só quando pedido** (0.7.0). O Windows usa o driver MttVDD
(licença MIT), uma opção desmarcada no instalador (ou `service virtual-display install`). Os arquivos dele vão
dentro do `.msi`, fixados pelo SHA-256, e o certificado do driver é conferido pela impressão digital. O macOS
usa o `CGVirtualDisplay`, uma API privada carregada em tempo de execução. O monitor só existe durante a sessão.
*Por quê:* atender computadores sem monitor sem o adaptador HDMI emulador, sem confiar num certificado novo sem
o dono escolher e sem deixar um monitor fantasma para trás. ([monitor-virtual.md](monitor-virtual.md))

## Processo

**Estudar outras ferramentas, nunca copiar o código delas.** *Por quê:* o RustDesk (AGPL) e o Sunshine/Moonlight
(GPL) são boas referências, mas código copiado traria junto as licenças e a história deles.

**Idiomas.** Os comentários no código são em inglês. A interface é em português, por enquanto. O README, a
arquitetura, o guia de servidor próprio e este arquivo são em inglês e português; os planos das fases, os estudos
e os roteiros de teste, em português.
