#!/usr/bin/env nu
# Line search: streams every line of a project into sieb and opens the
# pick in helix, at that line.
#
#     cd ~/Projects/rust/sieb; examples/stream/grep.nu
#     examples/stream/grep.nu ~/Projects/rust/nushell
#
# That is easily hundreds of thousands of rows. ripgrep skips what git
# ignores, and sieb matches the file name too, so `win key` finds the
# key handling in window.rs.

def main [
    dir?: path                    # project to search [default: here]
    --config (-c): path           # sieb config, such as a theme
    --sieb: string = "sieb"       # the sieb to run
    --print (-p)                  # print file:line instead of opening helix
] {
    let dir = $dir | default (pwd) | path expand
    let config = if $config == null { [] } else { [--config $config] }
    cd $dir

    # `.` rather than an empty pattern leaves out blank lines. Very long
    # lines, minified files mostly, are cut so they stay searchable.
    let picked = (
        ^rg --line-number --no-heading --color never --trim --max-columns 300 --max-columns-preview .
        | ^$sieb --prompt grep ...$config
        | complete
    )
    # sieb exits 1 on Esc, which is not an error here.
    if $picked.exit_code != 0 or ($picked.stdout | is-empty) {
        return
    }
    # path:line:text. The text may hold colons too, so parse from the left.
    let hit = $picked.stdout | parse --regex '^(?<file>[^:]+):(?<line>\d+):' | get 0?
    if $hit == null {
        return
    }
    let target = $"($dir | path join $hit.file):($hit.line)"
    if $print {
        print $target
    } else {
        # helix takes over this terminal, which is where the search started.
        ^hx $target
    }
}
