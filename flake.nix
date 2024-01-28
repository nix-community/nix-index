{
  description = "A files database for nixpkgs";

  inputs = {
    nixpkgs.url = "nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      ...
    }@inputs:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        inherit (nixpkgs) lib;
        classic = import ./. { inherit system inputs; };
      in
      {

        inherit (classic) packages devShells;

        apps = {
          nix-index = {
            type = "app";
            program = "${self.packages.${system}.default}/bin/nix-index";
          };
          nix-locate = {
            type = "app";
            program = "${self.packages.${system}.default}/bin/nix-locate";
          };
          default = self.apps.${system}.nix-locate;
        };

        checks =
          let
            packages = lib.mapAttrs' (n: lib.nameValuePair "package-${n}") self.packages.${system};
            devShells = lib.mapAttrs' (n: lib.nameValuePair "devShell-${n}") self.devShells.${system};
          in
          packages // devShells;
      }
    );
}
