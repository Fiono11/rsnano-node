FROM alpine:3.23 as builder

# Install build dependencies
COPY ./tools/scripts/ci/prepare/linux /tmp/prepare
RUN /tmp/prepare/prepare-alpine.sh

COPY ./ /tmp/src
WORKDIR /tmp/src

# Build node
ENV PATH="/root/.cargo/bin:${PATH}"
RUN cargo build --release --bin rsban --features banano

# Run node
FROM alpine:3.23

RUN apk update && apk add --no-cache gosu

RUN adduser -D bananocurrency bananocurrency

COPY --from=builder /tmp/src/target/release/rsban /usr/bin

COPY tools/scripts/docker/banano/config /usr/share/banano/config
COPY tools/scripts/docker/banano/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh
RUN ldconfig

RUN mkdir -p /home/bananocurrency/Banano && \
	chown -R bananocurrency:bananocurrency /home/bananocurrency
VOLUME /home/bananocurrency/Banano

WORKDIR /home/bananocurrency

ENV PATH="${PATH}:/usr/bin"
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["node", "run"]

ARG REPOSITORY=rsnano-node/rsnano-node
LABEL org.opencontainers.image.source https://github.com/$REPOSITORY
