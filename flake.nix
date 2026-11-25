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
        # The terminal pane links libghostty-vt, which ghostty-vt-sys builds from its
        # pinned source with exactly Zig 0.17 (nixpkgs stops at 0.16). The official
        # tarball is a static binary, so it runs unpatched.
        zigTarballs = {
          x86_64-linux = { arch = "x86_64"; sha256 = "3581863e3cbcfd0fce0efc682ce7e9e93da96668a977791d356a26d2a7e9b17a"; };
          aarch64-linux = { arch = "aarch64"; sha256 = "a1546d5646eb184437918d6635ca6b762a0e889eac09cef720ee7a1ed3fe7795"; };
        };
        zig_0_17 = pkgs.stdenvNoCC.mkDerivation rec {
          pname = "zig";
          version = "0.17.1";
          src = pkgs.fetchurl {
            url = "https://ziglang.org/download/${version}/zig-${zigTarballs.${system}.arch}-linux-${version}.tar.xz";
            inherit (zigTarballs.${system}) sha256;
          };
          dontConfigure = true;
          dontBuild = true;
          dontFixup = true;
          installPhase = ''
            mkdir -p $out/bin $out/libexec
            cp -r . $out/libexec/zig
            ln -s $out/libexec/zig/zig $out/bin/zig
          '';
        };
        # ESMFold2 structure prediction, kept out of the default shell: the ROCm
        # torch closure is several GB. See nix/esmfold2.nix.
        esmfold2 = import ./nix/esmfold2.nix { inherit pkgs; accel = "rocm"; };
        esmfold2-cpu = import ./nix/esmfold2.nix { inherit pkgs; accel = "cpu"; };
      in {
        # `nix run .#esmfold2 -- -s MQIFVKTLTG... -o ubq.cif`
        packages.esmfold2 = esmfold2.predict;
        packages.esmfold2-cpu = esmfold2-cpu.predict;
        apps.esmfold2 = { type = "app"; program = "${esmfold2.predict}/bin/esmfold2-predict"; };
        # The Python the ESMFold2 plugin runs in (`nix shell .#esmfold2-env -c python`).
        packages.esmfold2-env = esmfold2.env;
        packages.esmfold2-cpu-env = esmfold2-cpu.env;

        # `nix develop .#esmfold2`: Python with `esm` importable, plus the CLI.
        devShells.esmfold2 = pkgs.mkShell {
          packages = [ esmfold2.env esmfold2.predict ];
        };

        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [ rustToolchain pkg-config cmake perl clang mold zig_0_17 ];
          buildInputs = runtimeLibs ++ (with pkgs; [ alsa-lib openssl zstd zlib libgit2 ]);
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
        };
      });
}
