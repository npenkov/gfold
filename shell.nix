{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  buildInputs = with pkgs; [
    openssl
    libiconv
    pkg-config
    zlib
    rustup
  ];

  # Set environment variables for the build
  OPENSSL_DIR = "${pkgs.openssl.dev}";
  OPENSSL_LIB_DIR = "${pkgs.openssl.out}/lib";
  PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";

  shellHook = ''
    # Ensure rustup is initialized and use stable toolchain
    if [ ! -d "$HOME/.rustup" ]; then
      rustup-init -y --default-toolchain stable --profile minimal
    fi
    export PATH="$HOME/.cargo/bin:$PATH"
  '';
}
