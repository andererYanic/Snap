# Eigenes Iroh-Relais für Snap. Version passend zum Daemon (iroh 1.4.1).
FROM rust:1.91-slim-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config && rm -rf /var/lib/apt/lists/*
RUN cargo install iroh-relay --version =1.4.1 --features server --locked --root /out

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /data relay \
    && mkdir -p /data/certs && chown -R relay /data
COPY --from=build /out/bin/iroh-relay /usr/local/bin/iroh-relay
USER relay
WORKDIR /data
# Im Container nur Ports über 1024 (kein root); docker-compose bildet 80/443 darauf ab.
EXPOSE 8080/tcp 8443/tcp 7842/udp
ENTRYPOINT ["iroh-relay", "--config-path", "/etc/iroh-relay/relay.toml"]
