# A Docker daemon with gVisor's runsc next to runc (#322): the bench runs the
# replay jobs against it, so the host's own daemon is left as it is.
#
#   docker build -t aegis-gvisor-bench scripts/gvisor
#
# The release puts runsc, its containerd shim and the sentry's binaries
# (`gvisor-bin/`) side by side; runsc finds them next to itself.
# GVISOR_RELEASE is a dated release of the gVisor bucket
# (storage.googleapis.com/gvisor/releases/release/<date>/).
FROM docker:dind
ARG GVISOR_RELEASE=20261109
RUN apk add --no-cache curl zstd python3 \
 && arch="$(uname -m)" \
 && base="https://storage.googleapis.com/gvisor/releases/release/${GVISOR_RELEASE}/${arch}" \
 && cd /tmp \
 && curl -fsSLO "${base}/gvisor.tar.zstd" \
 && curl -fsSLO "${base}/gvisor.tar.zstd.sha512" \
 && sha512sum -c gvisor.tar.zstd.sha512 \
 && zstd -dc gvisor.tar.zstd | tar -x -C /usr/local/bin \
 && rm -rf /tmp/gvisor* \
 && runsc --version
COPY daemon.json /etc/docker/daemon.json
