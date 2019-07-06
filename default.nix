let
  # nixpkgs-unstable at 2019-07-06 16:03
  nixpkgsRev = "df738814d1bed1a554eac1536e99253ab75ba012";
  defaultNixpkgs = builtins.fetchTarball "github.com/NixOS/nixpkgs/archive/${nixpkgsRev}.tar.gz";
in
{ nixpkgs ? defaultNixpkgs }:

with (import nixpkgs {}); with rustPlatform;

buildRustPackage rec {
  name = "nix-index-${version}";
  version = "0.1.2";

  src = builtins.filterSource (name: type: !lib.hasPrefix "target" (baseNameOf name) && !lib.hasPrefix "result" (baseNameOf name) && name != ".git") ./.;
  buildInputs = [pkgconfig openssl curl];
  cargoSha256 = "10cg4wf36hkzp4fbws0f6wk12zkh6gsy92raq4d6kyhp7myp7p3d";

  postInstall = ''
    mkdir -p $out/etc/profile.d
    cp ${./command-not-found.sh} $out/etc/profile.d/command-not-found.sh
    substituteInPlace $out/etc/profile.d/command-not-found.sh \
      --replace "@out@" "$out"
  '';

  meta = with stdenv.lib; {
    description = "A files database for nixpkgs";
    homepage = https://github.com/bennofs/nix-index;
    license = with licenses; [ bsd3 ];
    maintainers = [ maintainers.bennofs ];
    platforms = platforms.all;
  };
}
