nix-shell -p openssl libiconv pkg-config darwin.apple_sdk.frameworks.Security --run "cargo build --release"
