#!/usr/bin/env bash
# System and toolchain setup for the GitPulse dev container.
#
# Mirrors the Linux dependency list in ci.yml and CONTRIBUTING.md, plus the two
# tools `npm run ci:local` requires beyond the language toolchains. Fails on the
# first error: a container that came up with half a toolchain is worse than one
# that refused to build, because the missing half surfaces as a confusing gate
# failure later.
set -euo pipefail

ACTIONLINT_VERSION=1.7.12

sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  libwebkit2gtk-4.1-dev \
  libgtk-3-dev \
  build-essential \
  curl \
  wget \
  file \
  libxdo-dev \
  libssl-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  pkg-config

rustup component add clippy rustfmt llvm-tools-preview

# Pinned rather than tracking main: this script runs unattended at container
# build time, so the version it installs should be the version it was tested
# against.
curl -fsSL "https://raw.githubusercontent.com/rhysd/actionlint/v${ACTIONLINT_VERSION}/scripts/download-actionlint.bash" \
  | bash -s -- "${ACTIONLINT_VERSION}" /usr/local/bin

cargo install cargo-llvm-cov --locked

echo "GitPulse dev container ready. Run 'npm ci' then 'npm run ci:local'."
