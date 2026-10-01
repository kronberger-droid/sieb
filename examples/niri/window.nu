#!/usr/bin/env nu
# Window switcher, after rofi's window mode. Compositor: niri only, since
# it lists and focuses windows through `niri msg`.
#
#     sieb --config examples/launcher/launcher.toml --script examples/niri/window.nu
#
# Lists the open windows as `workspace · app · title`, most recently
# focused first, and focuses the pick.

def main [choice?: string] {
    if ($env.NIRI_SOCKET? | is-empty) {
        print "\u{0}message\u{1f}window.nu needs niri"
        print "\u{0}no-custom\u{1f}true"
        return
    }
    match ($env.ROFI_RETV? | default "0") {
        "0" => {
            let workspaces = niri msg --json workspaces | from json
            print "\u{0}no-custom\u{1f}true"
            niri msg --json windows
            | from json
            | sort-by --reverse { $in.focus_timestamp?.secs? | default 0 }
            | each {|window|
                let workspace = $workspaces
                | where id == $window.workspace_id
                | get 0?
                | default {idx: "?"}
                let name = $workspace.name? | default $workspace.idx
                print $"($name) · ($window.app_id? | default '') · ($window.title? | default '')\u{0}info\u{1f}($window.id)"
            }
            | ignore
        }
        "1" => { niri msg action focus-window --id ($env.ROFI_INFO | into int) }
    }
}
