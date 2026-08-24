{
  description = "SerdesAI - Rust AI agent framework";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        rust = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rustfmt" "clippy" "rust-analyzer" ];
        };
      in {
        devShells.default = pkgs.mkShell {
          packages = [ rust pkgs.openssl pkgs.pkg-config pkgs.clang pkgs.lld ];
          # openssl-sys discovery: explicit lib/include dirs; pkg-config as fallback.
          OPENSSL_LIB_DIR = "${pkgs.openssl.out}/lib";
          OPENSSL_INCLUDE_DIR = "${pkgs.openssl.dev}/include";
          PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";
          # Runtime resolution of libssl.so.3 for binaries built in the shell.
          LD_LIBRARY_PATH = "${pkgs.openssl.out}/lib";
        };
      }
    );
}
