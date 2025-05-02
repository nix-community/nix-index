{
  lib,
  curl,
  darwin,
  openssl,
  pkg-config,
  rustPlatform,
  sqlite,
  stdenv,
}:
rustPlatform.buildRustPackage {
  pname = "nix-index";
  inherit ((lib.importTOML ./Cargo.toml).package) version;

  src = lib.sourceByRegex ./. [
    "(examples|src)(/.*)?"
    ''Cargo\.(toml|lock)''
    ''command-not-found\.sh''
  ];

  cargoLock = {
    lockFile = ./Cargo.lock;
  };

  nativeBuildInputs = [ pkg-config ];
  buildInputs = [
    openssl
    curl
    sqlite
  ] ++ lib.optionals stdenv.isDarwin [ darwin.apple_sdk.frameworks.Security ];

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
}
