#!/usr/bin/env nu
# Power menu for `sieb --script contrib/power.nu`.
#
# Shows how a script drives sieb: called with no argument it prints the
# first menu, called with the picked entry it either prints a follow-up
# menu (the confirmation) or acts and prints nothing, which closes sieb.

const ACTIONS = {
    Lock: [loginctl lock-session]
    Suspend: [systemctl suspend]
    Reboot: [systemctl reboot]
    Shutdown: [systemctl poweroff]
}

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

def actions-menu [] {
    menu-option prompt power
    # Typed text that matches nothing should not reach the script.
    menu-option no-custom "true"
    $ACTIONS | columns | each {|action| row $action } | ignore
}

def main [choice?: string] {
    match $choice {
        null => { actions-menu }
        "Yes" => {
            # The confirmed action rode along in the row's info.
            let command = $ACTIONS | get $env.ROFI_INFO
            run-external ...$command
        }
        "No" => { actions-menu }
        $action => {
            menu-option prompt $"($action | str lowercase)?"
            menu-option no-custom "true"
            row "No"
            row "Yes" info $action
        }
    }
}
