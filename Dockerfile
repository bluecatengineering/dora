FROM rust:1.72.0 as builder
# set workdir
WORKDIR /usr/src/dora
COPY . .
# setup sqlx-cli
RUN cargo install sqlx-cli
RUN sqlx database create
RUN sqlx migrate run
# release build
ARG BUILD_MODE=release
RUN cargo build --${BUILD_MODE} --bin dora

# run
FROM ubuntu:latest
RUN apt-get -qq update; \
    apt-get -qq --no-install-recommends install \
    dumb-init \
    isc-dhcp-server \
    iputils-ping \
    iproute2 \
    ca-certificates \
    wget \
    sudo;

ARG BUILD_MODE=release
COPY --from=builder /usr/src/dora/target/${BUILD_MODE}/dora /usr/local/bin/dora

RUN mkdir -p /var/lib/dora/

COPY util/entrypoint.sh /entrypoint.sh
ENTRYPOINT ["/entrypoint.sh"]
