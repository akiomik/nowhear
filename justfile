build: build-macos build-linux build-windows

build-linux:
  # NOTE: requires `brew install pkc-config` on macos
  PKG_CONFIG_ALLOW_CROSS=1 cargo build --target x86_64-unknown-linux-gnu

build-macos:
  cargo build --target aarch64-apple-darwin

build-windows:
  # NOTE: requires `brew install mingw-w64` on macos
  cargo build --target x86_64-pc-windows-gnu

build-windows-aarch64:
  cargo build --target aarch64-pc-windows-msvc

test:
  cargo test --all-targets --all-features

clippy: clippy-linux clippy-macos clippy-windows

clippy-linux:
  PKG_CONFIG_ALLOW_CROSS=1 cargo clippy --target x86_64-unknown-linux-gnu --all-targets --all-features -- -D warnings

clippy-macos:
  cargo clippy --target aarch64-apple-darwin --all-targets --all-features -- -D warnings

clippy-windows:
  cargo clippy --target x86_64-pc-windows-gnu --all-targets --all-features -- -D warnings

fmt:
  cargo fmt --all

doc-build:
  cargo doc --no-deps
