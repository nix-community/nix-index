{ |cmd_name|
  let comma_found = {
      if (which comma | is-not-empty) {
        $"  comma ($cmd_name)"
      }
    }
  let install = { |pkgs|
    $pkgs | each {|pkg| $"  nix shell nixpkgs#($pkg)" }
  }
  let run_once = { |pkgs|
    $pkgs | each {|pkg| $"  nix shell nixpkgs#($pkg) --command '($cmd_name) ...'" }
  }
  let single_pkg = { |pkg|
    let lines = [
      $"The program '($cmd_name)' is currently not installed."
      ""
      "You can install it by typing:"
      (do $install [$pkg] | get 0)
      ""
      "Or run it once with:"
      (do $run_once [$pkg] | get 0)
    ]
    $lines | append (do $comma_found) | str join "\n"
  }
  let multiple_pkgs = { |pkgs|
    let lines = [
      $"The program '($cmd_name)' is currently not installed. It is provided by several packages."
      ""
      "You can install it by typing one of the following:"
      (do $install $pkgs | str join "\n")
      ""
      "Or run it once with:"
      (do $run_once $pkgs | str join "\n")
    ]
    $lines | append (do $comma_found) | str join "\n"
  }
  let pkgs = (@out@/bin/nix-locate --minimal --no-group --type x --type s --whole-name --at-root $"/bin/($cmd_name)" | lines)
  let len = ($pkgs | length)
  let ret = match $len {
    0 => null,
    1 => (do $single_pkg ($pkgs | get 0)),
    _ => (do $multiple_pkgs $pkgs),
  }
  return $ret
}
