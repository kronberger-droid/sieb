#!/usr/bin/env nu
# File finder: streams `fd` into sieb and opens the pick with xdg-open.
#
#     examples/stream/find.nu            # everything under ~
#     examples/stream/find.nu ~/Projects
#
# sieb is usable from the first file fd finds, and the counter climbs
# while fd is still walking, so a large home costs no waiting.

def main [
    dir?: path                    # where to search [default: ~]
    --config (-c): path           # sieb config, such as a theme
    --sieb: string = "sieb"       # the sieb to run
    --print (-p)                  # print the path instead of opening it
] {
    let dir = $dir | default $env.HOME | path expand
    let config = if $config == null { [] } else { [--config $config] }

    # Relative paths read better in the list. Hidden files are in, git
    # internals and anything gitignored are not.
    let picked = (
        ^fd --type file --hidden --exclude .git --base-directory $dir
        | ^$sieb --prompt find ...$config
        | complete
    )
    # sieb exits 1 on Esc, which is not an error here.
    if $picked.exit_code != 0 or ($picked.stdout | is-empty) {
        return
    }
    let file = $dir | path join ($picked.stdout | str trim --right --char "\n")
    if $print {
        print $file
    } else {
        # Detached, so the terminal is free again right away.
        ^setsid -f xdg-open $file o+e> /dev/null
    }
}
