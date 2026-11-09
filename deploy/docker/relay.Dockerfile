# iroh-relay for the Hext Remote server with Docker (see compose.yml).
FROM rust:1.97.0-slim-bookworm AS build
ARG IROH_RELAY_VERSION=1.4.1
RUN cargo install iroh-relay --version "$IROH_RELAY_VERSION" --features server --locked --root /out

FROM debian:bookworm-slim
# The system CAs: iroh-relay's HTTP client refuses to start without them.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /out/bin/iroh-relay /usr/local/bin/iroh-relay
COPY iroh-relay.toml /etc/hext/iroh-relay.toml.in
COPY relay-start.sh /usr/local/bin/relay-start.sh
EXPOSE 3340 7842/udp
# As root, to read the key Caddy writes as root (mode 600), but without any
# capability, with a read-only filesystem and only the folder of the relay's
# own certificate mounted (compose.yml).
ENTRYPOINT ["/bin/sh", "/usr/local/bin/relay-start.sh"]
