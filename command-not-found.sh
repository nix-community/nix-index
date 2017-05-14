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
        1)
            # if only 1 package provides this,
            # then we can invoke it without asking the users
            # of course if they have opted in with one of 2
            # environment variables
            # they are based on the ones in command-not-found.sh
            #   NIX_AUTO_INSTALL : install the missing command into the
            #                      user’s environment
            #   NIX_AUTO_RUN     : run the command transparently inside of
            #                      nix shell
            # these will not return 127 if they worked correctly

            # QUESTION: will this mess up some scripts if a broken script
            #           fails to install?

            if ! [ -z "$NIX_AUTO_INSTALL" ]; then
                >&2 cat <<EOF
The program '$cmd' is currently not installed. It is provided by
the package '$toplevel.$attrs', which I will now install for you.
EOF
                nix-env -iA $toplevel.$attrs
                $@ # TODO: handle pipes correctly if AUTO_RUN/INSTALL is possible
                return $?
            elif ! [ -z "$NIX_AUTO_RUN" ]; then
                # how nix-shell handles commands is weird
                # $(echo $@) is need to handle this
                nix-shell -p $attrs --run "$(echo $@)"
                return $?
            else
                >&2 cat <<EOF
The program '$cmd' is currently not installed. You can install it
by typing:
  nix-env -iA $toplevel.$attrs
EOF
            fi
            ;;
        *)
            >&2 cat <<EOF
The program '$cmd' is currently not installed. It is provided by
several packages. You can install it by typing one of the following:
EOF

            while read attr; do
                >&2 echo "  nix-env -iA $toplevel.$attr"
            done <<< "$attrs"
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
