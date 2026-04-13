{
  description = "tonepoet - standalone CLI + TUI audio conversion toolkit";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    sox_ng = {
      url = "github:barstoolbluz/sox_ng";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    opustags = {
      url = "github:barstoolbluz/build-opustags";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    ssrc = {
      url = "github:barstoolbluz/ssrc";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, sox_ng, opustags, ssrc }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
          config.allowUnfree = true;
        };

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };

        nativeBuildInputs = with pkgs; [
          rustToolchain
          pkg-config
        ];

        buildInputs = with pkgs; [
          openssl
        ];

        # opus-tools — fix missing __opus_check_int/_ptr inline helpers
        opus-tools-fixed = pkgs.opus-tools.overrideAttrs (old: {
          postPatch = (old.postPatch or "") + ''
            sed -i '1i \
            #include <opus/opus_types.h>\
            static inline int __opus_check_int(int x) { (void)x; return x; }\
            static inline int *__opus_check_int_ptr(int *x) { return x; }' src/opusenc.c
          '';
        });

        # loudgain — pin ffmpeg 6 (loudgain's scan.c is incompatible with ffmpeg 7)
        loudgain = pkgs.loudgain.overrideAttrs (old: {
          buildInputs = map (dep:
            if pkgs.lib.getName dep == "ffmpeg" then pkgs.ffmpeg_6 else dep
          ) old.buildInputs;
          hardeningDisable = [ "all" ];
          NIX_CFLAGS_COMPILE = "-Wno-error -Wno-deprecated-declarations";
        });

        # Runtime dependencies for audio conversion
        runtimeDeps = [
          opustags.packages.${system}.default
          sox_ng.packages.${system}.default
          ssrc.packages.${system}.default
          (pkgs.ffmpeg_7-full.override { withUnfree = true; })
          loudgain
        ] ++ [
          opus-tools-fixed
        ] ++ (with pkgs; [
          flac
          libopus
          opusfile
          wavpack
          lame
          _7zz
          atomicparsley
        ]);

      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "tonepoet";
          version = "0.1.0";

          src = ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          nativeBuildInputs = nativeBuildInputs ++ [
            pkgs.makeWrapper
            pkgs.llvmPackages.libclang
            pkgs.clang
          ];
          buildInputs = buildInputs ++ [
            (pkgs.ffmpeg_7-full.override { withUnfree = true; })
          ];

          env.LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
          env.BINDGEN_EXTRA_CLANG_ARGS = "-isystem ${pkgs.llvmPackages.libclang.lib}/lib/clang/${pkgs.lib.getVersion pkgs.llvmPackages.clang}/include -isystem ${pkgs.glibc.dev}/include";

          postInstall = ''
            wrapProgram $out/bin/tonepoet \
              --prefix PATH : ${pkgs.lib.makeBinPath runtimeDeps}
          '';

          meta = with pkgs.lib; {
            description = "Standalone CLI + TUI audio conversion toolkit";
            license = licenses.mit;
            mainProgram = "tonepoet";
          };
        };

        devShells.default = pkgs.mkShell {
          inherit buildInputs;
          nativeBuildInputs = nativeBuildInputs ++ runtimeDeps ++ (with pkgs; [
            cargo-watch
            cargo-edit
            rust-analyzer
            llvmPackages.libclang
            clang
          ]);

          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

          shellHook = ''
            echo "tonepoet development environment"
            echo ""
            echo "  cargo build    - Build the project"
            echo "  cargo run      - Run tonepoet"
            echo "  cargo test     - Run tests"
            echo ""
          '';
        };

        apps.default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/tonepoet";
        };
      }
    );
}
