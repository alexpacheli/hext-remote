#!/usr/bin/env bash
# How much the Hext Remote server is used: the network traffic of the month
# (vnstat) against the provider's limit, and the relay since it started (its
# counters on 127.0.0.1:9090). Works with install.sh and with Docker.
#
#   ./deploy/uso.sh
#   HEXT_LIMITE_GB=5000 ./deploy/uso.sh     # another monthly limit (default: 10 TB, Oracle's)
set -euo pipefail

limit_gb=${HEXT_LIMITE_GB:-10000}
gb() { awk -v b="$1" 'BEGIN { printf "%.2f GB", b / 1e9 }'; }

echo "Tráfego deste mês (saída, o que o provedor cobra):"
# The interface of the default route: the one to the internet (not docker0).
iface=$(ip route show default 2>/dev/null | awk '{ for (i = 1; i < NF; i++) if ($i == "dev") { print $(i + 1); exit } }' || true)
if command -v vnstat >/dev/null && line=$(vnstat --oneline b ${iface:+-i "$iface"} 2>/dev/null) && [[ $line == *";"* ]]; then
    # Fields: version;interface;today;rx;tx;total;rate;month;rx;tx;...
    sent=$(cut -d';' -f10 <<<"$line")
    echo "  $(gb "$sent") de $limit_gb GB ($(awk -v s="$sent" -v l="$limit_gb" 'BEGIN { printf "%.1f", s / (l * 1e7) }')%)"
else
    echo "  sem dados ainda (o vnstat conta a partir da instalação: sudo apt install vnstat)"
fi

echo
echo "Relay (desde que iniciou):"
if ! metrics=$(curl -fsS --max-time 5 http://127.0.0.1:9090/metrics 2>/dev/null); then
    echo "  sem os contadores em 127.0.0.1:9090 (o relay está rodando?)"
    exit 1
fi
m() { awk -v k="relayserver_$1" '$1 == k { printf "%.0f", $2; found = 1 } END { if (!found) print 0 }' <<<"$metrics"; }
echo "  repassado:                       $(gb "$(m bytes_sent_total)")"
echo "  dispositivos conectados agora:   $(( $(m accepts_total) - $(m disconnects_total) ))"
echo "  dispositivos diferentes:         $(m unique_client_keys_total)"
echo "  conexões que bateram no limite:  $(m conns_rx_ratelimited_total)"

# The relay without the limit, if this server has it (install.sh with
# RELAY_FULL_HOST).
if metrics=$(curl -fsS --max-time 5 http://127.0.0.1:9091/metrics 2>/dev/null); then
    echo
    echo "Relay sem limite (desde que iniciou):"
    echo "  repassado:                       $(gb "$(m bytes_sent_total)")"
    echo "  dispositivos conectados agora:   $(( $(m accepts_total) - $(m disconnects_total) ))"
    echo "  dispositivos diferentes:         $(m unique_client_keys_total)"
fi
