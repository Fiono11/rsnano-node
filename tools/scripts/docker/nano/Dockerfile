FROM lukemathwalker/cargo-chef:latest-rust-alpine AS chef
RUN apk add --no-cache \
    build-base \
    openssl-dev \
    musl-dev \
    cmake \
    linux-headers \
    pkgconfig \
    bash \
    git
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --bin rsnano

FROM alpine:3.23
RUN apk update && apk add --no-cache gosu
RUN adduser -D nanocurrency nanocurrency
COPY --from=builder /app/target/release/rsnano /usr/bin
COPY tools/scripts/docker/nano/config /usr/share/nano/config
COPY tools/scripts/docker/nano/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh
RUN ldconfig
RUN mkdir -p /home/nanocurrency/Nano && \
    chown -R nanocurrency:nanocurrency /home/nanocurrency
VOLUME /home/nanocurrency/Nano
WORKDIR /home/nanocurrency
ENV PATH="${PATH}:/usr/bin"
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["node", "run"]

ARG REPOSITORY=rsnano-node/rsnano-node
LABEL org.opencontainers.image.source https://github.com/$REPOSITORY
