# aegis-server (#33, ADR 0003 §1 and §6): the API, the workers, or both,
# chosen with --role. Built from the repository root:
#
#   docker build -f docker/server/Dockerfile -t aegis-server .
#
# Next to it: aegis-aci (the agent's tools, found next to the running
# program), git (job checkouts) and the docker client (the gate runs each
# step in a sandbox container, Design §6–§7). The sandbox images per
# profile stay in docker/<profile>/ and run on the host's daemon.
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

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git tini \
 && rm -rf /var/lib/apt/lists/*
COPY --from=docker-cli /usr/local/bin/docker /usr/local/bin/docker
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
