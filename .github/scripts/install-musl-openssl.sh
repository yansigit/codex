#!/usr/bin/env bash
set -euo pipefail

: "${TARGET:?TARGET environment variable is required}"
: "${GITHUB_ENV:?GITHUB_ENV environment variable is required}"
: "${OPENSSL_CC:?OPENSSL_CC must name the target musl compiler}"

case "${TARGET}" in
  x86_64-unknown-linux-musl) openssl_target="linux-x86_64" ;;
  aarch64-unknown-linux-musl) openssl_target="linux-aarch64" ;;
  *) echo "Unexpected musl target: ${TARGET}" >&2; exit 1 ;;
esac

# Build the verified upstream release with the target musl compiler.
# openssl-sys 0.9.116 supports the OpenSSL 4 API.
openssl_version="4.0.3"
openssl_sha256="325b5c806167c13b40b1ffeadfe0248197c00eccc4cf123ec1e28d2d2fd216d9"
openssl_root="${RUNNER_TEMP:-/tmp}/codex-musl-tools-${TARGET}/openssl-${openssl_version}"
openssl_prefix="${openssl_root}/prefix"

if [[ ! -f "${openssl_prefix}/.complete" ]]; then
  mkdir -p "${openssl_root}"
  archive="${openssl_root}/openssl-${openssl_version}.tar.gz"
  curl -fsSL "https://github.com/openssl/openssl/releases/download/openssl-${openssl_version}/openssl-${openssl_version}.tar.gz" -o "${archive}"
  echo "${openssl_sha256}  ${archive}" | sha256sum -c -
  tar -xzf "${archive}" -C "${openssl_root}"

  (
    cd "${openssl_root}/openssl-${openssl_version}"
    # Build static libraries without zlib or unsupported musl async APIs.
    # OpenSSL 4 has removed engines and SSLv3, so their old flags are unnecessary.
    CC="${OPENSSL_CC}" perl ./Configure "${openssl_target}" \
      "--prefix=${openssl_prefix}" --openssldir=/usr/local/ssl --libdir=lib \
      no-shared no-module no-tests no-comp no-zlib no-zlib-dynamic \
      no-md2 no-rc5 no-weak-ssl-ciphers no-camellia no-idea no-seed \
      no-async -DOPENSSL_NO_SECURE_MEMORY
    make -j"${OPENSSL_BUILD_JOBS:-$(nproc)}" build_libs
    make install_dev
  )
  touch "${openssl_prefix}/.complete"
fi

# Scope these overrides to the target so host build dependencies are unaffected.
# openssl-sys honors OPENSSL_NO_VENDOR even when Cargo enables `vendored`.
target_env="${TARGET^^}"
target_env="${target_env//-/_}"
{
  echo "${target_env}_OPENSSL_DIR=${openssl_prefix}"
  echo "${target_env}_OPENSSL_NO_VENDOR=1"
  echo "${target_env}_OPENSSL_STATIC=1"
} >> "${GITHUB_ENV}"
