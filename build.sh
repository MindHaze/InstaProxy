#!/usr/bin/env sh
set -eu

# Always build the project beside this script, regardless of the caller's directory.
CDPATH= cd -- "$(dirname -- "$0")"

if ! command -v cargo >/dev/null 2>&1; then
    PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
    export PATH
fi
if ! command -v cargo >/dev/null 2>&1; then
    echo "Rust Cargo was not found. Install Rust from https://rustup.rs/" >&2
    exit 1
fi

echo "Formatting source files..."
cargo fmt --all

echo "Running tests..."
cargo test --all-targets --locked

echo "Building InstaProxy in release mode..."
cargo build --release --locked

echo
echo "Build completed successfully."
if [ -f target/release/instaproxy.exe ]; then
    echo "Binary: $(pwd)/target/release/instaproxy.exe"
else
    echo "Binary: $(pwd)/target/release/instaproxy"
fi
