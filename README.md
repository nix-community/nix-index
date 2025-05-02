# nix-index
## A files database for nixpkgs
**nix-index** is a tool to quickly locate the package providing a certain file in [`nixpkgs`](https://github.com/NixOS/nixpkgs). It indexes built derivations found in binary caches. 

###### Demo

```
$ nix-locate 'bin/hello'
hello.out                                        29,488 x /nix/store/bdjyhh70npndlq3rzmggh4f2dzdsj4xy-hello-2.10/bin/hello
linuxPackages_4_4.dpdk.examples               2,022,224 x /nix/store/jlnk3d38zsk0bp02rp9skpqk4vjfijnn-dpdk-16.07.2-4.4.52-examples/bin/helloworld
linuxPackages.dpdk.examples                   2,022,224 x /nix/store/rzx4k0pb58gd1dr9kzwam3vk9r8bfyv1-dpdk-16.07.2-4.9.13-examples/bin/helloworld
linuxPackages_4_10.dpdk.examples              2,022,224 x /nix/store/wya1b0910qidfc9v3i6r9rnbnc9ykkwq-dpdk-16.07.2-4.10.1-examples/bin/helloworld
linuxPackages_grsec_nixos.dpdk.examples       2,022,224 x /nix/store/2wqv94290pa38aclld7sc548a7hnz35k-dpdk-16.07.2-4.9.13-examples/bin/helloworld
camlistore.out                                7,938,952 x /nix/store/xn5ivjdyslxldhm5cb4x0lfz48zf21rl-camlistore-0.9/bin/hello
```
## Installation

### Flakes

1. create the database:

   ```
   $ nix run github:nix-community/nix-index#nix-index
   ```

2. query for a file:

   ```
   $ nix run github:nix-community/nix-index#nix-locate -- bin/hello
   ```

### Latest Git version

To install the latest development version of nix-index, simply clone the repo and run `nix-env -if.`:

```
$ git clone https://github.com/nix-community/nix-index
$ cd nix-index
$ nix-env -if.
```

### Stable

For the stable version, you can either [checkout](https://git-scm.com/docs/git-checkout) the latest [tag](https://git-scm.com/docs/git-tag) (see the list [here](https://github.com/nix-community/nix-index/tags)) or use Nixpkgs' repositories' and install it with:

```
$ nix-env -iA nixos.nix-index
```

## Usage

First, you need to generate an index by running `nix-index`, which takes around 10 minutes.
Then, you can use `nix-locate <pattern>`.
For more information, see `nix-locate --help` and `nix-index --help`.

### As a replacement for `command-not-found`

NixOS allows displaying suggestions for packages using [`command-not-found`](https://search.nixos.org/options?show=programs.command-not-found.enable).
`nix-index` provides a replacement for `command-not-found` with more elaborate suggestions.

#### NixOS

```nix
{ ... }:
{
  programs.command-not-found.enable = false;
  programs.nix-index = {
    enable = true;
    enableBashIntegration = true;
    enableZshIntegration = true;
    enableFishIntegration = true;
  };
}
```

Refer to [`programs.nix-index`](https://search.nixos.org/options?query=nix-index) option documentation for details.

Example output:

```
$ blender
The program 'blender' is currently not installed.
You can install it for all users by adding to your NixOS configuration:
  environment.systemPackages = with pkgs; [ blender ];

Or run it once with:
  nix-shell -p blender --run blender
```

#### Home Manager

```nix
{ ... }:
{
  programs.command-not-found.enable = false;
  programs.nix-index = {
    enable = true;
    enableBashIntegration = true;
    enableZshIntegration = true;
    enableFishIntegration = true;
  };
}
```

Refer to Home Manager option documentation on [`programs.nix-index`](https://nix-community.github.io/home-manager/options.xhtml#opt-programs.nix-index.enable) for details.

Example output:

```
$ blender
The program 'blender' is currently not installed.
You can install it for the current user '$USER' by adding to your Home Manager configuration:
  home.packages = with pkgs; [ blender ];

Or run it once with:
  nix-shell -p blender --run blender
```

### With a pre-generated database

[`nix-index-database`](https://github.com/Mic92/nix-index-database) provides pre-generated databases if you don't want to generate a database locally.
It also comes with modules for NixOS, Home Manager, and nix-darwin to use those databases.

## Contributing
If you find any missing features that you would like to implement, I'm very happy about any PRs! You can also create an issue first if the feature is more complex so we can discuss possible implementations.

Here is a quick description of all relevant files:

* `bin/{nix-index, nix-locate}.rs`: Implementation of the nix-index / nix-locate command line tools
* `src/database.rs`: High-level functions for working with the database format
* `src/files.rs`: The data types for working with file listings
* `src/frcode.rs`: Low-level implementation of an encoder to efficiently store many file paths (see comments in the file for more details). Used by `database.rs`.
* `src/hydra.rs`: Deals with everything that has to do with downloading from the binary cache (fetching file listings and references)
* `src/nixpkgs.rs`: Implements the gathering of the packages (store paths and attributes) using `nix-env`
* `src/package.rs`: High-level data types for representing store paths (sometimes also refered to as a package)
* `src/workset.rs`: A queue used by `nix-index` to implement the recursive fetching (fetching references of everything)
