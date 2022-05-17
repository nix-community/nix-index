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
      nix-index = with nixpkgsFor.${system}; rustPlatform.buildRustPackage  {
        pname = "nix-index";
        version = "0.1.3";

        src = self;

        nativeBuildInputs = [ pkg-config ];
        buildInputs = [ openssl curl ]
          ++ lib.optionals stdenv.hostPlatform.isDarwin [ darwin.apple_sdk.frameworks.Security libiconv ];
        cargoLock = {
          lockFile = ./Cargo.lock;
        };

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
    defaultPackage = forAllSystems (system: self.packages.${system}.nix-index);
    devShell = forAllSystems (system: with nixpkgsFor.${system}; stdenv.mkDerivation {
      name = "nix-index";
      nativeBuildInputs = [ rustc cargo pkg-config  ];
      buildInputs = [ openssl curl ]
          ++ lib.optional stdenv.hostPlatform.isDarwin darwin.apple_sdk.frameworks.Security;
      enableParallelBuilding = true;
    });
  };
}
