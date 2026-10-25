# Adds Brother's scanner driver to the Janus Scan Server image. The published
# image doesn't include it: the driver is Brother's, under Brother's license.
#
# Download brscan4 for the ADS-1000W (64-bit .deb) from support.brother.com,
# put it next to this file, then build (docker-compose.yml does this for you):
#
#   docker build --platform linux/amd64 -f driver.Dockerfile -t janus-scan-server-local .
#
# The result contains Brother's driver: keep it on your own machines or in a
# private registry.
ARG BASE=ghcr.io/januschung/janus-scan-server:latest
FROM ${BASE}
USER root
COPY brscan4-*.amd64.deb /tmp/
RUN janus-scan-install-driver /tmp/brscan4-*.amd64.deb && rm -f /tmp/brscan4-*.amd64.deb
USER 1000:1000
