build: build-macos build-linux build-windows

build-macos:
  cargo build --target aarch64-apple-darwin

build-linux:
  # NOTE: requires `brew install pkc-config` on macos
  PKG_CONFIG_ALLOW_CROSS=1 cargo build --target x86_64-unknown-linux-gnu

build-windows:
  # NOTE: requires `brew install mingw-w64` on macos
  cargo build --target x86_64-pc-windows-gnu

clippy:
  cargo clippy --all-targets --all-features -- -D warnings

fmt:
  cargo fmt --all
