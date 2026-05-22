FROM alpine:3.23 as builder

# Install build dependencies
COPY ./tools/scripts/ci/prepare/linux /tmp/prepare
RUN /tmp/prepare/prepare-alpine.sh

COPY ./ /tmp/src
WORKDIR /tmp/src

# Build node
ENV PATH="/root/.cargo/bin:${PATH}"
RUN cargo build --release --bin rsnano

# Run node
FROM alpine:3.23

RUN apk update && apk add --no-cache gosu

RUN adduser -D nanocurrency nanocurrency

COPY --from=builder /tmp/src/target/release/rsnano /usr/bin

COPY tools/scripts/docker/node/config /usr/share/nano/config
COPY tools/scripts/docker/node/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh
RUN ldconfig

RUN mkdir -p /home/nanocurrency/Nano && \
	chown -R nanocurrency:nanocurrency /home/nanocurrency
VOLUME /home/nanocurrency/Nano

WORKDIR /home/nanocurrency

ENV PATH="${PATH}:/usr/bin"
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["node", "run"]

ARG REPOSITORY=simpago/rsnano-node
LABEL org.opencontainers.image.source https://github.com/$REPOSITORY
