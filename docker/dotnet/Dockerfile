# Sandbox for the C# / .NET profile (#210, profiles/csharp.toml): the gate
# runs each step in a fresh container of this image with --network none, as
# the host user, with the job's tree at /work. Packages come from
# /home/aegis/.nuget/packages, filled by the fetch step (#66).
FROM mcr.microsoft.com/dotnet/sdk:10.0

RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*

# csharp-ls: the ACI's find_definition / find_references.
# dotnet-sonarscanner: the profile's Sonar step (`dotnet sonarscanner
# begin` / `end` around the build); it gets its Java runtime from the
# SonarQube server.
RUN dotnet tool install --tool-path /usr/local/dotnet-tools csharp-ls --version 0.28.0 \
 && dotnet tool install --tool-path /usr/local/dotnet-tools dotnet-sonarscanner --version 11.3.0 \
 && chmod -R a+rX /usr/local/dotnet-tools \
 && rm -rf /root/.nuget /root/.dotnet

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

RUN mkdir -p /home/aegis/.nuget/packages && chmod -R a+rwX /home/aegis
ENV HOME=/home/aegis \
    PATH=$PATH:/usr/local/dotnet-tools \
    DOTNET_CLI_HOME=/home/aegis \
    NUGET_PACKAGES=/home/aegis/.nuget/packages \
    DOTNET_CLI_TELEMETRY_OPTOUT=1 \
    DOTNET_NOLOGO=1 \
    DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1 \
    DOTNET_CLI_WORKLOAD_UPDATE_NOTIFY_DISABLE=1
WORKDIR /work
