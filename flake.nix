{
  description = "A files database for nixpkgs";

  inputs = {
    nixpkgs.url = "nixpkgs/nixos-unstable";
    flake-compat = {
      url = "github:edolstra/flake-compat";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, flake-compat }: let
    systems = [ "x86_64-linux" "x86_64-darwin" "aarch64-darwin" "aarch64-linux" ];
    forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);
    nixpkgsFor = forAllSystems (system: import nixpkgs { inherit system; });
  in {
    packages = forAllSystems (system: {
      default = with nixpkgsFor.${system}; rustPlatform.buildRustPackage  {
        pname = "nix-index";
        version = "0.1.5";

        src = self;

        nativeBuildInputs = [ pkg-config ];
        buildInputs = [ openssl curl sqlite ]
          ++ lib.optionals stdenv.hostPlatform.isDarwin [ darwin.apple_sdk.frameworks.Security libiconv ];
        cargoLock = {
          lockFile = ./Cargo.lock;
        };

        preUnpack = ''
          mkdir tmp
          cp -pr -L --reflink=auto -- "$cargoDeps" "tmp/$(stripHash "$cargoDeps")"
          chmod -R a+w "tmp/$(stripHash "$cargoDeps")"
          export cargoDeps="$(pwd)/tmp/$(stripHash "$cargoDeps")"
        '';

        postInstall = ''
          mkdir -p $out/etc/profile.d
          cp ${./command-not-found.sh} $out/etc/profile.d/command-not-found.sh
          substituteInPlace $out/etc/profile.d/command-not-found.sh \
            --replace "@out@" "$out"
        '';

        meta = with lib; {
          description = "A files database for nixpkgs";
          homepage = https://github.com/bennofs/nix-index;
          license = with licenses; [ bsd3 ];
          maintainers = [ maintainers.bennofs ];
          platforms = platforms.all;
        };
      };
    });
    checks = forAllSystems (system: {
      nix-index = self.packages.nix-index.${system};
    });
    devShell = forAllSystems (system: with nixpkgsFor.${system}; stdenv.mkDerivation {
      name = "nix-index";

      RUST_SRC_PATH = "${pkgs.rust.packages.stable.rustPlatform.rustLibSrc}";

      nativeBuildInputs = [ rustc cargo pkg-config clippy rustfmt ];
      buildInputs = [ openssl curl sqlite ]
          ++ lib.optional stdenv.hostPlatform.isDarwin darwin.apple_sdk.frameworks.Security;
      enableParallelBuilding = true;
    });
    apps = forAllSystems (system: {
      nix-index = {
        type = "app";
        program = "${self.packages.${system}.default}/bin/nix-index";
      };
      nix-locate = {
        type = "app";
        program = "${self.packages.${system}.default}/bin/nix-locate";
      };
      default = self.apps.${system}.nix-locate;
    });
  };
}
