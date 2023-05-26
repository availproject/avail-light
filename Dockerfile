# syntax = docker/dockerfile:1.2

# Phase 0: Builder
# =========================
FROM paritytech/ci-linux:1.69.0-bullseye as builder

# Install needed packages
RUN apt-get update && \
	apt-get install -yqq --no-install-recommends git openssh-client && \
	rm -rf /var/lib/apt/lists

# Install nightly Rust for WASM  & prepare folders
# RUN	rustup toolchain install nightly && \
#	rustup target add wasm32-unknown-unknown --toolchain nightly && \
#	rustup default nightly

# Clone & build node binary.
ARG LC_TAG=v1.4.0
RUN \
	mkdir -p /da/state && \
	mkdir -p /da/keystore && \
	git clone -b $LC_TAG --single-branch https://github.com/availproject/avail-light.git /da/src/ && \
	cd /da/src && \
	cargo build --release 

# Install binaries 
RUN \ 
	mkdir -p /da/bin && \
	mv /da/src/target/release/avail-light /da/bin && \
	# Clean src \
	rm -rf /da/src

# Phase 1: Binary deploy
# =========================
FROM debian:bullseye-slim

RUN \
	apt-get update && \
	apt-get install -y curl gettext-base && \
	rm -rf /var/lib/apt/lists && \
	groupadd -r avail && \
	useradd --no-log-init -r -g avail avail

COPY --chown=avail.avail --from=builder /da/ /da
COPY --chown=avail.avail config.yaml.template /da
COPY --chown=avail.avail --chmod=750 entrypoint.sh /

ENV \
	LC_LOG_LEVEL="INFO" \
	LC_HTTP_SERVER_HOST="127.0.0.1" \
	LC_HTTP_SERVER_PORT="7000" \
	LC_SECRET_KEY_SEED="0" \
	LC_LIBP2P_SEED=1 \
	LC_LIBP2P_PORT="37000" \
	LC_FULL_NODE_RPC="http://127.0.0.1:9933" \
	LC_FULL_NODE_WS="ws://127.0.0.1:9944" \
	LC_APP_ID=1 \
	LC_CONFIDENCE=92.0 \
	LC_AVAIL_PATH="/da/state" \
	LC_PROMETHEUS_PORT=9520 \
	LC_BOOTSTRAPS=""

# Opencontainers annotations
LABEL \
	org.opencontainers.image.authors="The Avail Project Team" \
	org.opencontainers.image.url="https://www.availproject.org/" \
	org.opencontainers.image.documentation="https://github.com/availproject/avail-deployment#readme" \
	org.opencontainers.image.source="https://github.com/availproject/avail-deployment" \
	org.opencontainers.image.version="1.0.0" \
	org.opencontainers.image.revision="1" \
	org.opencontainers.image.vendor="The Avail Project" \
	org.opencontainers.image.licenses="MIT" \
	org.opencontainers.image.title="Avail Light Client" \
	org.opencontainers.image.description="Data Availability Light Client"

# USER avail:avail
WORKDIR /da
VOLUME ["/tmp", "/da/state" ]
ENTRYPOINT ["/entrypoint.sh"]

