#!/bin/sh

# for bash 4
command_not_found_handle () { @out@/bin/command-not-found $@ }

# for zsh
command_not_found_handler () { @out@/bin/command-not-found $@ }
