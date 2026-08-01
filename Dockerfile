# syntax=docker/dockerfile:1.7

ARG RUST_VERSION=1.88
ARG DOCKER_CLI_VERSION=29

FROM rust:${RUST_VERSION}-alpine AS builder
WORKDIR /build

RUN apk add --no-cache build-base musl-dev perl

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked && \
    cp /build/target/release/arrman /tmp/arrman

FROM docker:${DOCKER_CLI_VERSION}-cli AS runtime

ARG VERSION=dev
LABEL org.opencontainers.image.title="Arrman" \
      org.opencontainers.image.description="Compose-aware operations console for self-hosted media services" \
      org.opencontainers.image.source="https://github.com/kevinelliott/arrman" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.version="${VERSION}"

COPY --from=builder /tmp/arrman /usr/local/bin/arrman
RUN ln -s /usr/local/bin/arrman /usr/local/bin/docker-services

EXPOSE 9876
ENTRYPOINT ["/usr/local/bin/arrman"]
CMD ["--help"]
