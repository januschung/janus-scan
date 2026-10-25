# Ubuntu-family image (Ubuntu 24.04, Linux Mint 22; Linux Lite 7 is covered by 24.04).
# Build with: docker build --platform linux/amd64 --build-arg BASE=ubuntu:24.04 -f docker/apt.Dockerfile .
ARG BASE=ubuntu:24.04
FROM ${BASE}
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo
ENV PATH=/usr/local/cargo/bin:$PATH
COPY scripts/linux-deps.sh docker/setup.sh /opt/janus-scan/
RUN sh /opt/janus-scan/setup.sh
WORKDIR /work
# test.sh comes from the mounted repo (matrix.sh mounts it at /src), so
# editing it never invalidates the image.
CMD ["sh", "/src/docker/test.sh"]
