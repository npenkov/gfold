nix-shell -p openssl zlib libiconv cargo pkg-config darwin.apple_sdk.frameworks.Security --run "cargo build --release"
