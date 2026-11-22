# Sandbox for the Kotlin profile (#586, profiles/kotlin.toml): the gate runs
# each step in a fresh container of this image with --network none, as the
# host user, with the job's tree at /work. Artifacts come from
# /home/aegis/.gradle/caches and a repo's pinned Gradle from
# /home/aegis/.gradle/wrapper, both filled by the fetch step (#66).
FROM gradle:9.8.2-jdk21

RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates curl unzip \
 && rm -rf /var/lib/apt/lists/*

# kotlin-language-server: the ACI's find_definition / find_references. A
# pinned release; the project publishes no checksum, so this is the
# SHA-256 of the asset as first downloaded (2026-11-22). JetBrains'
# kotlin-lsp is not used: its builds expire a few weeks after release.
ARG KLS_VERSION=1.3.13
ARG KLS_SHA256=4fe7d71d087b307c7869036171bd9d8c6a4284cd7c25b89098b0a24eb2d9b6d2
RUN set -eu; \
    curl --retry 6 --retry-delay 5 --retry-all-errors -fsSL -o /tmp/kls.zip \
      "https://github.com/fwcd/kotlin-language-server/releases/download/${KLS_VERSION}/server.zip"; \
    echo "$KLS_SHA256  /tmp/kls.zip" | sha256sum -c -; \
    unzip -q /tmp/kls.zip -d /opt; \
    mv /opt/server /opt/kotlin-language-server; \
    rm /tmp/kls.zip; \
    chmod -R a+rX /opt/kotlin-language-server; \
    ln -s /opt/kotlin-language-server/bin/kotlin-language-server /usr/local/bin/kotlin-language-server

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
    curl --retry 6 --retry-delay 5 --retry-all-errors -fsSL \
      -o /usr/local/bin/osv-scanner \
      "https://github.com/google/osv-scanner/releases/download/v${OSV_SCANNER_VERSION}/osv-scanner_linux_${arch}"; \
    echo "$sum  /usr/local/bin/osv-scanner" | sha256sum -c -; \
    chmod 0755 /usr/local/bin/osv-scanner; \
    osv-scanner --version

# Ahead of /usr/bin/gradle on PATH: the repo's wrapper, and the fetch
# step's proxy.
COPY --chmod=0755 gradle /usr/local/bin/gradle
# The profile's upgrade of a CVE job (#501): a dependency in the repo's
# version catalog.
COPY --chmod=0755 aegis-gradle-upgrade /usr/local/bin/aegis-gradle-upgrade
# GRADLE_USER_HOME/init.d applies to the image's Gradle and to any the
# wrapper downloads. One-shot containers want no daemon.
COPY --chmod=0644 aegis.init.gradle /home/aegis/.gradle/init.d/aegis.init.gradle
RUN printf 'org.gradle.daemon=false\norg.gradle.console=plain\norg.gradle.welcome=never\n' \
      > /home/aegis/.gradle/gradle.properties \
 && mkdir -p /home/aegis/.gradle/caches /home/aegis/.gradle/wrapper \
 && chmod -R a+rwX /home/aegis
# The host user has no entry in /etc/passwd, so Java's user.home would be
# "?".
ENV HOME=/home/aegis \
    GRADLE_USER_HOME=/home/aegis/.gradle \
    JAVA_TOOL_OPTIONS=-Duser.home=/home/aegis
ENTRYPOINT []
WORKDIR /work
