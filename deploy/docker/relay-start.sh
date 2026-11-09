#!/bin/sh
# Writes the relay's name into its configuration and starts it. Until Caddy
# has the first certificate the relay cannot start; Docker restarts it.
set -eu
case "$RELAY_HOST" in
    *[!A-Za-z0-9.-]* | "" ) echo "RELAY_HOST inválido: $RELAY_HOST" >&2; exit 1 ;;
esac
sed "s/{\$RELAY_HOST}/$RELAY_HOST/g" /etc/hext/iroh-relay.toml.in > /tmp/iroh-relay.toml
exec iroh-relay --config-path /tmp/iroh-relay.toml
