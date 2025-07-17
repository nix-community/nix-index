## 0.1.10 [Unreleased]
### Added
### Fixed
### Changed

## 0.1.9
### Added

* `texlive.pkgs` is added to the `EXTRA_SCOPES`, i.e. Texlive packages can be searched now. (#254)
* Support for command-not-found for Nushell has been added. (#271)

### Fixed

* Packages with no files in the index are now excluded, this significantly reduces the index size (~4MB → 1MB) (#266)
* debuginfo processing now skips symlinks which were contained inside of `$debug/lib/debug`. (#278)

### Changed

* `--top-level` is now the default. To use the old default, add `--all`. (#243)
* Chore updates of the Flake lock, dependencies, clippy and a move to `thiserror` rather than `error_chain`. (#279)

## 0.1.8
### Added

* use HTTPS for cache.nixos.org (#246)
* client now use reqwest to obtain HTTPS, content decoding, timeouts and more for free (#247)

### Fixed

* nix-community buildbot CI builds all attributes out of the box (#240)

### Changed

### Removed

## 0.1.7
### Added
* improve help message for `nix-locate --type` (issue #204)
* improve error message when `nix-env` is killed by a signal
### Fixed
* disable nixpkgs overlays (issue #161)
* fix Nix command suggestions for command-not-found.sh (issue #185)
### Changed
* update dependencies
### Removed

## 0.1.6
### Added
* support setting `NIX_INDEX_DATABASE` environment variable to set database location (#213), thanks @mmarx
* this version and future versions will be published to https://crates.io, simplifying library usage in other projects
### Fixed
### Changed
### Removed

## 0.1.5
### Added
### Fixed
* fix crash when using wildcard pattern with nix-locate (issue #205)
### Changed
### Removed

## 0.1.4 - 2023-01-13
### Added
### Fixed
* fix RUSTSEC-2021-0131 (integer overflow in brotli) by migrating away from `brotli2` crate
* fix RUSTSEC-2022-0006 (data race in `thread_local`) by updating `thread_local`
* fix panic when using `--type` CLI (issue #202)
### Changed
* update all dependencies in Cargo.lock

### 0.1.3 - 2023-01-10
### Added
* flake.nix added to repository, allows directly running nix-index from git (#162), thanks @matthewbauer
* support for proxies (#132), thanks @whizsid
* command-not-found.sh suggests new `nix profile` command if manifest.json exists (#135), thanks @matthewbauer
* support building project via Nix on Darwin (#175), thanks @BrianHicks
* indexer supports prefix filtering (#177), rhanks @virchau13
* command-line option to specify system for which to build the index (#183), thanks @usertam
* nix-channel-index: new command to build a programs.sqlite as currently distributed with nix channels (#192), thanks @K900
### Fixed
* command-not-found.sh never accesses undefined variables anymore (allows set -u) (#123), thanks @matthewbauer
* support xlibs renamed to xorg in recent nixpkgs (#179), thanks @cole-h
### Changed
* rust dependencies updated to latest versions, thanks @elude03, @berbiche, @Sciecentistguy, @Mic92
* nix-env is now invoked in parallel to query paths (improves performance)
* performance improvement: multithread compression (#152), thanks @enolan
* performance improvement: reduce compression level from 22 to 19 (#152), thanks @enolan
* performance improvement: get store paths from nix-env in parallel (#152), thanks @enolan

## 0.1.2 - 2018-09-18
### Added
### Fixed
* don't stop when a single request fails (thanks @jameysharp)
### Changed
### Removed

## 0.1.1 - 2018-01-26
### Added
* `--show-trace` command line option
### Fixed
### Changed
### Removed

## 0.1.0 - 2017-07-22
### Added
* Initial release
