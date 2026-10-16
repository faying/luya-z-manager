# Build image for the U60 Pro (MU5250) Rust programs: Rust + the aarch64 musl
# target + zig + cargo-zigbuild, every piece pinned. The same file lives in
# data-service scripts/build.Dockerfile and manager onboard/build.Dockerfile
# (each repo must build on its own); keep them identical, and keep the Rust
# version equal to rust-version in Cargo.toml and the CI toolchain.
# SPDX-License-Identifier: MIT
FROM rust:1.99.0@sha256:6ff07edce8775d0f64be7aba9197229407301bddf2054d62c27b541a6238a181
ARG ZIG=0.17.0
ARG ZIG_SHA256_X86_64=1cbe9df9f27e6b78d14ccbca43b6703a404ef79ef1c463de901d7f088d4e2026
ARG ZIG_SHA256_AARCH64=9e8d11661d4ae3bd57702a3832781e23ad151dde5798e16a5ccd503f65234ff8
ARG CARGO_ZIGBUILD=0.23.4
RUN set -eu; \
    arch=$(uname -m); \
    case "$arch" in \
      x86_64) sum=$ZIG_SHA256_X86_64 ;; \
      aarch64) sum=$ZIG_SHA256_AARCH64 ;; \
      *) echo "no zig for $arch" >&2; exit 1 ;; \
    esac; \
    curl -fsSL -o /tmp/zig.tar.xz "https://ziglang.org/download/$ZIG/zig-$arch-linux-$ZIG.tar.xz"; \
    echo "$sum  /tmp/zig.tar.xz" | sha256sum -c -; \
    mkdir -p /opt/zig; tar -xJf /tmp/zig.tar.xz -C /opt/zig --strip-components=1; rm /tmp/zig.tar.xz; \
    ln -s /opt/zig/zig /usr/local/bin/zig; \
    rustup target add aarch64-unknown-linux-musl; \
    cargo install --locked cargo-zigbuild --version "$CARGO_ZIGBUILD"; \
    rm -rf /usr/local/cargo/registry
