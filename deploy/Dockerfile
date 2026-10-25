# syntax=docker/dockerfile:1
# Janus Scan Server base image (x86-64 only: Brother's driver is x86-64 only).
#
# This is the image the release workflow publishes. It has everything except
# Brother's brscan4 driver, which isn't ours to redistribute: users add it on
# their own machine with deploy/driver.Dockerfile (a seconds-long build).
# From the repo root:
#
#   docker build --platform linux/amd64 -f deploy/Dockerfile -t janus-scan-server .

# ---- Web front end (platform-independent output, so build natively) -------
FROM --platform=$BUILDPLATFORM node:22-bookworm-slim AS web
WORKDIR /src/app
COPY app/package.json app/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY app/ ./
RUN npm run build:web

# ---- Server binary, cross-compiled to x86-64 ------------------------------
# libsane is loaded at runtime, not linked, so no x86-64 C libraries are
# needed here; on Apple Silicon this stays a fast native build.
FROM --platform=$BUILDPLATFORM rust:1-bookworm AS server
RUN if [ "$(dpkg --print-architecture)" != amd64 ]; then \
      apt-get update && apt-get install -y --no-install-recommends gcc-x86-64-linux-gnu libc6-dev-amd64-cross \
      && rm -rf /var/lib/apt/lists/*; \
    fi \
    && rustup target add x86_64-unknown-linux-gnu
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc
WORKDIR /src
# The whole workspace has to be present for Cargo to load it, the desktop
# app included, though only the server is built.
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY app/src-tauri app/src-tauri
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --locked --release -p janus-scan-server --target x86_64-unknown-linux-gnu \
    && cp target/x86_64-unknown-linux-gnu/release/janus-scan-server /janus-scan-server

# ---- Files for installing without Docker (LXC) ------------------------------
#   docker build --platform linux/amd64 -f deploy/Dockerfile --target dist \
#     --output type=local,dest=deploy/dist .
FROM scratch AS dist
COPY --from=server /janus-scan-server /bin/janus-scan-server
COPY --from=web /src/app/build-web /web
COPY deploy/register-scanner.sh deploy/lxc/install.sh deploy/lxc/janus-scan.service deploy/server.yaml /

# ---- Runtime (no Brother driver) ------------------------------------------
# No libsane: the server loads Brother's backend directly (it exports the
# plain SANE API). Debian's libsane1 would add about 300 MB of dependencies
# (udev, systemd, gphoto2, poppler, net-snmp, perl) for scanners we don't use.
FROM debian:stable-slim
# netbase (/etc/services, /etc/protocols): without it Brother's driver
# crashes when it opens a network scanner. libjpeg62-turbo: the driver loads
# it to decode what the scanner sends over Wi-Fi. tzdata: {date} and {time}
# in file names follow TZ.
RUN apt-get update \
    && apt-get install -y --no-install-recommends netbase libjpeg62-turbo tzdata \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /etc/janus-scan
# Only these files: nothing else from deploy/ (such as a driver .deb) may end
# up in the published image.
COPY --chmod=0755 deploy/entrypoint.sh /usr/local/bin/janus-scan-entrypoint
COPY --chmod=0755 deploy/register-scanner.sh /usr/local/bin/janus-scan-register
COPY --chmod=0755 deploy/install-driver.sh /usr/local/bin/janus-scan-install-driver
COPY --from=server /janus-scan-server /usr/local/bin/janus-scan-server
COPY --from=web /src/app/build-web /usr/share/janus-scan/web

# Brother's backend, in place of libsane (installed by driver.Dockerfile).
ENV PORT=8080 \
    JANUS_SCAN_LIBSANE=/usr/lib64/sane/libsane-brother4.so.1 \
    WEB_DIR=/usr/share/janus-scan/web \
    JANUS_CONFIG=/etc/janus-scan/server.yaml
EXPOSE 8080
USER 1000:1000
HEALTHCHECK --interval=60s --timeout=5s --start-period=10s CMD ["janus-scan-server", "health"]
ENTRYPOINT ["janus-scan-entrypoint"]
