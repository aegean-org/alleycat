FROM rust:1-bookworm AS builder

RUN cargo install iroh-relay --version 0.98.0 --features server --locked

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/cargo/bin/iroh-relay /usr/local/bin/iroh-relay

RUN useradd --system --uid 65532 --home-dir /nonexistent --shell /usr/sbin/nologin relay

USER 65532:65532
EXPOSE 8080/tcp 9090/tcp

ENTRYPOINT ["iroh-relay"]
