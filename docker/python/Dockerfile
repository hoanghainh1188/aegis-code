# Sandbox for the Python profile (#5, profiles/python.toml): the gate runs
# each step in a fresh container of this image with --network none, as the
# host user, with the job's tree at /work. The repo's own dependencies live
# in /work/.venv (filled by #66), which comes first on PATH.
FROM python:3.12-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates \
 && rm -rf /var/lib/apt/lists/*

RUN pip install --no-cache-dir \
      ruff==0.17.1 pytest==9.1.1 pytest-xdist==3.8.0 pytest-cov==7.1.0 \
      coverage==7.16.2 uv==0.13.4

# osv-scanner: the dependencies step's audit of a changed lockfile (#159,
# #191), through a proxy to the profile's audit_hosts only. A pinned
# release, checked against its published SHA-256.
ARG OSV_SCANNER_VERSION=2.7.0
ARG TARGETARCH
RUN set -eu; \
    arch="${TARGETARCH:-$(dpkg --print-architecture)}"; \
    case "$arch" in \
      amd64) sum=160c6253e2922e9e0a6a9771c5c4c40a4b3c6090b34a3cdd5e80c96cc2e4218b ;; \
      arm64) sum=4061ef4c021ed3c47f203b818f337e351336cc5cd67c0274fc36bd1dfc728dd7 ;; \
      *) echo "no osv-scanner for $arch" >&2; exit 1 ;; \
    esac; \
    python -c "import sys, urllib.request; urllib.request.urlretrieve(sys.argv[1], '/usr/local/bin/osv-scanner')" \
      "https://github.com/google/osv-scanner/releases/download/v${OSV_SCANNER_VERSION}/osv-scanner_linux_${arch}"; \
    echo "$sum  /usr/local/bin/osv-scanner" | sha256sum -c -; \
    chmod 0755 /usr/local/bin/osv-scanner; \
    osv-scanner --version

RUN mkdir -p /home/aegis/.cache/uv && chmod -R a+rwX /home/aegis
ENV HOME=/home/aegis \
    PATH=/work/.venv/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin \
    PYTHONDONTWRITEBYTECODE=1 \
    UV_OFFLINE=1 \
    PIP_NO_INDEX=1
WORKDIR /work
