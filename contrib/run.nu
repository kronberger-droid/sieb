#!/usr/bin/env nu
# Command runner, after rofi's run mode:
#
#     sieb --config contrib/launcher.toml --script contrib/run.nu
#
# Lists the programs on PATH and runs the pick. Typed text runs as a
# shell command, so arguments work too.

def programs [] {
    $env.PATH
    | where {|dir| $dir | path exists }
    | each {|dir| ls --short-names $dir | where type in [file symlink] | get name }
    | flatten
    | uniq
    | sort
}

# Detached from sieb: its own session, and stdout closed so sieb sees the
# script finish and closes right away.
def launch [command: string] {
    cd $env.HOME
    ^setsid -f sh -c $command o+e> /dev/null
}

def main [choice?: string] {
    match ($env.ROFI_RETV? | default "0") {
        "0" => { programs | each { print $in } | ignore }
        _ => { launch $choice }
    }
}
