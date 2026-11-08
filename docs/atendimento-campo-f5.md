# Atendimento de campo (F5, marco 8)

As contas usadas de verdade, do começo ao fim: criar a conta, pôr dois computadores e um técnico nela, entrar
sem aceite, compartilhar com outra conta, revogar e ver a atividade. Serve para fechar a F5. Marque cada passo
e anote o que estranhar (tempo, mensagem confusa, clique a mais).

**Quem é quem:**
- **Notebook do dono:** o computador de onde o dono acessa, com o app aberto.
- **Escritório** e **Casa:** dois computadores Windows do dono, com o serviço instalado
  (`HextRemote.exe service install`, como administrador). Um deles fora da rede do notebook (outra casa,
  4G), para a conexão passar pelo servidor e pelo relay.
- **Técnico:** um computador com outra conta (outro e-mail), para o compartilhamento.

## Antes de começar

- [ ] O servidor está na versão da `main` (na VM: `cd remote-control && git pull && ./deploy/install.sh`).
- [ ] O servidor manda e-mail de verdade: `/etc/hext/mail.env` com o SMTP da Brevo e o log dizendo "SMTP:
      conexão e login ok" ([hext-server.md](hext-server.md), "E-mail"). Sem isso, o código de confirmação
      está no log do servidor (`journalctl -u hext-server | grep código`).
- [ ] Os quatro computadores têm o mesmo build do app.
- [ ] Um app autenticador no celular (Google Authenticator, 1Password, Authy...).

## 1. Criar a conta

1. [ ] No notebook, **Conta → Criar conta**: e-mail e senha (**quem digita a senha é o dono**). Chega o código
   de 6 dígitos; com ele, a conta fica confirmada.
2. [ ] Entrar. Ligar a **verificação em duas etapas**: ler o QR com o autenticador, digitar o código e guardar
   os 10 códigos de recuperação.
3. [ ] **Sair** e entrar de novo: pede a senha e o código do autenticador.
4. [ ] Em **Dispositivos**, **Criar a lista com este computador**. Guardar o código de recuperação da lista
   (aparece uma vez só).

## 2. Pôr os computadores na conta

Em cada um, Escritório e Casa:

1. [ ] Entrar na conta (senha e código do autenticador).
2. [ ] **Colocar este computador na conta**: aparece um campo para o código. No notebook, **Adicionar outro
   dispositivo** mostra um código de 12 caracteres; digitá-lo no computador. Em até 5 segundos o notebook
   mostra o pedido e aceita; o computador aparece em **Dispositivos da conta**.
3. [ ] **Pôr este computador em Meus computadores**. Ele aparece em **Computadores da conta**, e no notebook,
   na tela inicial, em **Meus computadores**, com a bolinha verde.
4. [ ] No notebook, um clique no computador em Meus computadores conecta. **Ainda pede aceite** (falta o
   vínculo).

## 3. Entrar sem aceite (vínculo)

1. [ ] No Escritório, em Dispositivos, **Deixar os dispositivos da conta entrarem sem aceite**. O Windows pede
   administrador. Na tela inicial aparece **Conta vinculada** com o número de dispositivos.
2. [ ] Conferir pela linha de comando: `HextRemote.exe service account` mostra a lista vinculada.
3. [ ] No notebook, clicar no Escritório em Meus computadores: **entra sem ninguém aceitar**. A barra no
   Escritório mostra quem está conectado, também com a tela bloqueada.
4. [ ] O `C:\ProgramData\HextRemote\audit.log` do Escritório tem "sessão iniciada (entrou como dono)".
5. [ ] A Casa, sem vínculo, continua pedindo aceite.

## 4. Tirar um dispositivo

1. [ ] Com a sessão do notebook aberta no Escritório, tirar o notebook da lista a partir da Casa
   (**Dispositivos da conta → Remover**).
2. [ ] Em até 30 segundos a sessão cai, e o `audit.log` diz "sessão encerrada: ... não está mais na conta".
3. [ ] O notebook volta à lista: **Colocar este computador na conta**, com um código de **Adicionar outro
   dispositivo** na Casa ou com a chave de recuperação guardada no passo 1.4. Depois disso, entra sem aceite
   de novo.

## 5. Compartilhar com outra conta

1. [ ] No técnico, criar a outra conta (passos 1.1 a 1.4) e copiar o **ID da lista** em Dispositivos. Mandar ao
   dono por mensagem.
2. [ ] No notebook, em **Computadores da conta**, **Compartilhar** no Escritório: colar o ID, nome
   `tecnico@...`, marcar **Controlar** e **Arquivos**, validade **1 dia**.
3. [ ] No técnico, o Escritório aparece em **Compartilhados com esta conta** e em Meus computadores, com
   "(compartilhado)" e bolinha.
4. [ ] O técnico conecta: entra sem aceite, pode controlar e mandar arquivo; **não** consegue área de
   transferência nem reiniciar. O `audit.log` diz "por compartilhamento com tecnico@...".
5. [ ] O dono clica **Revogar**: em até 30 segundos a sessão do técnico cai, e o próximo pedido volta a pedir
   aceite.

## 6. Atividade

1. [ ] No notebook, **Dispositivos → Atividade → Ver os acessos**: aparecem as sessões dos passos 3 a 5, com
   data, hora e "Escritório".
2. [ ] No técnico (outra conta), a atividade do Escritório **não** aparece.

## 7. Desligar

1. [ ] Na tela inicial do Escritório, **Desligar a entrada sem aceite** (administrador). **Conta vinculada**
   some, e o notebook volta a pedir aceite.
2. [ ] `HextRemote.exe service account` diz "Nenhuma conta vinculada".

## Resultado

| Item | Ok? | Observação |
|---|---|---|
| Criar a conta, 2FA, sair e entrar | | |
| Lista de dispositivos e código de recuperação | | |
| Computadores na conta, online, um clique | | |
| Entrar sem aceite (vínculo) | | |
| Dispositivo removido derruba a sessão | | |
| Compartilhar, permissões, revogar | | |
| Atividade | | |
| Desligar o vínculo | | |

Com tudo ok, a F5 fecha. No Mac, a entrada sem aceite pela conta chega com o LaunchDaemon (F6).
