# All-in-one runner image for Aegis CI.
# Bundles Rust stable, tools (rustfmt, clippy, cargo-llvm-cov, cargo-deny),
# and the runtime profiles for Python, Go, Node.js, Java, and .NET.
FROM rust:1.99-bookworm

ENV DEBIAN_FRONTEND=noninteractive

# Install system dependencies, compilers, Java 17, and Maven
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    git \
    gnupg \
    build-essential \
    pkg-config \
    libssl-dev \
    python3 \
    python3-pip \
    python3-venv \
    openjdk-17-jdk-headless \
    maven \
 && rm -rf /var/lib/apt/lists/*

# Rust components & CLI tools
RUN rustup component add rustfmt clippy llvm-tools-preview \
 && cargo install --locked cargo-llvm-cov@0.9.1 cargo-deny@0.20.2 \
 && rm -rf /usr/local/cargo/registry /usr/local/cargo/git

# Python test tools
RUN pip install --no-cache-dir --break-system-packages \
    ruff==0.17.1 \
    pytest==9.1.1 \
    pytest-cov==7.1.0

# Go stable
RUN curl -fsSL https://go.dev/dl/go1.23.6.linux-amd64.tar.gz | tar -C /usr/local -xz
ENV PATH="/usr/local/go/bin:${PATH}"

# Node.js 24
RUN curl -fsSL https://deb.nodesource.com/setup_24.x | bash - \
 && apt-get install -y --no-install-recommends nodejs \
 && rm -rf /var/lib/apt/lists/*

# .NET 10.0 SDK
RUN curl -fsSL https://dot.net/v1/dotnet-install.sh -o /tmp/dotnet-install.sh \
 && chmod +x /tmp/dotnet-install.sh \
 && /tmp/dotnet-install.sh --channel 10.0 --install-dir /usr/share/dotnet \
 && ln -s /usr/share/dotnet/dotnet /usr/bin/dotnet \
 && rm /tmp/dotnet-install.sh

ENV AEGIS_REQUIRE_PROFILE_TOOLS=1 \
    CARGO_TERM_COLOR=always
