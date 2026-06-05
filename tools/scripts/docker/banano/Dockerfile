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
RUN cargo chef cook --release --features banano --recipe-path recipe.json
COPY . .
RUN cargo build --release --bin rsban --features banano

FROM alpine:3.23
RUN apk update && apk add --no-cache gosu
RUN adduser -D bananocurrency bananocurrency
COPY --from=builder /app/target/release/rsban /usr/bin
COPY tools/scripts/docker/banano/config /usr/share/banano/config
COPY tools/scripts/docker/banano/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh
RUN ldconfig
RUN mkdir -p /home/bananocurrency/Banano /home/bananocurrency/BananoBeta /home/bananocurrency/BananoTest 
WORKDIR /home/bananocurrency
ENV PATH="${PATH}:/usr/bin"
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["node", "run"]

ARG REPOSITORY=rsnano-node/rsnano-node
LABEL org.opencontainers.image.source https://github.com/$REPOSITORY
