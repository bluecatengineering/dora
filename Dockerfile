FROM rust:1.72.0 as builder

WORKDIR /usr/src/dora
COPY . .

RUN cargo build --release --bin dora

FROM ubuntu:latest
RUN apt-get -qq update; \
    apt-get -qq --no-install-recommends install \
    ca-certificates \
    wget \
    sudo;
COPY --from=builder /usr/src/dora/target/release/dora /usr/local/bin/dora

RUN mkdir -p /var/dora/lib/

ARG DORA_CONFIG=config.yaml
COPY ${DORA_CONFIG} /var/dora/lib/config.yaml

CMD ["dora"]
