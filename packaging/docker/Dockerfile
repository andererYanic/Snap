# Snap als Gerät ohne Oberfläche (z. B. auf dem eigenen Server).
# Bauen (im Wurzelverzeichnis des Repos):  docker build -f packaging/docker/Dockerfile -t snap .
FROM rust:1.97-trixie AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p space-daemon \
 && strip target/release/space-daemon

FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates tini \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --user-group --home-dir /data --shell /usr/sbin/nologin snap \
 && mkdir -p /data && chown snap:snap /data
COPY --from=build /src/target/release/space-daemon /usr/local/bin/space-daemon
COPY packaging/docker/entrypoint.sh /usr/local/bin/snap-entrypoint
# Alles Veränderliche liegt im Volume /data (Schlüssel, Datenbank, Spiegel, Downloads).
ENV SPACE_CONFIG=/data/config.toml \
    SPACE_DATA_DIR=/data/state \
    SPACE_SOCKET=/data/state/space.sock \
    SPACE_PIN_DIR=/data/Snap \
    SPACE_DOWNLOAD_DIR=/data/Downloads
USER snap
VOLUME /data
# Optional: UDP-Port für direkte Verbindungen (ohne geht alles über Relais und Lochstanzen).
EXPOSE 11204/udp
HEALTHCHECK --interval=60s --timeout=10s --start-period=30s CMD ["space-daemon", "info"]
ENTRYPOINT ["tini", "--", "snap-entrypoint"]
