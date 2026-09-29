# syntax=docker/dockerfile:1.7
#
# OXIM container image. Build from the repository root:
#
#   docker build -f deploy/docker/Dockerfile -t oxim .
#   docker build -f deploy/docker/Dockerfile --target sim -t oxim-sim .
#
# The default target is the engine. The `sim` target holds oxim-sim, the
# simulator used by the demo in deploy/docker/compose.yaml.

ARG RUST_VERSION=1.89
ARG DEBIAN_RELEASE=bookworm

FROM rust:${RUST_VERSION}-${DEBIAN_RELEASE} AS builder
WORKDIR /src
COPY . .
# Build outputs live in cache mounts, so the binaries are copied to /out in
# the same step.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p oxim -p oxim-sim \
    && install -D -m 0755 target/release/oxim /out/usr/bin/oxim \
    && install -D -m 0755 target/release/oxim-sim /out/usr/bin/oxim-sim
# The default configuration comes from `oxim init`; the data directory
# moves to the volume and the web server listens on all interfaces of the
# container (publish it only where it is needed; see docs/install/docker.md).
RUN /out/usr/bin/oxim init --dir /out/etc/oxim \
    && sed -i 's|^data_dir: data$|data_dir: /var/lib/oxim|' /out/etc/oxim/oxim.yaml \
    && grep -q '^data_dir: /var/lib/oxim$' /out/etc/oxim/oxim.yaml \
    && sed -i 's|^  listen: 127.0.0.1:8080 .*$|  listen: 0.0.0.0:8080|' /out/etc/oxim/oxim.yaml \
    && grep -q '^  listen: 0.0.0.0:8080$' /out/etc/oxim/oxim.yaml \
    && rmdir /out/etc/oxim/data

FROM debian:${DEBIAN_RELEASE}-slim AS base
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 oxim \
    && useradd --system --uid 10001 --gid oxim --groups dialout \
        --home-dir /var/lib/oxim --no-create-home --shell /usr/sbin/nologin oxim

FROM base AS sim
COPY --from=builder /out/usr/bin/oxim-sim /usr/bin/oxim-sim
USER 10001:10001
ENTRYPOINT ["oxim-sim"]
CMD ["--help"]

FROM base AS runtime
ARG VERSION=0.0.0
ARG REVISION=unknown
LABEL org.opencontainers.image.title="OXIM" \
      org.opencontainers.image.description="Open-source clinical integration engine" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.source="https://github.com/oxim-health/oxim" \
      org.opencontainers.image.url="https://github.com/oxim-health/oxim" \
      org.opencontainers.image.documentation="https://github.com/oxim-health/oxim/blob/main/docs/install/docker.md" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0" \
      org.opencontainers.image.vendor="The OXIM Contributors"
COPY --from=builder /out/usr/bin/oxim /usr/bin/oxim
COPY --from=builder --chown=root:oxim /out/etc/oxim /etc/oxim
RUN chmod -R g-w,o-rwx /etc/oxim \
    && install -d -m 0750 -o oxim -g oxim /var/lib/oxim
# Relative paths in channel files (such as a file destination's directory)
# resolve against the working directory, which is on the volume.
WORKDIR /var/lib/oxim
VOLUME ["/var/lib/oxim"]
# The example channel of `oxim init` listens for HL7 v2 over MLLP on 2575;
# the web UI and REST API listen on 8080.
EXPOSE 2575 8080
USER 10001:10001
ENTRYPOINT ["oxim"]
CMD ["run", "-c", "/etc/oxim/oxim.yaml"]
