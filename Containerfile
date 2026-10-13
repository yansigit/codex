# Ubuntu 24.04 verification image for Nyacode.
# Docker and Podman both build this file; it uses no BuildKit-only syntax.
#
#   docker build -f Containerfile -t nyacode-verify:rust-1.95 .
#   podman build -f Containerfile -t nyacode-verify:rust-1.95 .
#
# scripts/verify-container.sh is the supported entrypoint. The repository is
# bind-mounted at runtime. This image does not copy it, and it does not expect
# the host home directory to be mounted.

FROM ubuntu:24.04

ARG RUST_VERSION=1.95.0
ARG NEXTEST_VERSION=0.9.103
ARG SCCACHE_VERSION=0.8.2

ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    HOME=/home/verifier \
    RUST_MIN_STACK=67108864 \
    CARGO_INCREMENTAL=0 \
    CARGO_NET_GIT_FETCH_WITH_CLI=true \
    CARGO_TERM_COLOR=always

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bubblewrap \
        build-essential \
        ca-certificates \
        clang \
        cmake \
        curl \
        git \
        libasound2-dev \
        libcap-dev \
        libclang-dev \
        libssl-dev \
        perl \
        pkg-config \
        protobuf-compiler \
        python3 \
        ripgrep \
        xz-utils \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /home/verifier /var/cache/sccache /usr/local/cargo /usr/local/rustup \
    && chmod 0777 /home/verifier /var/cache/sccache

# Install the toolchain under /usr/local and symlink it into /usr/local/bin so
# cargo is on PATH for login shells, non-login shells, and stripped PATH values.
RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
        | sh -s -- -y --no-modify-path --profile minimal \
            --default-toolchain "${RUST_VERSION}" \
            --component rustfmt,clippy,rust-src \
    && ln -sfn /usr/local/cargo/bin/cargo /usr/local/bin/cargo \
    && ln -sfn /usr/local/cargo/bin/cargo-clippy /usr/local/bin/cargo-clippy \
    && ln -sfn /usr/local/cargo/bin/cargo-fmt /usr/local/bin/cargo-fmt \
    && ln -sfn /usr/local/cargo/bin/clippy-driver /usr/local/bin/clippy-driver \
    && ln -sfn /usr/local/cargo/bin/rustc /usr/local/bin/rustc \
    && ln -sfn /usr/local/cargo/bin/rustdoc /usr/local/bin/rustdoc \
    && ln -sfn /usr/local/cargo/bin/rustfmt /usr/local/bin/rustfmt \
    && ln -sfn /usr/local/cargo/bin/rustup /usr/local/bin/rustup \
    && rustc --version \
    && rustc --version | grep -F "rustc ${RUST_VERSION}" \
    && command -v cargo \
    && command -v rustc \
    && cargo --version

RUN curl -fsSL \
        "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-${NEXTEST_VERSION}/cargo-nextest-${NEXTEST_VERSION}-x86_64-unknown-linux-gnu.tar.gz" \
        | tar -xz -C /usr/local/cargo/bin \
    && ln -sfn /usr/local/cargo/bin/cargo-nextest /usr/local/bin/cargo-nextest \
    && cargo nextest --version \
    && curl -fsSL \
        "https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
        | tar -xz --strip-components=1 -C /usr/local/bin \
            "sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl/sccache" \
    && sccache --version

WORKDIR /src
# The verifier script is the command. A bare `docker run` still has cargo on PATH.
CMD ["/bin/bash"]
