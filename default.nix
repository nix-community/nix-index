let
  # nixpkgs-unstable at 2017-07-21 12:12
  nixpkgsRev = "1d78df27294017d464e60bf9b0595e7e42cfca55";
  defaultNixpkgs = builtins.fetchTarball "github.com/NixOS/nixpkgs/archive/${nixpkgsRev}.tar.gz";
in
{ nixpkgs ? defaultNixpkgs }:

with (import nixpkgs {}); with rustPlatform;

let
  registry = rustRegistry.overrideAttrs (old: {
    name = "rustRegistry-2017-07-21";
    src = fetchFromGitHub {
      owner = "rust-lang";
      repo = "crates.io-index";
      rev = "25ee65416ccd75fd0bdbcc31affb119513dd2951";
      sha256 = "1y2mlj31xpzw5h2l4rl1r74gc58lagqsmcmfj0fjgmjzv7glqcwz";
    };
  });
in buildRustPackage rec {
  name = "nix-index-${version}";
  version = "0.1.0";

  rustRegistry = registry;

  src = builtins.filterSource (name: type: !lib.hasPrefix "target" (baseNameOf name) && !lib.hasPrefix "result" (baseNameOf name) && name != ".git") ./.;
  depsSha256 = "0v145fi9bfiwvsdy7hz9lw4m2f2j8sxvixfzmjwfnq4klm51c8yl";
  buildInputs = [pkgconfig openssl curl];

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
