#!/bin/bash
# Our apt and dnf repositories, which the Linux packages update from
# (docs/repositorios-linux.md).
#
#   repo.sh url                     the base URL: HEXT_REPO_URL, or this
#                                   repository's GitHub Pages
#   repo.sh files <arch> <out>      what a package ships to subscribe itself:
#                                   hext-remote.sources and hext-remote.gpg
#                                   (.deb), hext-remote.repo and
#                                   RPM-GPG-KEY-hext-remote (.rpm), from the
#                                   public key HEXT_REPO_KEY (default
#                                   installer/linux/repo-key.asc)
#   repo.sh variant <arch>          with that key: the files into
#                                   target/linux-repo and "--variant repo" on
#                                   stdout, for cargo deb / generate-rpm;
#                                   without it: nothing
#   repo.sh import                 the armored private key HEXT_REPO_GPG_KEY
#                                   (passphrase HEXT_REPO_GPG_PASSPHRASE, if
#                                   any) into a new GNUPGHOME, ready to sign
#                                   unattended; it must match HEXT_REPO_KEY
#                                   when that file exists
#   repo.sh verify <pkgs>           the .deb and .rpm in <pkgs> against the
#                                   release's SHA256SUMS.txt and, with
#                                   HEXT_UPDATE_KEY (the update manifest's
#                                   public key, hex), against the signed
#                                   latest.json too
#   repo.sh build <pkgs> <out>      <out>/apt and <out>/rpm from the .deb and
#                                   .rpm in <pkgs>, signed with the one secret
#                                   key in GNUPGHOME (the .rpm files too)
#
# Tools: gpg; for build also apt-ftparchive (apt-utils), createrepo_c and
# rpmsign (rpm); for verify, jq and openssl 3.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
die() { echo "repo.sh: $*" >&2; exit 1; }
# Hex text to bytes (printf %b reads \xHH).
hex2bin() { printf '%b' "$(sed 's/../\\x&/g' <<<"$1")"; }

base_url() {
  if [ -n "${HEXT_REPO_URL:-}" ]; then
    echo "${HEXT_REPO_URL%/}"
    return
  fi
  local repo="${GITHUB_REPOSITORY:-alexpacheli/hext-remote}"
  local owner="${repo%%/*}"
  echo "https://${owner,,}.github.io/${repo#*/}"
}

files() {
  local arch="$1" out="$2" deb_arch
  case "$arch" in
    x86_64 | amd64) deb_arch=amd64 ;;
    aarch64 | arm64) deb_arch=arm64 ;;
    *) die "arquitetura desconhecida: $arch" ;;
  esac
  local key="${HEXT_REPO_KEY:-$here/repo-key.asc}"
  [ -f "$key" ] || die "sem a chave pública $key"
  local url
  url="$(base_url)"

  local home
  home="$(mktemp -d)"
  # One public key, nothing secret.
  local keys
  keys="$(gpg --homedir "$home" --batch --with-colons --show-keys "$key")"
  [ "$(grep -c '^pub:' <<<"$keys")" = 1 ] || die "$key deve ter exatamente uma chave"
  grep -q '^sec:' <<<"$keys" && die "$key tem uma chave privada"

  mkdir -p "$out"
  gpg --homedir "$home" --batch --yes --dearmor -o "$out/hext-remote.gpg" "$key"
  rm -rf "$home"
  cp "$key" "$out/RPM-GPG-KEY-hext-remote"

  cat >"$out/hext-remote.sources" <<EOF
# Hext Remote updates, added by the hext-remote package and removed with it.
# To keep it off, add "Enabled: no" below: upgrades then leave this file alone.
Types: deb
URIs: $url/apt
Suites: stable
Components: main
Architectures: $deb_arch
Signed-By: /etc/apt/keyrings/hext-remote.gpg
EOF

  cat >"$out/hext-remote.repo" <<EOF
# Hext Remote updates, installed and removed with the hext-remote package.
[hext-remote]
name=Hext Remote
baseurl=$url/rpm
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-hext-remote
metadata_expire=6h
EOF
}

variant() {
  local out="$here/../../target/linux-repo"
  rm -rf "$out"
  [ -f "${HEXT_REPO_KEY:-$here/repo-key.asc}" ] || return 0
  files "$1" "$out"
  echo "repo.sh: pacotes inscritos em $(base_url)" >&2
  echo "--variant repo"
}

import_key() {
  [ -n "${GNUPGHOME:-}" ] || die "GNUPGHOME não definido"
  [ -n "${HEXT_REPO_GPG_KEY:-}" ] || die "HEXT_REPO_GPG_KEY vazio"
  [ ! -e "$GNUPGHOME" ] || die "$GNUPGHOME já existe"
  mkdir -m 700 "$GNUPGHOME"
  # The passphrase given once to the agent, so rpmsign (which runs gpg
  # itself) signs without asking.
  echo allow-preset-passphrase >"$GNUPGHOME/gpg-agent.conf"
  local pass="${HEXT_REPO_GPG_PASSPHRASE:-}"
  printf '%s' "$pass" >"$GNUPGHOME/.pass"
  printf '%s\n' "$HEXT_REPO_GPG_KEY" |
    gpg --batch --quiet --pinentry-mode loopback --passphrase-file "$GNUPGHOME/.pass" --import
  rm -f "$GNUPGHOME/.pass"

  local secret
  secret="$(gpg --batch --with-colons --with-keygrip --list-secret-keys)"
  [ "$(grep -c '^sec:' <<<"$secret")" = 1 ] || die "HEXT_REPO_GPG_KEY deve ter exatamente uma chave privada"
  local fpr
  fpr="$(awk -F: '/^fpr:/ { print $10; exit }' <<<"$secret")"

  if [ -n "$pass" ]; then
    local preset grip
    preset="$(gpgconf --list-dirs libexecdir)/gpg-preset-passphrase"
    for grip in $(awk -F: '/^grp:/ { print $10 }' <<<"$secret"); do
      printf '%s' "$pass" | "$preset" --preset "$grip"
    done
  fi
  echo test | gpg --batch --local-user "$fpr" --clearsign >/dev/null ||
    die "a chave não assina (frase secreta errada?)"

  local key="${HEXT_REPO_KEY:-$here/repo-key.asc}"
  if [ -f "$key" ]; then
    local public
    public="$(gpg --batch --with-colons --show-keys "$key" | awk -F: '/^fpr:/ { print $10; exit }')"
    [ "$public" = "$fpr" ] || die "a chave de HEXT_REPO_GPG_KEY ($fpr) não é a de $key ($public)"
  else
    echo "repo.sh: sem $key: os pacotes ainda não se inscrevem no repositório"
  fi
  echo "chave $fpr"
}

verify() {
  local dir="$1" f name sum
  shopt -s nullglob
  local packages=("$dir"/*.deb "$dir"/*.rpm)
  shopt -u nullglob
  [ "${#packages[@]}" -gt 0 ] || die "nenhum .deb ou .rpm em $dir"
  [ -f "$dir/SHA256SUMS.txt" ] || die "sem SHA256SUMS.txt"

  local manifest=""
  if [ -n "${HEXT_UPDATE_KEY:-}" ]; then
    # latest.json, signed by hext-release (Ed25519, signature in hex):
    # checked as the app checks it, with the raw public key wrapped as DER.
    [ -f "$dir/latest.json" ] && [ -f "$dir/latest.json.sig" ] ||
      die "a release não tem latest.json assinado (HEXT_UPDATE_KEY está definida)"
    local key tmp
    key="$(tr -d ' \r\n' <<<"$HEXT_UPDATE_KEY")"
    [[ "$key" =~ ^[0-9a-fA-F]{64}$ ]] || die "HEXT_UPDATE_KEY não é uma chave Ed25519 em hex"
    tmp="$(mktemp -d)"
    hex2bin "302a300506032b6570032100$key" >"$tmp/key.der"
    hex2bin "$(tr -d ' \r\n' <"$dir/latest.json.sig")" >"$tmp/sig"
    openssl pkeyutl -verify -pubin -keyform DER -inkey "$tmp/key.der" -rawin \
      -in "$dir/latest.json" -sigfile "$tmp/sig" >/dev/null ||
      { rm -rf "$tmp"; die "a assinatura de latest.json não confere"; }
    rm -rf "$tmp"
    manifest="$dir/latest.json"
  else
    echo "repo.sh: sem HEXT_UPDATE_KEY: conferido só pelo SHA256SUMS.txt" >&2
  fi

  for f in "${packages[@]}"; do
    name="$(basename "$f")"
    sum="$(sha256sum "$f" | cut -d' ' -f1)"
    # Every package listed (no --ignore-missing passing on an empty match).
    awk -v s="$sum" -v n="$name" '$1 == s && ($2 == n || $2 == "*" n) { f = 1 } END { exit !f }' \
      "$dir/SHA256SUMS.txt" ||
      die "$name não confere com SHA256SUMS.txt (ou não está nele)"
    if [ -n "$manifest" ]; then
      jq -e --arg n "$name" --arg s "$sum" \
        '[.files[] | select(.name == $n and .sha256 == $s)] | length == 1' "$manifest" >/dev/null ||
        die "$name não confere com latest.json (ou não está nele)"
    fi
    echo "conferido: $name"
  done
}

build() {
  local src="$1" out="$2"
  local fpr
  fpr="$(gpg --batch --with-colons --list-secret-keys | awk -F: '/^fpr:/ { print $10; exit }')"
  [ -n "$fpr" ] || die "nenhuma chave privada em ${GNUPGHOME:-~/.gnupg}"
  [ "$(gpg --batch --with-colons --list-secret-keys | grep -c '^sec:')" = 1 ] || die "mais de uma chave privada"

  shopt -s nullglob
  local debs=("$src"/*.deb) rpms=("$src"/*.rpm)
  shopt -u nullglob
  [ "${#debs[@]}" -gt 0 ] || die "nenhum .deb em $src"
  [ "${#rpms[@]}" -gt 0 ] || die "nenhum .rpm em $src"

  rm -rf "$out"
  mkdir -p "$out/apt/pool/main/h/hext-remote" "$out/rpm"
  cp "${debs[@]}" "$out/apt/pool/main/h/hext-remote/"
  cp "${rpms[@]}" "$out/rpm/"
  local sign=(gpg --batch --yes --local-user "$fpr" --digest-algo SHA512)

  # apt: one suite (stable), one component (main), both architectures.
  (
    cd "$out/apt"
    for arch in amd64 arm64; do
      local dir="dists/stable/main/binary-$arch"
      mkdir -p "$dir"
      apt-ftparchive --arch "$arch" packages pool >"$dir/Packages"
      gzip -9kn "$dir/Packages"
    done
    apt-ftparchive \
      -o APT::FTPArchive::Release::Origin="Hext Remote" \
      -o APT::FTPArchive::Release::Label="Hext Remote" \
      -o APT::FTPArchive::Release::Suite=stable \
      -o APT::FTPArchive::Release::Codename=stable \
      -o APT::FTPArchive::Release::Architectures="amd64 arm64" \
      -o APT::FTPArchive::Release::Components=main \
      release dists/stable >Release.tmp
    mv Release.tmp dists/stable/Release
    "${sign[@]}" --clearsign -o dists/stable/InRelease dists/stable/Release
    "${sign[@]}" --armor --detach-sign -o dists/stable/Release.gpg dists/stable/Release
  )

  # dnf: both architectures in one repository (dnf picks its own); the
  # packages and the metadata signed.
  local db
  db="$(mktemp -d)"
  gpg --batch --armor --export "$fpr" >"$db/key.asc"
  rpm --dbpath "$db" --import "$db/key.asc"
  for f in "$out"/rpm/*.rpm; do
    rpmsign --addsign \
      --define "__gpg $(command -v gpg)" \
      --define "_gpg_name $fpr" \
      --define "_gpg_path ${GNUPGHOME:-$HOME/.gnupg}" \
      --define "_gpg_digest_algo sha256" \
      "$f" >/dev/null
    rpm --dbpath "$db" --checksig "$f" | grep -q 'digests signatures OK' ||
      die "assinatura de $(basename "$f") não confere"
  done
  rm -rf "$db"
  createrepo_c --quiet --general-compress-type=gz "$out/rpm"
  "${sign[@]}" --armor --detach-sign -o "$out/rpm/repodata/repomd.xml.asc" "$out/rpm/repodata/repomd.xml"

  # The public key, for whoever checks by hand.
  gpg --batch --armor --export "$fpr" >"$out/key.asc"
  cat >"$out/index.html" <<EOF
<!doctype html>
<meta charset="utf-8">
<title>Hext Remote: repositórios Linux</title>
<p>Repositórios apt (<code>apt/</code>) e dnf (<code>rpm/</code>) do Hext Remote.
Os pacotes .deb e .rpm das <a href="https://github.com/${GITHUB_REPOSITORY:-alexpacheli/hext-remote}/releases/latest">releases</a>
já se inscrevem aqui ao instalar. Chave: <a href="key.asc">key.asc</a> (<code>$fpr</code>).</p>
EOF
  echo "repositórios em $out, assinados por $fpr"
}

case "${1:-}" in
  url) base_url ;;
  files) [ $# = 3 ] || die "uso: repo.sh files <arch> <out>"; files "$2" "$3" ;;
  variant) [ $# = 2 ] || die "uso: repo.sh variant <arch>"; variant "$2" ;;
  import) import_key ;;
  verify) [ $# = 2 ] || die "uso: repo.sh verify <pacotes>"; verify "$2" ;;
  build) [ $# = 3 ] || die "uso: repo.sh build <pacotes> <out>"; build "$2" "$3" ;;
  *) die "uso: repo.sh url | files <arch> <out> | variant <arch> | import | build <pacotes> <out>" ;;
esac
