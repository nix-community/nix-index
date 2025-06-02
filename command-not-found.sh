#!/bin/sh

# for bash 4
# this will be called when a command is entered
# but not found in the user’s path + environment
command_not_found_handle () {

    # TODO: use "command not found" gettext translations

    # taken from http://www.linuxjournal.com/content/bash-command-not-found
    # - do not run when inside Midnight Commander or within a Pipe
    if [ -n "${MC_SID-}" ] || ! [ -t 1 ]; then
        >&2 echo "$1: command not found"
        return 127
    fi

    toplevel=nixpkgs # nixpkgs should always be available even in NixOS
    cmd=$1
    attrs=$(@out@/bin/nix-locate --minimal --no-group --type x --type s --top-level --whole-name --at-root "/bin/$cmd")
    len=$(echo -n "$attrs" | grep -c "^")

    case $len in
        0)
            >&2 echo "$cmd: command not found"
            ;;
        1)
            >&2 cat <<EOF
The program '$cmd' is currently not installed.
EOF
            # if only 1 package provides this, then we can invoke it
            # without asking the users if they have opted in with one
            # of 2 environment variables

            # they are based on the ones found in
            # command-not-found.sh:

            #   NIX_AUTO_INSTALL : install the missing command into the
            #                      user’s environment
            #   NIX_AUTO_RUN     : run the command transparently inside of
            #                      nix shell

            # these will not return 127 if they worked correctly

            if ! [ -z "${NIX_AUTO_INSTALL-}" ]; then
                >&2 cat <<EOF
It is provided by the package '$toplevel.${attrs%.out}', which I will now install for you.
EOF
                if [ -e "$HOME/.nix-profile/manifest.json" ]; then
                    nix profile install $toplevel#$attrs
                else
                    nix-env -iA $toplevel.$attrs
                fi
                if [ "$?" -eq 0 ]; then
                    $@ # TODO: handle pipes correctly if AUTO_RUN/INSTALL is possible
                    return $?
                else
                    >&2 cat <<EOF
Failed to install $toplevel.attrs.
$cmd: command not found
EOF
                fi
            elif ! [ -z "${NIX_AUTO_RUN-}" ]; then
                nix-build --no-out-link -A $attrs "<$toplevel>"
                if [ "$?" -eq 0 ]; then
                    # how nix-shell handles commands is weird
                    # $(echo $@) is need to handle this
                    nix-shell -p $attrs --run "$(echo $@)"
                    return $?
                else
                    >&2 cat <<EOF
Failed to install $toplevel.attrs.
$cmd: command not found
EOF
                fi
            else
                # The Correct Way of checking we're running Home Manager
                if [ -n "$__HM_SESS_VARS_SOURCED" ]; then
                    >&2 cat <<EOF
Install it for the current user '$USER' by adding to your Home Manager configuration:
  home.packages = with pkgs; [ ${attrs%.out} ];

EOF
                fi
                # The Correct Way of checking we're running NixOS
                if [ -e "/etc/NIXOS" ]; then
                    >&2 cat <<EOF
Install it for all users by adding to your NixOS configuration:
  environment.systemPackages = with pkgs; [ ${attrs%.out} ];

EOF
                fi
                if [ -e "$HOME/.nix-profile/manifest.json" ]; then
                    >&2 cat <<EOF
You can run it once with:
  nix shell $toplevel#${attrs%.out} -c $cmd

Or install it by typing:
  nix profile install $toplevel#${attrs%.out}
EOF
                else
                    >&2 cat <<EOF
You can run it once with:
  nix-shell -p ${attrs%.out} --run '$cmd'
EOF
                fi
            fi
            ;;
        *)
            >&2 cat <<EOF
The program '$cmd' is currently not installed. It is provided by several packages.
EOF

            # ensure we get each element of attrs in a cross platform way
            if [ -n "$__HM_SESS_VARS_SOURCED" ]; then
                >&2 cat <<EOF
Install it for the current user '$USER' by adding to your Home Manager configuration one of:
EOF
                while read attr; do
                    >&2 echo "  home.packages = with pkgs; [ ${attr%.out} ];"
                done <<< "$attrs"
                echo \n
            fi
            if [ -e "/etc/NIXOS" ]; then
                >&2 cat <<EOF
Install it for all users by adding to your NixOS configuration one of:
EOF
                while read attr; do
                    >&2 echo "  environment.systemPackages = with pkgs; [ ${attr%.out} ];"
                done <<< "$attrs"
                echo \n
            fi
            >&2 cat <<EOF
You can run it once with one of:
EOF
            while read attr; do
                if [ -e "$HOME/.nix-profile/manifest.json" ]; then
                    >&2 echo "  nix shell $toplevel#${attr%.out} -c $cmd"
                else
                    >&2 echo "  nix-shell -p ${attr%.out} --run '$cmd'"
                fi
            done <<< "$attrs"
            if [ -e "$HOME/.nix-profile/manifest.json" ]; then
                >&2 cat <<EOF

Or install it by typing one of:
EOF
                while read attr; do
                    >&2 echo "  nix profile install $toplevel#${attr%.out}"
                done <<< "$attrs"
                echo "\n"
            fi
            ;;
    esac

    return 127 # command not found should always exit with 127
}

# for zsh...
# we just pass it to the bash handler above
# apparently they work identically
command_not_found_handler () {
    command_not_found_handle $@
    return $?
}
