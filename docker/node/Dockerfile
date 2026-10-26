# Sandbox for the TypeScript / JavaScript profile (#210,
# profiles/typescript.toml): the gate runs each step in a fresh container of
# this image with --network none, as the host user, with the job's tree at
# /work. The repo's own tools (prettier, eslint, tsc, its test runner) come
# from /work/node_modules, filled by the fetch step (#66); npm's cache is
# /home/aegis/.npm.
FROM node:24-bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*

# typescript-language-server (with a TypeScript for its tsserver): the
# ACI's find_definition / find_references. c8: the profile's coverage for
# repos that do not have it themselves; `npx --no-install` finds it on
# PATH.
RUN npm install -g --no-fund --no-audit \
      typescript-language-server@6.0.2 typescript@6.0.3 c8@12.0.0 \
 && npm cache clean --force

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

RUN mkdir -p /home/aegis/.npm && chmod -R a+rwX /home/aegis
ENV HOME=/home/aegis \
    npm_config_cache=/home/aegis/.npm \
    npm_config_update_notifier=false \
    npm_config_fund=false
WORKDIR /work
