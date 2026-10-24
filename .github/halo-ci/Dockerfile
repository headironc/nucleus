FROM mcr.microsoft.com/playwright:v1.64.1-noble@sha256:7852dc5a7d72f5fe458a597d690f6c42fbc8e9e26ddfa95f8e6ef552558d9bbf AS environment

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_HOME=/opt/cargo \
    RUSTUP_HOME=/opt/rustup \
    PATH=/opt/cargo/bin:$PATH
RUN sed -i 's|http://azure.archive.ubuntu.com|https://archive.ubuntu.com|g; s|http://archive.ubuntu.com|https://archive.ubuntu.com|g; s|http://security.ubuntu.com|https://security.ubuntu.com|g' /etc/apt/sources.list.d/ubuntu.sources \
    && apt-get -o Acquire::Retries=3 -o Acquire::https::Timeout=30 update \
    && apt-get -o Acquire::Retries=3 -o Acquire::https::Timeout=30 install --yes --no-install-recommends \
      build-essential bison flex ca-certificates curl git pkg-config openssl unzip \
      libssl-dev libicu-dev libreadline-dev zlib1g-dev libxml2-dev nginx \
    && rm -rf /var/lib/apt/lists/*
ARG BUN_VERSION
RUN curl --fail --location --silent --show-error --connect-timeout 10 --max-time 120 \
      "https://github.com/oven-sh/bun/releases/download/bun-v${BUN_VERSION}/bun-linux-x64.zip" \
      --output /tmp/bun.zip \
    && echo '36368faef7527875d5ffa52e53cd48021741f2a83eb6208a8dd64068d422a913  /tmp/bun.zip' | sha256sum --check \
    && unzip -q /tmp/bun.zip -d /tmp \
    && mv /tmp/bun-linux-x64/bun /usr/local/bin/bun \
    && rm -rf /tmp/bun.zip /tmp/bun-linux-x64
ARG RUST_VERSION
ARG RUST_COMPONENTS
RUN curl --fail --location --silent --show-error --connect-timeout 10 --max-time 120 \
      https://static.rust-lang.org/rustup/archive/1.29.2/x86_64-unknown-linux-gnu/rustup-init \
      --output /tmp/rustup-init \
    && echo '5be191e8c70a5007074254cb74d78dbfdd25cd5a61c27de7b8e5410122ec24f4  /tmp/rustup-init' | sha256sum --check \
    && chmod +x /tmp/rustup-init \
    && /tmp/rustup-init -y --no-modify-path --profile minimal \
      --default-toolchain "$RUST_VERSION" --component "$RUST_COMPONENTS" \
    && rm /tmp/rustup-init \
    && chown -R pwuser:pwuser /opt/cargo /opt/rustup

FROM environment AS postgresql
WORKDIR /opt/nucleus
COPY fixture/postgresql.ts fixture/postgresql.version ./fixture/
RUN bun fixture/postgresql.ts

FROM environment
COPY --from=postgresql /opt/nucleus/target/postgresql /opt/nucleus/target/postgresql
ARG INPUT_DIGEST
# nginx -t checks compiled temporary paths even when the fixture only uses proxying.
RUN install -d -o pwuser -g pwuser /var/lib/nginx/fastcgi /var/lib/nginx/uwsgi /var/lib/nginx/scgi \
    && test -n "$INPUT_DIGEST" && printf '%s\n' "$INPUT_DIGEST" > /etc/nucleus-halo-ci-inputs
LABEL org.opencontainers.image.source="https://github.com/silence-force/nucleus"
USER pwuser
WORKDIR /home/pwuser
