{
  description = "plasmid-gpui - proof-of-concept plasmid sequence editor built on GPUI";

  # Enter with `nix develop`, then `cargo run --release -- <folder with .gb files>`.

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" ] (system:
      let
        pkgs = import nixpkgs { inherit system; overlays = [ (import rust-overlay) ]; };
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
        };
        # Libraries GPUI loads at runtime (Vulkan renderer, Wayland/X11 windowing, fonts).
        runtimeLibs = with pkgs; [
          vulkan-loader wayland libxkbcommon libGL
          libx11 libxcb libxcursor libxi libxrandr
          fontconfig freetype
        ];
      in {
        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [ rustToolchain pkg-config cmake perl clang mold ];
          buildInputs = runtimeLibs ++ (with pkgs; [ alsa-lib openssl zstd zlib libgit2 ]);
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
        };
      });
}
