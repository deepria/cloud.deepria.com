FROM rust:1.100.0-trixie@sha256:e84068000361bf3d4a5ba2b7647f62893528b1eb9f847d7a23b493f5ce8ea254 AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY apps/api apps/api
COPY db/migrations db/migrations
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --locked --package cloud-api && \
    strip --strip-unneeded target/release/cloud-api

FROM debian:trixie-slim@sha256:8aa5a7bf5a1955bddbe7d74fcdd0b91ab30e74362a8b73af672b0481b35b44a0
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/cloud-api /usr/local/bin/cloud-api
USER 10001:10001
ENV API_BIND=0.0.0.0:3000
EXPOSE 3000
ENTRYPOINT ["/usr/local/bin/cloud-api"]
