# Atendimento de campo (F4, marco 13)

Um atendimento de verdade, do começo ao fim, com dois computadores em redes diferentes. Serve para fechar a
F4: tudo que os marcos 1 a 12 entregaram, usado na ordem de um suporte real. Marque cada passo e anote o que
estranhar (tempo, mensagem confusa, clique a mais).

**Quem é quem:**
- **Técnico:** o computador que acessa, com o app aberto (instalado ou não).
- **Cliente:** o computador atendido, Windows com o serviço instalado (`HextRemote.exe service install`,
  como administrador).

Para o marco valer, o cliente deve estar **fora da rede do técnico** (outra casa, 4G, etc.): assim a conexão
passa pelo servidor e pelo relay como num atendimento de verdade.

## Antes de começar

- [ ] O servidor está na versão da `main` (na VM: `cd remote-control && git pull && ./deploy/install.sh`).
- [ ] Os dois computadores têm o mesmo build do app.
- [ ] No cliente, `HextRemote.exe service status` mostra "rodando", o ID e o nome.
- [ ] No cliente há um arquivo pequeno para mandar ao técnico (ex.: `Documentos\relatorio.txt`).
- [ ] No técnico há um arquivo para mandar ao cliente (ex.: um instalador ou um `.txt`).

## 1. Pedido por ID e aceite

1. [ ] O cliente passa o ID (e o nome que aparece na tela inicial) por telefone ou mensagem.
2. [ ] O técnico digita o ID em **Acessar outro computador** e clica **Conectar**, sem senha. **Sessões
   abertas** mostra "aguardando aceitar".
3. [ ] No cliente aparece a janela de aceite com o nome e o ID do técnico. O botão **Aceitar** só fica
   disponível depois de um instante.
4. [ ] O cliente marca **Área de transferência**, **Arquivos** e **Reiniciar** e aceita.
5. [ ] Abre a janela da sessão no técnico. No cliente, a barra no alto mostra quem está conectado.
6. [ ] O computador aparece em **Computadores acessados**, com a bolinha verde.

## 2. UAC e `Ctrl+Alt+Del`

1. [ ] O técnico abre algo que pede administrador no cliente (ex.: **Gerenciador de Dispositivos** pelo Painel
   de Controle, ou `regedit`). O prompt do UAC aparece **na janela do técnico**, e ele consegue clicar em
   **Sim** (ou digitar a senha do administrador, se o cliente quiser: quem digita a senha é o cliente).
2. [ ] **Ctrl+Alt+End** no técnico abre a tela do `Ctrl+Alt+Del` no cliente. **Esc** volta.
3. [ ] O cliente bloqueia o computador (Win+L): o técnico continua vendo a tela de bloqueio, e a sessão não cai.
   O cliente desbloqueia.

## 3. Arquivos e área de transferência

1. [ ] O técnico arrasta o arquivo para a janela da sessão: ele chega em **Downloads** do cliente, e a barra
   mostra o andamento.
2. [ ] O cliente usa **Enviar arquivo** na barra e escolhe `relatorio.txt`: o técnico confirma e recebe o
   arquivo em Downloads.
3. [ ] O técnico copia um texto e cola no Bloco de Notas do cliente, e o contrário também funciona.

## 4. Chat

1. [ ] O técnico abre o chat (balão ou **Ctrl+Alt+C**) e escreve: aparece no cliente.
2. [ ] O cliente responde pela barra (**Chat**): chega ao técnico.

## 5. Reiniciar e voltar

1. [ ] O técnico avisa no chat e aperta **Ctrl+Alt+R** duas vezes em até 5 segundos.
2. [ ] O cliente reinicia. No técnico, a janela mostra que espera a volta.
3. [ ] Depois do boot, **antes de alguém entrar na conta**, a sessão volta sozinha (meta: menos de 2 minutos
   depois do boot). O técnico vê a tela de login.
4. [ ] O cliente entra na conta: a sessão continua, e a barra volta a aparecer.
5. [ ] O `audit.log` (`C:\ProgramData\HextRemote\audit.log`) registra o pedido de reinício e a retomada.

## 6. Senha e vínculo (opcional, se o cliente usar acesso não assistido)

1. [ ] O cliente liga a senha: aba **Segurança** → **Definir a senha de acesso** (o Windows pede administrador
   e uma janela pede a senha). **Quem digita a senha é o cliente.** Conferir também **Trocar a senha** e
   **Desligar a senha** no mesmo cartão.
2. [ ] O técnico encerra, conecta de novo com a senha (sem ninguém aceitar) e marca **Entrar sem senha da
   próxima vez**.
3. [ ] O técnico encerra e conecta de novo **sem a senha**: entra direto. Na tela inicial do cliente, **Técnicos
   que entram sem senha** lista o técnico.
4. [ ] O cliente clica **Revogar**: o próximo pedido sem senha volta a pedir aceite.

## 7. Vários computadores (opcional)

1. [ ] Com a sessão aberta, o técnico conecta num segundo computador. Cada um fica na sua janela, e **Sessões
   abertas** lista os dois.
2. [ ] O texto copiado numa janela não chega à outra: só a janela usada por último troca área de transferência.

## 8. Encerrar

1. [ ] O cliente clica **Encerrar** na barra: a sessão acaba no técnico com aviso, e a barra some no cliente.
2. [ ] O `audit.log` tem o início e o fim de cada sessão sem aceite.

## Resultado

| Item | Ok? | Observação |
|---|---|---|
| Pedido por ID e aceite | | |
| UAC e `Ctrl+Alt+Del` | | |
| Arquivos (os dois sentidos) | | |
| Área de transferência | | |
| Chat | | |
| Reinício e volta (tempo) | | |
| Senha e vínculo | | |
| Vários computadores | | |

Com tudo ok, o marco 13 fecha. Os testes do Mac instalado (pendências do marco 9, em `F4.md`) fecham a F4.
