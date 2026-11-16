# Sandbox for the PHP profile (#37, profiles/php.toml): the gate runs each
# step in a fresh container of this image with --network none, as the host
# user, with the job's tree at /work.
FROM php:8.3-cli

RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates curl unzip libzip-dev \
 && docker-php-ext-install zip \
 && pecl install pcov \
 && docker-php-ext-enable pcov \
 && rm -rf /var/lib/apt/lists/*

COPY --from=composer:2.8 /usr/bin/composer /usr/local/bin/composer

# Global CLI tools for formatting, static analysis, and LSP when the target
# repo does not bundle them in vendor/bin.
RUN COMPOSER_HOME=/usr/local/composer COMPOSER_BIN_DIR=/usr/local/bin \
    composer global require --no-interaction --no-progress \
      friendsofphp/php-cs-fixer:^3.64 \
      phpstan/phpstan:^2.0

# phpactor: the ACI's language server (profiles/php.toml `lsp`, #549). The
# project ships it as a PHAR; composer would pull its dependencies into the
# global tools above. A pinned release, checked against its SHA-256.
ARG PHPACTOR_VERSION=2026.10.07.0
ARG PHPACTOR_SHA256=b28bf6166e016d539fc2f2aec18ef918b12590e436a6f1ccbdf30b5bbdbdb16b
RUN set -eu; \
    curl --retry 6 --retry-delay 5 --retry-all-errors -fsSL \
      -o /usr/local/bin/phpactor \
      "https://github.com/phpactor/phpactor/releases/download/${PHPACTOR_VERSION}/phpactor.phar"; \
    echo "${PHPACTOR_SHA256}  /usr/local/bin/phpactor" | sha256sum -c -; \
    chmod 0755 /usr/local/bin/phpactor; \
    phpactor --version

# osv-scanner: the dependencies step's audit of a changed lockfile (#159,
# #191), through a proxy to the profile's audit_hosts only.
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

RUN mkdir -p /home/aegis/.cache/composer && chmod -R a+rwX /home/aegis
ENV HOME=/home/aegis \
    COMPOSER_CACHE_DIR=/home/aegis/.cache/composer \
    PATH=/work/vendor/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin
WORKDIR /work
