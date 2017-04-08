let
  defaultNixpkgs = builtins.fetchTarball "github.com/NixOS/nixpkgs/archive/8889d405c7aa22ae7d1e571d3f3c30b14e2faafd.tar.gz" ;
in
{ nixpkgs ? defaultNixpkgs }:

with (import nixpkgs {}); with rustPlatform;

buildRustPackage rec {
  name = "nix-index-${version}";
  version = "0.1.0";

  src = builtins.filterSource (name: type: !lib.hasPrefix "target" (baseNameOf name) && !lib.hasPrefix "result" (baseNameOf name)) ./.;
  depsSha256 = "1hqlshh928jmpj1lm07h63f47p2amfg6qw5plfvwk0xbas6hg9fw";
  buildInputs = [pkgconfig openssl curl];

  meta = with stdenv.lib; {
    description = "A files database for nixpkgs";
    homepage = https://github.com/bennofs/nix-index;
    license = with licenses; [ bsd3 ];
    maintainers = [ maintainers.bennofs ];
    platforms = platforms.all;
  };
}
