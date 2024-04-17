{
  description = "A files database for nixpkgs";

  inputs = {
    nixpkgs.url = "nixpkgs/nixos-unstable";
    flake-compat.url = "github:nix-community/flake-compat";
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
            "(examples|src)(/.*)?"
            ''Cargo\.(toml|lock)''
            ''command-not-found\.sh''
          ];

          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          nativeBuildInputs = [ pkg-config ];
          buildInputs = [ openssl curl sqlite ]
            ++ lib.optionals stdenv.isDarwin [ darwin.apple_sdk.frameworks.Security ];

          postInstall = ''
            substituteInPlace command-not-found.sh \
              --subst-var out
            install -Dm555 command-not-found.sh -t $out/etc/profile.d
          '';

          meta = with lib; {
            description = "A files database for nixpkgs";
            homepage = "https://github.com/nix-community/nix-index";
            license = with licenses; [ bsd3 ];
            maintainers = [ maintainers.bennofs ];
          };
        };
      });

      checks = forAllSystems (system:
          let
            packages = lib.mapAttrs' (n: lib.nameValuePair "package-${n}") self.packages.${system};
            devShells = lib.mapAttrs' (n: lib.nameValuePair "devShell-${n}") self.devShells.${system};
          in packages // devShells
      );

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
