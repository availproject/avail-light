FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app

LABEL org.opencontainers.image.source=https://github.com/availproject/avail-light

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json

ARG BUILD_PROFILE=release
ENV BUILD_PROFILE $BUILD_PROFILE

RUN apt-get update && apt-get -y upgrade && apt-get install -y cmake libclang-dev

RUN cargo chef cook --profile $BUILD_PROFILE --recipe-path recipe.json

COPY . .
RUN cargo build --profile $BUILD_PROFILE --locked --bin avail-light

RUN cp /app/target/$BUILD_PROFILE/avail-light /app/avail-light

FROM ubuntu as run
WORKDIR /app
RUN apt-get update && apt-get install -y ca-certificates

COPY --from=builder /app/config.yaml /app/config.yaml
COPY --from=builder /app/avail-light /usr/local/bin

EXPOSE 80 443 7001 37000
CMD ["/bin/bash", "-c", "while true; do /usr/local/bin/avail-light -c /app/config.yaml; sleep 20; done"]
