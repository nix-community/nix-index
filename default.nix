let
  nixpkgsRev = "bd78749d3387f84d4f70dada335df04479f8170c";
  defaultNixpkgs = builtins.fetchTarball "github.com/NixOS/nixpkgs/archive/${nixpkgsRev}.tar.gz";
in
{ nixpkgs ? defaultNixpkgs }:

with (import nixpkgs {}); with rustPlatform;

buildRustPackage rec {
  name = "nix-index-${version}";
  version = "0.1.0";

  src = builtins.filterSource (name: type: !lib.hasPrefix "target" (baseNameOf name) && !lib.hasPrefix "result" (baseNameOf name)) ./.;
  depsSha256 = "15z3saizbyyjdwfrfz94sznxn2pzm7zl8vgp9ps5grva5k9izaxg";
  buildInputs = [pkgconfig openssl curl];

  meta = with stdenv.lib; {
    description = "A files database for nixpkgs";
    homepage = https://github.com/bennofs/nix-index;
    license = with licenses; [ bsd3 ];
    maintainers = [ maintainers.bennofs ];
    platforms = platforms.all;
  };
}
