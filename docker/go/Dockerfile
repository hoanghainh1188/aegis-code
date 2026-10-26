# Sandbox for the Go profile (#210, profiles/go.toml): the gate runs each
# step in a fresh container of this image with --network none, as the host
# user, with the job's tree at /work. Modules come from /go/pkg/mod, filled
# by the fetch step (#66). The build cache stays in the container: the
# gate mounts its caches read-only and `go` must write there.
FROM golang:1.27-bookworm

# gopls: the ACI's find_definition / find_references.
RUN GOBIN=/usr/local/bin go install golang.org/x/tools/gopls@v0.23.0 \
 && rm -rf /go/pkg /root/.cache

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
    curl -fsSL -o /usr/local/bin/osv-scanner \
      "https://github.com/google/osv-scanner/releases/download/v${OSV_SCANNER_VERSION}/osv-scanner_linux_${arch}"; \
    echo "$sum  /usr/local/bin/osv-scanner" | sha256sum -c -; \
    chmod 0755 /usr/local/bin/osv-scanner; \
    osv-scanner --version

RUN mkdir -p /go/pkg/mod /home/aegis/.cache/go-build \
 && chmod -R a+rwX /go /home/aegis
# GOTOOLCHAIN=local: a go.mod asking for a newer Go fails instead of
# downloading one.
ENV HOME=/home/aegis \
    GOMODCACHE=/go/pkg/mod \
    GOCACHE=/home/aegis/.cache/go-build \
    GOTOOLCHAIN=local \
    GOTELEMETRY=off
WORKDIR /work
