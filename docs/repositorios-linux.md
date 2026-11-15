# Repositórios Linux (apt e dnf)

Com os repositórios, o Hext Remote instalado por `.deb` ou `.rpm` passa a ser atualizado junto com o sistema
(`apt upgrade`, `dnf upgrade`, a Central de Programas, o GNOME Software), como o Chrome e o VS Code. O
`.AppImage` fica de fora: segue com o aviso de versão nova no próprio app.

## Como funciona

- **Os pacotes se inscrevem sozinhos.** O `.deb` traz a chave em `/usr/share/keyrings/hext-remote.gpg` e, ao
  instalar, grava `/etc/apt/sources.list.d/hext-remote.sources` (formato deb822, `Signed-By` apontando para a
  chave). O `.rpm` traz `/etc/yum.repos.d/hext-remote.repo` (`gpgcheck=1`, `repo_gpgcheck=1`) e a chave em
  `/etc/pki/rpm-gpg/RPM-GPG-KEY-hext-remote`. Remover o pacote remove a inscrição e a chave.
- **Só depois que a chave pública estiver no código.** Enquanto `installer/linux/repo-key.asc` não existir, os
  pacotes saem como antes, sem inscrição: ninguém fica com um repositório quebrado.
- **Só releases publicadas.** O workflow **Linux repositories** (`.github/workflows/repositorios.yml`) roda quando
  uma release é publicada (nunca um rascunho nem uma pré-release, a mesma regra da atualização automática). Ele
  baixa os `.deb` e `.rpm` da release, confere o `SHA256SUMS.txt`, monta os dois repositórios, assina e publica
  no GitHub Pages. Os repositórios guardam só a última versão.
- **Assinatura.** No apt, o `Release` é assinado (`InRelease` e `Release.gpg`). No dnf, cada `.rpm` é assinado
  (`rpmsign`) e o `repomd.xml` também (`repomd.xml.asc`). Os `.rpm` da página da release continuam sem
  assinatura; só as cópias do repositório são assinadas. Para instalar um arquivo baixado, o dnf não exige
  assinatura.
- **Endereço.** `https://<dono>.github.io/<repositório>` (hoje `https://alexpacheli.github.io/hext-remote`), com
  `apt/` e `rpm/`. A variável do repositório `HEXT_REPO_URL` troca o endereço, para o workflow e para os
  pacotes gerados a partir daí.
- **Testado no CI.** A cada build dos instaladores, o `release.yml` cria uma chave descartável, gera pacotes
  inscritos num repositório local, monta o repositório com o mesmo script e, num Ubuntu e num Fedora limpos,
  instala a versão 1, atualiza para a 2 pelo repositório (com as assinaturas conferidas) e remove
  (`installer/linux/repo-test.sh`).

## O que você precisa fazer (uma vez)

A ordem importa: o repositório precisa existir antes de um pacote apontar para ele.

1. **Gerar a chave**, num computador seu com GnuPG (no Windows, o Gpg4win). Sem validade, ou com uma longa (uma
   chave vencida faz o apt e o dnf recusarem as atualizações até um pacote novo trazer a chave renovada):

   ```bash
   gpg --quick-generate-key "Hext Remote <suporte@hext.com.br>" rsa4096 sign never
   gpg --list-secret-keys --keyid-format long   # anote a impressão digital (40 caracteres)
   ```

   Uma frase secreta é opcional; com ela, vai também para um segredo (passo 2).

2. **Guardar a parte privada nos segredos** do GitHub (**Settings → Secrets and variables → Actions**):

   ```bash
   gpg --armor --export-secret-keys <impressão digital> > chave-privada.asc
   ```

   - `HEXT_REPO_GPG_KEY`: o conteúdo inteiro de `chave-privada.asc` (de `-----BEGIN` a `-----END`).
   - `HEXT_REPO_GPG_PASSPHRASE`: a frase secreta, se houver.

   Depois apague `chave-privada.asc` e guarde uma cópia da chave num lugar seguro, fora do computador: sem ela,
   uma chave nova obriga cada usuário a reinstalar o pacote.

3. **Ligar o GitHub Pages:** **Settings → Pages → Build and deployment → Source: GitHub Actions**.

4. **Publicar o repositório pela primeira vez:** **Actions → Linux repositories → Run workflow** (no `main`, sem
   tag). Ele publica a última release; confira que `https://alexpacheli.github.io/hext-remote/apt/dists/stable/InRelease`
   abre.

5. **Pôr a chave pública no código**, num pull request:

   ```bash
   gpg --armor --export <impressão digital> > installer/linux/repo-key.asc
   ```

   O workflow confere que a chave do segredo é a mesma deste arquivo. A partir da próxima release, os pacotes
   se inscrevem nos repositórios.

Sem `HEXT_REPO_GPG_KEY`, o workflow termina com um aviso e não publica nada; a release não é afetada.

## Para quem já tem o pacote

Quem instalou uma versão anterior à primeira inscrita recebe a inscrição ao instalar essa versão (baixada da
release ou pelo aviso do app). Daí em diante, as atualizações vêm pelo sistema.

Para desligar o repositório sem remover o app: no Debian e no Ubuntu, acrescente `Enabled: no` em
`/etc/apt/sources.list.d/hext-remote.sources` (as atualizações do pacote passam a deixar o arquivo como está);
no Fedora, `enabled=0` em `/etc/yum.repos.d/hext-remote.repo` (arquivo de configuração: o rpm mantém a
alteração).

## Limites

- **Repositório privado:** o GitHub Pages de um repositório privado exige um plano pago. Nesse caso, ou para
  sair do GitHub Pages, publique o conteúdo do artefato em outro servidor estático e aponte `HEXT_REPO_URL` para
  ele (os pacotes já instalados continuam no endereço antigo até a próxima versão, que traz o novo).
- O GitHub Pages tem limites de uso (cerca de 100 GB de tráfego por mês e sites de até 1 GB); para os pacotes
  de hoje, sobra.
- Ao publicar uma release, quem estiver no meio de um `apt update` pode ver por alguns minutos um arquivo que já
  não existe (só a última versão fica no repositório); a próxima atualização resolve.
- O GNOME Software pode pedir, na primeira atualização pelo repositório no Fedora, para confiar na chave do Hext
  Remote; o `dnf upgrade` pergunta o mesmo (`-y` aceita).
