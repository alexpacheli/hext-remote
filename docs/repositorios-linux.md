# Repositórios Linux (apt e dnf)

Com os repositórios, o Hext Remote instalado por `.deb` ou `.rpm` passa a ser atualizado junto com o sistema
(`apt upgrade`, `dnf upgrade`, a Central de Programas, o GNOME Software), como o Chrome e o VS Code. O
`.AppImage` fica de fora: segue com o aviso de versão nova no próprio app.

## Como funciona

- **Os pacotes se inscrevem sozinhos.** O `.deb`, ao instalar, grava `/etc/apt/sources.list.d/hext-remote.sources`
  (formato deb822) e copia a chave para `/etc/apt/keyrings/hext-remote.gpg`, para onde aponta o `Signed-By`;
  remover o pacote apaga os dois (uma atualização, não). O `.rpm` traz `/etc/yum.repos.d/hext-remote.repo`
  (`gpgcheck=1`, `repo_gpgcheck=1`, `metadata_expire=6h`) e a chave em
  `/etc/pki/rpm-gpg/RPM-GPG-KEY-hext-remote`; remover o pacote apaga os dois arquivos, mas a chave que o dnf
  importou fica no banco do rpm (veja [Para quem já tem o pacote](#para-quem-já-tem-o-pacote)).
- **Só depois que a chave pública estiver no código.** Enquanto `installer/linux/repo-key.asc` não existir, os
  pacotes saem como antes, sem inscrição: ninguém fica com um repositório quebrado.
- **Só releases publicadas.** O workflow **Linux repositories** (`.github/workflows/repositorios.yml`) roda quando
  uma release é publicada ou quando uma pré-release vira release (nunca um rascunho nem uma pré-release, a mesma
  regra da atualização automática). Ele baixa os `.deb` e `.rpm` da release, confere cada um no `SHA256SUMS.txt`
  e, se a variável `HEXT_UPDATE_KEY` existir, também na lista assinada da atualização automática
  (`latest.json`, conferida como o app confere), monta os dois repositórios, assina e publica no GitHub Pages.
  Os repositórios guardam só a última versão.
- **A chave só no `main`.** Os segredos ficam num ambiente (`linux-repo`) que só aceita o branch `main`: um
  workflow alterado em outro branch não chega até eles.
- **Assinatura.** No apt, o `Release` é assinado (`InRelease` e `Release.gpg`). No dnf, cada `.rpm` é assinado
  (`rpmsign`) e o `repomd.xml` também (`repomd.xml.asc`). Os `.rpm` da página da release continuam sem
  assinatura; só as cópias do repositório são assinadas. Para instalar um arquivo baixado, o dnf não exige
  assinatura.
- **Endereço.** `https://<dono>.github.io/<repositório>` (hoje `https://alexpacheli.github.io/hext-remote`), com
  `apt/` e `rpm/`. A variável do repositório `HEXT_REPO_URL` troca o endereço, para o workflow e para os
  pacotes gerados a partir daí (antes de trocar, leia [Mudar de endereço](#mudar-de-endereço-ou-tornar-o-repositório-privado)).
- **Testado no CI.** A cada build dos instaladores, o `release.yml` cria uma chave descartável, gera pacotes
  inscritos num repositório local, monta o repositório com o mesmo script e, num Ubuntu e num Fedora limpos,
  instala a versão 1, atualiza para a 2 pelo repositório (com as assinaturas conferidas) e remove
  (`installer/linux/repo-test.sh`).

## O que você precisa fazer (uma vez)

A ordem importa: o repositório precisa existir antes de um pacote apontar para ele. Os comandos abaixo são para
o Git Bash, o Linux ou o macOS. No PowerShell, use sempre `--output`, como abaixo, e nunca `>`: o `>` do
Windows PowerShell 5.1 grava em UTF-16, e a chave sai ilegível.

1. **Gerar a chave**, num computador seu com GnuPG (o Git Bash já traz o `gpg`; ou o Gpg4win). Sem validade, ou
   com uma longa (uma chave vencida faz o apt e o dnf recusarem as atualizações até um pacote novo trazer a
   chave renovada):

   ```bash
   gpg --quick-generate-key "Hext Remote <suporte@hext.com.br>" rsa4096 sign never
   gpg --list-secret-keys --keyid-format long   # anote a impressão digital (40 caracteres)
   ```

   Uma frase secreta é opcional; com ela, vai também para um segredo (passo 2).

2. **Criar o ambiente e guardar a parte privada nele.** Em **Settings → Environments → New environment**, crie
   `linux-repo`. Em **Deployment branches and tags**, escolha **Selected branches and tags** e acrescente só
   `main`. Exporte a chave:

   ```bash
   gpg --armor --output chave-privada.asc --export-secret-keys <impressão digital>
   ```

   e, em **Environment secrets** do `linux-repo` (não nos segredos do repositório):

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
   gpg --armor --output installer/linux/repo-key.asc --export <impressão digital>
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

No Fedora, a chave que o dnf importou na primeira atualização fica no banco do rpm depois de remover o pacote.
Para tirá-la também:

```bash
rpm -q gpg-pubkey --qf '%{NAME}-%{VERSION}-%{RELEASE}\t%{SUMMARY}\n' | grep -i hext
sudo rpm -e gpg-pubkey-<versão>-<release>   # o nome da linha acima
```

## Mudar de endereço ou tornar o repositório privado

O GitHub Pages de um repositório privado exige um plano pago. Para tornar o repositório privado, ou só para
sair do GitHub Pages, publique o conteúdo do artefato em outro servidor estático e aponte `HEXT_REPO_URL` para
ele. **A ordem importa**, porque cada computador só conhece o endereço que veio no pacote instalado:

1. Ponha o endereço novo no ar e mude `HEXT_REPO_URL`.
2. Publique uma release (gerada com o endereço novo) **também no endereço antigo**, e mantenha o antigo
   servindo por um bom tempo (meses): é por ele que os computadores recebem o pacote que os leva ao novo.
3. Só então desligue o GitHub Pages (ou torne o repositório privado).

Quem não atualizar a tempo fica com uma inscrição quebrada (o `apt update` e o `dnf` reclamam) e precisa
apagar à mão `/etc/apt/sources.list.d/hext-remote.sources` ou `/etc/yum.repos.d/hext-remote.repo` e
reinstalar o pacote.

## Limites

- O GitHub Pages tem limites de uso (cerca de 100 GB de tráfego por mês e sites de até 1 GB); para os pacotes
  de hoje, sobra.
- Ao publicar uma release, quem estiver no meio de um `apt update` pode ver por alguns minutos um arquivo que já
  não existe (só a última versão fica no repositório); a próxima atualização resolve.
- Sem a variável `HEXT_UPDATE_KEY`, os pacotes são conferidos só pelo `SHA256SUMS.txt`, que vem da própria
  release: protege contra um download corrompido, não contra quem consiga trocar os arquivos da release.
- O GNOME Software (PackageKit) pode pedir, na primeira atualização pelo repositório no Fedora, para confiar na
  chave do Hext Remote, por causa do `repo_gpgcheck`; o `dnf upgrade` pergunta o mesmo (`-y` aceita). Ainda não
  conferido numa máquina real.
