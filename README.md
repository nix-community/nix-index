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
First, you need to generate an index by running `nix-index` (it takes around 5 minutes) . Then, you can use `nix-locate pattern`. For more information, see `nix-locate --help` and `nix-index --help`.

### Use pre-generated database

[nix-index-database](https://github.com/Mic92/nix-index-database) provides pre-generated databases if you don't want to generate a database locally.
It also comes with nixos/home-manager modules to use those databases.

### Usage as a command-not-found replacement

Nix-index provides a "command-not-found" script that can print for you the attribute path of unfound commands in your shell. You can either source `${pkgs.nix-index}/etc/command-not-found.sh` in your own shell init files (works for ZSH and Bash for as far as we know) or you can use the following in home-manager / `/etc/nixos/configuration.nix`:

```nix
    programs.command-not-found.enable = false;
    # for home-manager, use programs.bash.initExtra instead
    programs.bash.interactiveShellInit = ''
      source ${pkgs.nix-index}/etc/profile.d/command-not-found.sh
    '';
```

Replace `bash` with `zsh` if you use `zsh`.

Example output:

```
$ blender
The program 'blender' is currently not installed. You can install it
by typing:
  nix-env -iA nixpkgs.blender.out

Or run it once with:
  nix-shell -p blender.out --run ...
```

A [`home-manager` module](https://nix-community.github.io/home-manager/options.html#opt-programs.nix-index.enable) is now available to integrate `nix-index` with `bash`, `zsh`, and `fish` using this script.

You can also use `command-not-found.nu` as a Nushell hook by adding the
following to your Nushell config:

```nix
  programs.nushell = {
    enable = true;
    extraConfig = ''
      $env.config.hooks.command_not_found = source ${pkgs.nix-index}/etc/profile.d/command-not-found.nu
    '';
  };
```

### Faster `command_not_found` Index

The default setup above is great to see _everything_ in every package, but filtering this to only what is needed makes this much faster.

Using hyperfine, it takes ~2 seconds using the full index. Using the index generated with the options below will have all the same functionality for a `command_not_found` function/command:

```
$ ...nix-index --filter-prefix '/bin/' --db ~/.cache/nix-index-not-found/ ...
```

then in the script you are using for `command_not_found` add the `--db ~/.cache/nix-index-not-found/` option to use the smaller index.

#### Speeds

Speeds also include the time it takes to build the index once. This might not be super accurate, though I used a pre-generated cache to make sure that the speed would be as similar as possible.

The first row is the default index that was created using the plain `nix-index` command. The second row is using the filtered index. The options above move the normal index location so that `nix-locate` will retain its current functionality.

|          build_time          |         speed          |     stddev      |   size   |
|------------------------------|------------------------|-----------------|----------|
| 6min 11sec 884ms 322µs 582ns | 2sec 328ms 569µs 676ns | 83ms 829µs 45ns | 83.6 MiB |
|      42sec 518ms 400µs 433ns |       30ms 654µs 262ns | 4ms 199µs 949ns |  1.5 MiB |

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
