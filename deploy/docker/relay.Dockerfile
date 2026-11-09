# iroh-relay for the Hext Remote server with Docker (see compose.yml).
FROM rust:1.97.0-slim-bookworm AS build
ARG IROH_RELAY_VERSION=1.4.1
RUN cargo install iroh-relay --version "$IROH_RELAY_VERSION" --features server --locked --root /out

FROM debian:bookworm-slim
COPY --from=build /out/bin/iroh-relay /usr/local/bin/iroh-relay
COPY iroh-relay.toml /etc/hext/iroh-relay.toml.in
COPY relay-start.sh /usr/local/bin/relay-start.sh
EXPOSE 3340 7842/udp
# As root, to read Caddy's certificate in its volume (mounted read only).
ENTRYPOINT ["/bin/sh", "/usr/local/bin/relay-start.sh"]
