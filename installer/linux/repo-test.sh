#!/bin/bash
# Inside a clean container (Ubuntu or Fedora): a package built with the
# repository (`--variant repo`) subscribes itself, the system upgrades it from
# that repository (signed metadata, and for dnf signed packages), and
# removing it removes the subscription. The repository is a local one
# (repo.sh build), mounted at /repo, and the package points there.
#
#   docker run --rm -v "$PWD:/src:ro" -v "$PWD/old:/old:ro" -v "$PWD/site:/repo:ro" ubuntu:24.04 \
#     bash /src/installer/linux/repo-test.sh /old/hext-remote_X-1_amd64.deb X-2
set -euo pipefail

package="$1"
want="$2"
fail() { echo "repo-test: $*"; exit 1; }

case "$package" in
  *.deb)
    export DEBIAN_FRONTEND=noninteractive
    sources=/etc/apt/sources.list.d/hext-remote.sources
    keyring=/usr/share/keyrings/hext-remote.gpg
    apt-get update -qq
    apt-get install -y -qq "$package" >/dev/null
    [ -f "$sources" ] || fail "o pacote não inscreveu $sources"
    [ -f "$keyring" ] || fail "o pacote não trouxe $keyring"
    cat "$sources"
    # A bad or unknown signature is only a warning for apt: any line about
    # our repository fails the test.
    apt-get update 2>&1 | tee update.log
    if grep -E '^(W|E|Err):' update.log | grep -q '/repo/'; then fail "apt update reclamou do repositório"; fi
    apt-cache policy hext-remote
    apt-get install -y -qq --only-upgrade hext-remote >/dev/null
    got="$(dpkg-query -W -f '${Version}' hext-remote)"
    [ "$got" = "$want" ] || fail "versão $got depois de atualizar, esperada $want"
    [ -f "$sources" ] || fail "a atualização tirou $sources"
    apt-get remove -y -qq hext-remote >/dev/null
    [ ! -e "$sources" ] || fail "remover deixou $sources"
    [ ! -e "$keyring" ] || fail "remover deixou $keyring"
    apt-get update -qq
    ;;
  *.rpm)
    repo=/etc/yum.repos.d/hext-remote.repo
    key=/etc/pki/rpm-gpg/RPM-GPG-KEY-hext-remote
    dnf install -y -q "$package" >/dev/null
    [ -f "$repo" ] || fail "o pacote não trouxe $repo"
    [ -f "$key" ] || fail "o pacote não trouxe $key"
    cat "$repo"
    # repo_gpgcheck and gpgcheck: dnf imports the key from the package (-y)
    # and checks the metadata and the package with it.
    dnf upgrade -y hext-remote
    got="$(rpm -q --qf '%{VERSION}-%{RELEASE}' hext-remote)"
    [ "$got" = "$want" ] || fail "versão $got depois de atualizar, esperada $want"
    # (rpm 6 puts the signature on the line after the label.)
    signature="$(rpm -qi hext-remote | grep -A1 '^Signature')"
    echo "$signature"
    grep -q 'Key ID' <<<"$signature" || fail "o pacote do repositório veio sem assinatura"
    rpm -q gpg-pubkey >/dev/null || fail "a chave não foi importada"
    [ -f "$repo" ] || fail "a atualização tirou $repo"
    dnf remove -y -q hext-remote >/dev/null
    [ ! -e "$repo" ] || fail "remover deixou $repo"
    [ ! -e "$key" ] || fail "remover deixou $key"
    ;;
  *) fail "pacote desconhecido: $package" ;;
esac
echo "ok: repositório ($(basename "$package") -> $want)"
