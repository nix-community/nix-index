#!/bin/sh

# for bash 4
# this will be called when a command is entered
# but not found in the user’s path + environment
command_not_found_handle () {
    toplevel=nixos # TODO: detect this somehow
    cmd=$1 # TODO: differentiate between paths and commands
    attrs=$(@out@/bin/nix-locate -1 --no-group --type x --top-level --regex "/bin/$cmd$")
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

            # need $(echo ...) to ensure attrs is split up by line correctly
            for attr in $(echo $attrs); do
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
