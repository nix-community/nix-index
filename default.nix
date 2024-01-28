{
  system ? builtins.currentSystem,
  inputs ? import (fetchTarball "https://github.com/fricklerhandwerk/flake-inputs/tarball/1.0") {
    root = ./.;
  },
  nixpkgs-config ? {
    inherit system;
    config = { };
    overlays = [ ];
  },
}:
let
  # avoid re-importing `nixpkgs` if it comes from `flake.nix`
  pkgs =
    if inputs.nixpkgs ? lib then
      inputs.nixpkgs.legacyPackages.${system}
    else
      import inputs.nixpkgs nixpkgs-config;
in
rec {
  packages = {
    default = pkgs.callPackage ./package.nix { };
  };
  devShells = {
    minimal =
      with pkgs;
      mkShell {
        name = "nix-index";

        nativeBuildInputs = [
          pkg-config
        ];

        buildInputs =
          [
            openssl
            sqlite
          ]
          ++ lib.optionals stdenv.isDarwin [
            darwin.apple_sdk.frameworks.Security
          ];

        env.LD_LIBRARY_PATH = lib.makeLibraryPath [ openssl ];
      };

    default =
      with pkgs;
      mkShell {
        name = "nix-index";

        inputsFrom = [ devShells.minimal ];

        nativeBuildInputs = [
          rustc
          cargo
          clippy
          rustfmt
        ];

        env = {
          LD_LIBRARY_PATH = lib.makeLibraryPath [ openssl ];
          RUST_SRC_PATH = rustPlatform.rustLibSrc;
        };
      };

  };
  app-shell =
    with pkgs;
    mkShellNoCC {
      name = "nix-index";
      packages = [ packages.default ];
    };
}
