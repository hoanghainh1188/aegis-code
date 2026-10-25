# aegis-server (#33, ADR 0003 §1 and §6): the API, the workers, or both,
# chosen with --role. Built from the repository root:
#
#   docker build -f docker/server/Dockerfile -t aegis-server .
#
# Next to it: aegis-aci (the agent's tools, found next to the running
# program), codebase-memory-mcp (the call graph), git (job checkouts) and
# the docker client (the gate runs each step in a sandbox container,
# Design §6–§7). The sandbox images per profile stay in docker/<profile>/
# and run on the host's daemon.
#
# Built with the bedrock feature, so one image serves `poc` and
# `customer_jp`. No secret is in the image (#30): `[secrets]` in the
# mounted configuration says where they are read.

FROM rust:1.99-bookworm AS build
WORKDIR /src
# Queries are checked against .sqlx/, not a live database.
ENV SQLX_OFFLINE=true CARGO_TERM_COLOR=never
COPY Cargo.toml Cargo.lock ./
COPY .sqlx .sqlx
COPY crates crates
COPY profiles profiles
COPY policies policies
COPY prompts prompts
COPY config config
COPY docker/egress docker/egress
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p aegis-server -p aegis-aci \
        --features aegis-server/bedrock \
 && install -D -m 0755 target/release/aegis-server target/release/aegis-aci -t /out

# Only the client: the daemon is the host's (docs/runbook/README.md, "Worker
# và Docker").
FROM docker:29-cli AS docker-cli

# Codebase-Memory (design §3.2, #185), the call graph the agent queries
# next to the ACI; config/poc.toml finds it on PATH. The portable build:
# the plain one needs glibc 2.38, bookworm has 2.36. Checksums from the
# release's checksums.txt.
FROM debian:bookworm-slim AS codebase-memory
ARG TARGETARCH
ARG CBM_VERSION=0.12.1
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && case "${TARGETARCH}" in \
      amd64) sum=8c3d0341064301e78ecc84cf7348d4e2c6562a0f57448243ea560aaa34de36c1 ;; \
      arm64) sum=ed0f1f548cd79b121c401c2e9f7841c189a001af26d3768368088c24f6a53533 ;; \
      *) echo "no Codebase-Memory build for ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
 && curl -fsSL -o /tmp/cbm.tar.gz \
      "https://github.com/DeusData/codebase-memory-mcp/releases/download/v${CBM_VERSION}/codebase-memory-mcp-linux-${TARGETARCH}-portable.tar.gz" \
 && echo "${sum}  /tmp/cbm.tar.gz" | sha256sum -c - \
 && tar -xzf /tmp/cbm.tar.gz -C /usr/local/bin codebase-memory-mcp \
 && /usr/local/bin/codebase-memory-mcp --version

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git tini \
 && rm -rf /var/lib/apt/lists/*
COPY --from=docker-cli /usr/local/bin/docker /usr/local/bin/docker
COPY --from=codebase-memory /usr/local/bin/codebase-memory-mcp /usr/local/bin/codebase-memory-mcp
COPY --from=build /out/ /usr/local/bin/

# Not root. The gate starts sandboxes as this same uid:gid, so the host's
# workspace directory must belong to it (deploy/tofu/modules/aegis/user_data.sh).
ARG AEGIS_UID=10001
RUN groupadd --gid ${AEGIS_UID} aegis \
 && useradd --uid ${AEGIS_UID} --gid aegis --home-dir /srv/aegis --create-home aegis
WORKDIR /srv/aegis
# Mode configurations and prices; relative paths in them are taken from
# here. The server's own configuration is mounted at /etc/aegis.
COPY --chown=root:root config/poc.toml config/customer-jp.toml config/pricing.toml config/
USER aegis

EXPOSE 8080
ENTRYPOINT ["/usr/bin/tini", "--", "aegis-server"]
CMD ["--config", "/etc/aegis/server.toml", "--role", "all"]
