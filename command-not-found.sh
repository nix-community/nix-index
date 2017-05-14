#!/bin/sh

# for bash 4
# this will be called when a command is entered
# but not found in the user’s path + environment
command_not_found_handle () {

    # TODO: use "command not found" gettext translations

    # taken from http://www.linuxjournal.com/content/bash-command-not-found
    # - do not run when inside Midnight Commander or within a Pipe
    if [ -n "$MC_SID" ] || ! [ -t 1 ]; then
        >&2 echo "$1: command not found"
        return 127
    fi

    toplevel=nixpkgs # nixpkgs should always be available even in NixOS
    cmd=$1
    attrs=$(@out@/bin/nix-locate --minimal --no-group --type x --top-level --whole-name --at-root "/bin/$cmd")
    len=$(echo -n "$attrs" | grep -c "^")

    case $len in
        0)
            >&2 echo "$cmd: command not found"
            ;;
        1) # TODO: add option to autorun with 1 match
            >&2 echo "The program '$cmd' is currently not installed. You can install it"
            >&2 echo "by typing:"
            >&2 echo "  nix-env -iA $toplevel.$attrs"
            ;;
        *)
            >&2 echo "The program '$cmd' is currently not installed. It is provided by"
            >&2 echo "several packages. You can install it by typing one of the following:"

	    echo -n "$attrs" | while read attr; do
                >&2 echo "  nix-env -iA $toplevel.$attr"
	    done
            ;;
    esac

    exit 127 # command not found should always exit with 127
}

# for zsh...
# we just pass it to the bash handler above
# apparently they work identically
command_not_found_handler () {
    command_not_found_handle $@
}
