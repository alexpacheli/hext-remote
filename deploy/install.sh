#!/usr/bin/env bash
# Installs (or updates) the Hext Remote services on a Debian or Ubuntu
# server: hext-server and iroh-relay behind Caddy. Run from the repository
# root, as a user with sudo, with your domain (the names signal.<domain> and
# relay.<domain> must already point to this server):
#
#   HEXT_DOMAIN=example.org ./deploy/install.sh
#
# (Or each name by itself: SIGNAL_HOST=… RELAY_HOST=… ./deploy/install.sh.)
#
# Optionally, a second relay without the per-device limit, for the accounts
# listed in /etc/hext/relay-livre.txt (one e-mail per line):
#
#   RELAY_FULL_HOST=relay-full.example.org ./deploy/install.sh
#
# (It is kept too; RELAY_FULL_HOST= ./deploy/install.sh takes it out.)
# The names are kept in /etc/hext/hosts.env, so to update after a `git pull`
# it is just ./deploy/install.sh: it rebuilds, keeps the database and the
# secret, and restarts the services.
set -euo pipefail

cd "$(dirname "$0")/.."
IROH_RELAY_VERSION=1.4.1
HOSTS=/etc/hext/hosts.env

step() { printf '\n==> %s\n' "$*"; }

step "Names"
if [ -z "${HEXT_DOMAIN:-}${SIGNAL_HOST:-}${RELAY_HOST:-}" ]; then
    if sudo test -f "$HOSTS"; then
        SIGNAL_HOST=$(sudo sed -n 's/^SIGNAL_HOST=//p' "$HOSTS")
        RELAY_HOST=$(sudo sed -n 's/^RELAY_HOST=//p' "$HOSTS")
    elif grep -qs '^signal\.hext\.com\.br ' /etc/caddy/Caddyfile; then
        # Installed before the names were a setting: the names it had.
        SIGNAL_HOST=signal.hext.com.br
        RELAY_HOST=relay.hext.com.br
    fi
fi
SIGNAL_HOST=${SIGNAL_HOST:-${HEXT_DOMAIN:+signal.$HEXT_DOMAIN}}
RELAY_HOST=${RELAY_HOST:-${HEXT_DOMAIN:+relay.$HEXT_DOMAIN}}
for name in "$SIGNAL_HOST" "$RELAY_HOST"; do
    if ! [[ $name =~ ^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$ && $name == *.* ]]; then
        echo "Informe o domínio: HEXT_DOMAIN=exemplo.com.br ./deploy/install.sh" >&2
        echo "(os nomes signal.<domínio> e relay.<domínio> devem apontar para este servidor)" >&2
        exit 1
    fi
done
# The relay without the limit: the one given, else the one kept (an empty
# value given takes it out).
if [ -z "${RELAY_FULL_HOST+set}" ] && sudo test -f "$HOSTS"; then
    RELAY_FULL_HOST=$(sudo sed -n 's/^RELAY_FULL_HOST=//p' "$HOSTS")
fi
RELAY_FULL_HOST=${RELAY_FULL_HOST:-}
if [ -n "$RELAY_FULL_HOST" ] &&
    ! [[ $RELAY_FULL_HOST =~ ^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$ && $RELAY_FULL_HOST == *.* ]]; then
    echo "RELAY_FULL_HOST inválido: $RELAY_FULL_HOST" >&2
    exit 1
fi
echo "sinalização: $SIGNAL_HOST"
echo "relay:       $RELAY_HOST"
echo "relay sem limite: ${RELAY_FULL_HOST:-(nenhum)}"

step "vnstat (the month's traffic, for deploy/uso.sh)"
if ! command -v vnstat >/dev/null; then
    sudo apt-get install -y vnstat
fi
sudo systemctl enable --now vnstat

step "Caddy"
if ! command -v caddy >/dev/null; then
    sudo apt-get install -y debian-keyring debian-archive-keyring apt-transport-https curl gnupg
    curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' |
        sudo gpg --dearmor --yes -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
    curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' |
        sudo tee /etc/apt/sources.list.d/caddy-stable.list >/dev/null
    sudo apt-get update
    sudo apt-get install -y caddy
fi

step "Building hext-server (a few minutes)"
if ! command -v cargo >/dev/null; then
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi
cargo build --release --locked -p hext-server
sudo install -m 755 target/release/hext-server /usr/local/bin/hext-server

step "iroh-relay $IROH_RELAY_VERSION"
if ! /usr/local/bin/iroh-relay --version 2>/dev/null | grep -q "$IROH_RELAY_VERSION"; then
    cargo install iroh-relay --version "$IROH_RELAY_VERSION" --features server --locked \
        --root "$HOME/.local/iroh-relay"
    sudo install -m 755 "$HOME/.local/iroh-relay/bin/iroh-relay" /usr/local/bin/iroh-relay
fi

step "Configuration"
sudo mkdir -p /etc/hext
printf 'SIGNAL_HOST=%s\nRELAY_HOST=%s\nRELAY_FULL_HOST=%s\n' "$SIGNAL_HOST" "$RELAY_HOST" "$RELAY_FULL_HOST" |
    sudo tee "$HOSTS" >/dev/null
if ! sudo test -f /etc/hext/secrets.env; then
    # The secret the relay presents to hext-server when asking about a device.
    token=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
    printf 'HEXT_RELAY_TOKEN=%s\nIROH_RELAY_HTTP_BEARER_TOKEN=%s\n' "$token" "$token" |
        sudo tee /etc/hext/secrets.env >/dev/null
    sudo chmod 600 /etc/hext/secrets.env
fi
# The names into the files, in place of {$SIGNAL_HOST}, {$RELAY_HOST} and
# {$RELAY_FULL_HOST}.
names() {
    sed -e "s/{\$SIGNAL_HOST}/$SIGNAL_HOST/g" -e "s/{\$RELAY_HOST}/$RELAY_HOST/g" \
        -e "s/{\$RELAY_FULL_HOST}/$RELAY_FULL_HOST/g" "$1"
}
names deploy/iroh-relay.toml | sudo tee /etc/hext/iroh-relay.toml >/dev/null
names deploy/Caddyfile | sudo tee /etc/caddy/Caddyfile >/dev/null
sudo chmod 644 /etc/hext/iroh-relay.toml /etc/caddy/Caddyfile
sudo install -m 644 deploy/hext-server.service /etc/systemd/system/hext-server.service
sudo install -m 644 deploy/iroh-relay.service /etc/systemd/system/iroh-relay.service
if [ -n "$RELAY_FULL_HOST" ]; then
    names deploy/iroh-relay-full.toml | sudo tee /etc/hext/iroh-relay-full.toml >/dev/null
    names deploy/relay-full.caddy | sudo tee /etc/caddy/hext-relay-full.caddy >/dev/null
    sudo chmod 644 /etc/hext/iroh-relay-full.toml /etc/caddy/hext-relay-full.caddy
    sudo install -m 644 deploy/iroh-relay-full.service /etc/systemd/system/iroh-relay-full.service
    printf 'HEXT_RELAY_FULL_URL=https://%s\n' "$RELAY_FULL_HOST" |
        sudo tee /etc/hext/relay-full.env >/dev/null
    if ! sudo test -f /etc/hext/relay-livre.txt; then
        printf '%s\n' "# Contas que usam o relay sem limite: um e-mail por linha." \
            "# Editar vale na hora, sem reiniciar nada." | sudo tee /etc/hext/relay-livre.txt >/dev/null
    fi
    sudo chmod 644 /etc/hext/relay-livre.txt
else
    sudo systemctl disable --now iroh-relay-full 2>/dev/null || true
    sudo rm -f /etc/caddy/hext-relay-full.caddy /etc/hext/relay-full.env
fi
sudo systemctl daemon-reload

step "Firewall (80, 443 TCP; 7842 UDP, and 7843 for the relay without the limit; IPv4 and IPv6)"
# Oracle's images reject what is not listed: allow before the REJECT rule.
allow() {
    for table in iptables ip6tables; do
        command -v "$table" >/dev/null || continue
        if ! sudo "$table" -C INPUT -p "$1" --dport "$2" -j ACCEPT 2>/dev/null; then
            reject=$(sudo "$table" -L INPUT --line-numbers -n | awk '/REJECT/ {print $1; exit}')
            sudo "$table" -I INPUT "${reject:-1}" -p "$1" --dport "$2" -j ACCEPT
        fi
    done
}
allow tcp 80
allow tcp 443
allow udp 7842
if [ -n "$RELAY_FULL_HOST" ]; then
    allow udp 7843
fi
# The port of the manual test, if it is still open.
sudo iptables -D INPUT -p tcp --dport 8080 -j ACCEPT 2>/dev/null || true
if command -v netfilter-persistent >/dev/null; then
    sudo netfilter-persistent save
fi

step "Services"
# The old relay (on 80/443 by itself) gives way to Caddy.
sudo systemctl stop iroh-relay 2>/dev/null || true
sudo systemctl enable --now caddy
sudo systemctl reload caddy
sudo systemctl enable --now hext-server
sudo systemctl restart hext-server
sudo systemctl enable --now iroh-relay
sudo systemctl restart iroh-relay
if [ -n "$RELAY_FULL_HOST" ]; then
    sudo systemctl enable --now iroh-relay-full
    sudo systemctl restart iroh-relay-full
fi

step "Checks"
# On the first run Caddy needs a moment to get the certificates, and the
# relay restarts until they exist: retry for up to 90 s.
check() {
    for _ in $(seq 18); do
        if curl -fsS --max-time 10 "$1" >/dev/null 2>&1; then
            echo "ok    $1"
            return
        fi
        sleep 5
    done
    curl -fsS --max-time 10 "$1" >/dev/null || true
    echo "FALHA $1"
}
check "https://$SIGNAL_HOST/health"
check "https://$RELAY_HOST/generate_204"
if [ -n "$RELAY_FULL_HOST" ]; then
    check "https://$RELAY_FULL_HOST/generate_204"
fi
systemctl --no-pager --lines=0 status caddy hext-server iroh-relay | grep -E '●|Active'
echo
echo "Logs: journalctl -u hext-server -u iroh-relay -u caddy -f"
echo "Uso e tráfego: ./deploy/uso.sh"
