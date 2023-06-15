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

      checks = forAllSystems (system: {
        nix-index = self.packages.${system}.default;
      });

      devShells = forAllSystems (system: {
        default = with nixpkgsFor.${system}; stdenv.mkDerivation {
          name = "nix-index";

          RUST_SRC_PATH = rustPlatform.rustLibSrc;

          nativeBuildInputs = [ rustc cargo pkg-config clippy rustfmt ];
          buildInputs = [ openssl curl sqlite ]
            ++ lib.optional stdenv.isDarwin darwin.apple_sdk.frameworks.Security;
          enableParallelBuilding = true;
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
