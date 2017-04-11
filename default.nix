let
  defaultNixpkgs = builtins.fetchTarball "github.com/NixOS/nixpkgs/archive/285097af2781cde7eaf819dba39f6d2bfad51692.tar.gz" ;
in
{ nixpkgs ? defaultNixpkgs }:

with (import nixpkgs {}); with rustPlatform;

buildRustPackage rec {
  name = "nix-index-${version}";
  version = "0.1.0";

  src = builtins.filterSource (name: type: !lib.hasPrefix "target" (baseNameOf name) && !lib.hasPrefix "result" (baseNameOf name)) ./.;
  depsSha256 = "04hw4wq24xi6qm92ma1dksm4csknqygxkpini5ibs9sfhzs90y10";
  buildInputs = [pkgconfig openssl curl];

  meta = with stdenv.lib; {
    description = "A files database for nixpkgs";
    homepage = https://github.com/bennofs/nix-index;
    license = with licenses; [ bsd3 ];
    maintainers = [ maintainers.bennofs ];
    platforms = platforms.all;
  };
}
