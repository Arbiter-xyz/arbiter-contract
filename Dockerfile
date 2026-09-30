# #115: canonical, pinned build environment for the OracleEscrow WASM.
#
# Anyone (including an SCF reviewer) can reproduce the deployed bytecode with:
#
#   docker build -t arbiter-contract-build .
#   docker run --rm -v "$PWD:/src" arbiter-contract-build
#
# which prints the sha256 of the optimized WASM. Every input that can change
# the output is pinned here: the base image by digest-able tag, rustc via
# rust-toolchain.toml, crate versions via the committed Cargo.lock
# (`--locked`), and stellar-cli by exact release. Keep RUST_VERSION in sync
# with rust-toolchain.toml.

FROM rust:1.89.0-slim-bookworm

ARG STELLAR_CLI_VERSION=27.1.0

RUN apt-get update \
 && apt-get install -y --no-install-recommends curl ca-certificates \
 && curl -fsSL -o /tmp/stellar-cli.deb \
      "https://github.com/stellar/stellar-cli/releases/download/v${STELLAR_CLI_VERSION}/stellar-cli_${STELLAR_CLI_VERSION}_amd64.deb" \
 && (dpkg -i /tmp/stellar-cli.deb || apt-get install -f -y) \
 && rm -rf /tmp/stellar-cli.deb /var/lib/apt/lists/*

RUN rustup target add wasm32v1-none

# Build from a FIXED path. rustc embeds absolute source paths (panic
# locations, debug info) into the binary; building under the same /src path
# everywhere, plus --remap-path-prefix in the build script, removes the
# build-path dependence that most often breaks Rust WASM reproducibility.
WORKDIR /src

ENV SOURCE_DATE_EPOCH=0 \
    CARGO_INCREMENTAL=0 \
    CARGO_HOME=/usr/local/cargo

ENTRYPOINT ["/src/scripts/reproducible-build.sh"]
