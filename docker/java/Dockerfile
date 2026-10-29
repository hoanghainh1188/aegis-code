# Sandbox for the Java profile (#210, profiles/java.toml): the gate runs
# each step in a fresh container of this image with --network none, as the
# host user, with the job's tree at /work. Artifacts come from
# /home/aegis/.m2/repository (Maven) and /home/aegis/.gradle/caches
# (Gradle), filled by the fetch step (#66).
FROM maven:3.9.15-eclipse-temurin-21

# python3: jdtls's launcher is a Python script.
RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates curl python3 \
 && rm -rf /var/lib/apt/lists/*

# jdtls: the ACI's find_definition / find_references. A pinned milestone,
# checked against its published SHA-256.
ARG JDTLS=1.61.0/jdt-language-server-1.61.0-202609031315.tar.gz
ARG JDTLS_SHA256=338e7e73d61836651ba2453919a0d34fa763eb4e7c03342092309bffb8934c64
RUN set -eu; \
    curl -fsSL -o /tmp/jdtls.tar.gz "https://download.eclipse.org/jdtls/milestones/${JDTLS}"; \
    echo "$JDTLS_SHA256  /tmp/jdtls.tar.gz" | sha256sum -c -; \
    mkdir -p /opt/jdtls; \
    tar -xzf /tmp/jdtls.tar.gz -C /opt/jdtls; \
    rm /tmp/jdtls.tar.gz; \
    chmod -R a+rX /opt/jdtls; \
    ln -s /opt/jdtls/bin/jdtls /usr/local/bin/jdtls

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

# Ahead of /usr/bin/mvn on PATH: gives Maven the fetch step's proxy.
COPY --chmod=0755 mvn /usr/local/bin/mvn

RUN mkdir -p /home/aegis/.m2/repository /home/aegis/.gradle/caches \
 && chmod -R a+rwX /home/aegis
# The host user has no entry in /etc/passwd, so Java's user.home would be
# "?": Maven's local repository is set through it. The image's entrypoint
# copies settings into /root/.m2, which the host user cannot write.
ENV HOME=/home/aegis \
    MAVEN_CONFIG=/home/aegis/.m2 \
    MAVEN_OPTS=-Duser.home=/home/aegis \
    GRADLE_USER_HOME=/home/aegis/.gradle
ENTRYPOINT []
WORKDIR /work
