let
  # nixpkgs-unstable at 2021-10-16 18:26
  nixpkgsRev = "8e1eab9eae4278c9bb1dcae426848a581943db5a";
  defaultNixpkgs = builtins.fetchTarball "github.com/NixOS/nixpkgs/archive/${nixpkgsRev}.tar.gz";
in
{ nixpkgs ? defaultNixpkgs, pkgs ? import nixpkgs {}, ... }:

with pkgs; with rustPlatform;

buildRustPackage rec {
  name = "nix-index-${version}";
  version = "0.1.3";

  src = builtins.filterSource (name: type: !lib.hasPrefix "target" (baseNameOf name) && !lib.hasPrefix "result" (baseNameOf name) && name != ".git") ./.;
  buildInputs = [openssl curl];
  nativeBuildInputs = [ pkg-config ];

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
}
