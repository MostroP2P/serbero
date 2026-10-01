# syntax=docker/dockerfile:1
#
# Serbero container image. Build: docker build -t serbero .
# Run: see deploy/compose.yml and the README "Run with Docker" section.

FROM rust:1-slim-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY messages ./messages
# The binary is copied out of the cache mount, which is not part of the layer.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --bin serbero \
    && cp target/release/serbero /usr/local/bin/serbero

FROM debian:trixie-slim
# ca-certificates: TLS to relays and the judge verifies against the system
# roots (rustls-platform-verifier). sqlite3: the README's inspection recipes.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates sqlite3 \
    && rm -rf /var/lib/apt/lists/*
# A fixed, unprivileged UID and GID so the /data volume's ownership is
# predictable.
RUN groupadd --system --gid 10001 serbero \
    && useradd --system --uid 10001 --gid 10001 --home-dir /data --no-create-home serbero \
    && mkdir -p /data /etc/serbero \
    && chown serbero:serbero /data
COPY --from=build /usr/local/bin/serbero /usr/local/bin/serbero
COPY config.sample.toml /etc/serbero/config.sample.toml

# The default db_path, "serbero.db", is relative to the working directory, so
# the database lands in the /data volume.
WORKDIR /data
VOLUME /data
ENV SERBERO_CONFIG=/etc/serbero/config.toml
USER serbero

LABEL org.opencontainers.image.title="serbero" \
      org.opencontainers.image.description="Helps the parties of a Mostro dispute resolve it themselves, and escalates to a human solver when needed" \
      org.opencontainers.image.source="https://github.com/MostroP2P/serbero" \
      org.opencontainers.image.licenses="MIT"

# Serbero only makes outbound connections: no port to expose.
ENTRYPOINT ["/usr/local/bin/serbero"]
