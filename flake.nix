{
  description = "A files database for nixpkgs";

  inputs = {
    nixpkgs.url = "nixpkgs/nixos-unstable";
    flake-compat = {
      url = "github:edolstra/flake-compat";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, flake-compat }:
    let
      inherit (nixpkgs) lib;
      systems = [ "x86_64-linux" "x86_64-darwin" "aarch64-darwin" "aarch64-linux" ];
      forAllSystems = lib.genAttrs systems;
      nixpkgsFor = nixpkgs.legacyPackages;
    in
    {
      packages = forAllSystems (system: {
        default = with nixpkgsFor.${system}; rustPlatform.buildRustPackage {
          pname = "nix-index";
          inherit ((lib.importTOML ./Cargo.toml).package) version;

          src = lib.sourceByRegex self [
            "(etc|examples|src)(/.*)?"
            ''Cargo\.(toml|lock)''
          ];

          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          nativeBuildInputs = [ pkg-config ];
          buildInputs = [ openssl curl sqlite ]
            ++ lib.optionals stdenv.isDarwin [ darwin.apple_sdk.frameworks.Security ];

          postInstall = ''
            substituteInPlace etc/command-not-found.* \
              --subst-var out
            install -Dm444 etc/command-not-found.* -t $out/etc/profile.d
          '';

          meta = with lib; {
            description = "A files database for nixpkgs";
            homepage = "https://github.com/nix-community/nix-index";
            license = with licenses; [ bsd3 ];
            maintainers = [ maintainers.bennofs ];
          };
        };
      });

      checks = forAllSystems (system: {
        nix-index = self.packages.${system}.default;
      });

      devShells = forAllSystems (system: {
        minimal = with nixpkgsFor.${system}; mkShell {
          name = "nix-index";

          nativeBuildInputs = [
            pkg-config
          ];

          buildInputs = [
            openssl
            sqlite
          ] ++ lib.optionals stdenv.isDarwin [
            darwin.apple_sdk.frameworks.Security
          ];

          env.LD_LIBRARY_PATH = lib.makeLibraryPath [ openssl ];
        };

        default = with nixpkgsFor.${system}; mkShell {
          name = "nix-index";

          inputsFrom = [ self.devShells.${system}.minimal ];

          nativeBuildInputs = [ rustc cargo clippy rustfmt ];

          env = {
            LD_LIBRARY_PATH = lib.makeLibraryPath [ openssl ];
            RUST_SRC_PATH = rustPlatform.rustLibSrc;
          };
        };
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
