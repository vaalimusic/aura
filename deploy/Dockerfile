FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY apps ./apps
COPY crates ./crates
COPY everty-ui ./everty-ui
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p aura-node && cp target/release/aura-node /aura-node

FROM debian:bookworm-slim
RUN groupadd --gid 10001 aura && useradd --uid 10001 --gid aura --no-create-home aura \
    && mkdir -p /var/lib/aura-node && chown aura:aura /var/lib/aura-node
COPY --from=build /aura-node /usr/local/bin/aura-node
USER 10001:10001
VOLUME ["/var/lib/aura-node"]
EXPOSE 45071/tcp
ENTRYPOINT ["/usr/local/bin/aura-node"]
