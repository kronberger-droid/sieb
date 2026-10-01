#!/usr/bin/env nu
# Power menu, after rofi's powermenu.sh:
#
#     sieb --config contrib/power.toml --script contrib/power.nu
#
# Shows how a script drives sieb: called with no argument it prints the
# first menu, called with the picked entry it either prints a follow-up
# menu (the confirmation) or acts and prints nothing, which closes sieb.

# Glyphs from Nerd Fonts, keyed by the action they stand for.
const ACTIONS = [
    [action glyph];
    [lock "\u{f033e}"]
    [suspend "\u{f0904}"]
    [logout "\u{f0343}"]
    [hibernate "\u{f04b2}"]
    [reboot "\u{f0709}"]
    [shutdown "\u{f0425}"]
]

# rofi's in-band options: `\0key\x1fvalue` lines set menu options, and
# `text\0key\x1fvalue` attaches options to a row.
def menu-option [key: string, value: string] {
    print $"\u{0}($key)\u{1f}($value)"
}

def row [text: string, ...options: string] {
    if ($options | is-empty) {
        print $text
    } else {
        print $"($text)\u{0}($options | str join "\u{1f}")"
    }
}

def uptime [] {
    let seconds = open /proc/uptime | split row " " | first | into float | into int
    let h = $seconds // 3600
    let m = $seconds mod 3600 // 60
    let s = $seconds mod 60
    $"($h)h ($m)m ($s)s"
}

def actions-menu [] {
    menu-option prompt (sys host | get hostname)
    menu-option message $"Uptime: (uptime)"
    menu-option no-custom "true"
    for it in $ACTIONS {
        row $"($it.glyph) ($it.action)" info $it.action
    }
}

def confirm-menu [action: string] {
    menu-option prompt Confirmation
    menu-option message "Are you Sure?"
    menu-option no-custom "true"
    row "\u{f012c} yes" info $action
    row "\u{f0156} no"
}

# Detached and with its output closed, so sieb sees the script finish.
def spawn [...argv: string] {
    ^setsid -f ...$argv o+e> /dev/null
}

def act [action: string] {
    match $action {
        lock => { spawn veila lock }
        suspend => { systemctl suspend }
        hibernate => { systemctl hibernate }
        reboot => { systemctl reboot }
        shutdown => { systemctl poweroff }
        logout => {
            if ($env.NIRI_SOCKET? | is-not-empty) {
                niri msg action quit
            } else if ($env.SWAYSOCK? | is-not-empty) {
                swaymsg exit
            }
        }
    }
}

def main [choice?: string] {
    let info = $env.ROFI_INFO? | default ""
    match [$choice $info] {
        [null, _] => { actions-menu }
        # Locking is undone by unlocking, so it needs no confirmation.
        [_, lock] => { act lock }
        [$picked, $action] if ($picked | str ends-with " yes") => { act $action }
        # "no" closes the menu, like rofi's.
        [$picked, _] if ($picked | str ends-with " no") => {}
        [_, $action] => { confirm-menu $action }
    }
}
