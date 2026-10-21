# Sandbox for the Rust profile (#5, profiles/rust.toml): the gate runs each
# step in a fresh container of this image with --network none, as the host
# user, with the job's tree at /work. Dependencies come from the cargo
# registry and git caches mounted at /usr/local/cargo/{registry,git}
# (filled by #66).
FROM rust:1.99-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates pkg-config libssl-dev g++ make \
 && rm -rf /var/lib/apt/lists/*

RUN rustup component add rustfmt clippy llvm-tools-preview \
 && cargo install --locked cargo-llvm-cov@0.9.1 cargo-deny@0.20.2 \
 && rm -rf /usr/local/cargo/registry /usr/local/cargo/git

# Steps run as the host user, not root: cargo needs to write its lock and
# cache under CARGO_HOME, and HOME must be writable by anyone.
RUN mkdir -p /usr/local/cargo/registry /usr/local/cargo/git /home/aegis \
 && chmod -R a+rwX /usr/local/cargo /home/aegis
ENV HOME=/home/aegis \
    CARGO_TERM_COLOR=never \
    CARGO_NET_OFFLINE=true
WORKDIR /work
