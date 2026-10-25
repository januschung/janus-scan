# Arch-family image (CachyOS).
# Build with: docker build --platform linux/amd64 --build-arg BASE=cachyos/cachyos:latest -f docker/pacman.Dockerfile .
#
# cachyos/cachyos is the baseline x86-64 image from github.com/CachyOS/docker
# (its plain `Dockerfile`). cachyos/cachyos-v3 needs x86-64-v3 CPU features
# (AVX2 etc.), which amd64 emulation on Apple Silicon may not provide.
ARG BASE=cachyos/cachyos:latest
FROM ${BASE}
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo
ENV PATH=/usr/local/cargo/bin:$PATH
COPY scripts/linux-deps.sh docker/setup.sh /opt/janus-scan/
RUN sh /opt/janus-scan/setup.sh
WORKDIR /work
# test.sh comes from the mounted repo (matrix.sh mounts it at /src), so
# editing it never invalidates the image.
CMD ["sh", "/src/docker/test.sh"]
