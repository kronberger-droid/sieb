#!/usr/bin/env nu
# Bitwarden through rbw, after rofi-rbw:
#
#     rbw unlock; sieb --script examples/rbw/rbw.nu
#
# Unlock first. A locked vault makes rbw ask for the master password, and
# that prompt would open while sieb holds the keyboard. With sieb-pinentry
# as rbw's pinentry, the unlock is a sieb panel of its own.
#
# Pick an entry, then what to do with it: type it into the window sieb
# was opened over, or copy it. Typing waits a moment for focus to return,
# and copies clear themselves. Needs rbw, wtype and wl-clipboard.
#
# The secrets never pass through this script. Each action is a small sh
# program that asks rbw itself and pipes the answer straight into wtype or
# wl-copy, so no password lands in nu's memory or in anyone's argv. Only
# the entry's id, which is no secret, is passed along.

# Seconds for focus to return to the window sieb was opened over.
const TYPE_DELAY = 1
# Milliseconds between typed keys. Some apps drop keys typed faster.
const KEY_DELAY = 12
# Seconds before a copied secret is cleared, if it is still the clipboard.
const CLEAR_AFTER = 30

const ACTIONS = [
    [action glyph label];
    [autotype "\u{f11c}" "type username, Tab, password"]
    [password "\u{f084}" "type password"]
    [username "\u{f007}" "type username"]
    [copy-password "\u{f0c5}" "copy password"]
    [copy-username "\u{f0c5}" "copy username"]
    [copy-totp "\u{f017}" "copy TOTP code"]
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

# Entry names are user text inside markup.
def escape []: string -> string {
    str replace --all "&" "&amp;" | str replace --all "<" "&lt;" | str replace --all ">" "&gt;"
}

def entries []: nothing -> table {
    ^rbw list --fields id,name,user,folder
    | lines
    | split column "\t" id name user folder
}

def entries-menu [] {
    menu-option prompt rbw
    menu-option no-custom "true"
    if (^rbw unlocked | complete).exit_code != 0 {
        menu-option message "The vault is locked: run rbw unlock first"
        row "locked" nonselectable "true"
        return
    }
    menu-option markup-rows "true"
    for it in (entries | sort-by folder name) {
        let folder = if ($it.folder | is-empty) { "" } else { $"($it.folder | escape)/" }
        let user = $"<span alpha='55%'>($it.user | escape)</span>"
        row $"($folder)($it.name | escape)  ($user)" info $it.id
    }
}

def actions-menu [id: string] {
    let name = entries | where id == $id | get name.0? | default "entry"
    menu-option prompt rbw
    menu-option message $name
    menu-option no-custom "true"
    # Where the next call finds the entry, since its rows carry the action.
    menu-option data $id
    for it in $ACTIONS {
        row $"($it.glyph) ($it.label)" info $it.action
    }
}

# The sh side, run detached so sieb closes and focus goes back. $1 is the
# entry id. `$(...)` drops rbw's trailing newline, which wtype would type
# as Enter, and printf is a shell builtin, so the secret is never an
# argument of a process.
const SH_PRELUDE = '
id=$1 delay=$2 keys=$3 clear=$4
type_out() { printf %s "$1" | wtype -d "$keys" -; }
copy() {
    printf %s "$1" | wl-copy --sensitive
    notify-send -a rbw "$2" "Clears in ${clear}s"
    sleep "$clear"
    [ "$(wl-paste -n 2>/dev/null)" = "$1" ] && wl-copy --clear
}
'

const SH_ACTIONS = {
    autotype: '
u=$(rbw get --field username "$id") && p=$(rbw get "$id") || exit 1
sleep "$delay"
type_out "$u"; wtype -k Tab; type_out "$p"
# As rofi-rbw does: a TOTP is the next thing a login asks for.
c=$(rbw code "$id" 2>/dev/null) && copy "$c" "TOTP code copied"
'
    password: 'p=$(rbw get "$id") || exit 1; sleep "$delay"; type_out "$p"'
    username: 'u=$(rbw get --field username "$id") || exit 1; sleep "$delay"; type_out "$u"'
    copy-password: 'p=$(rbw get "$id") || exit 1; copy "$p" "Password copied"'
    copy-username: 'u=$(rbw get --field username "$id") || exit 1; copy "$u" "Username copied"'
    copy-totp: '
c=$(rbw code "$id") || { notify-send -a rbw "No TOTP for this entry"; exit 1; }
copy "$c" "TOTP code copied"
'
}

def act [action: string, id: string] {
    let program = $SH_PRELUDE + ($SH_ACTIONS | get $action)
    # Detached and with its output closed, so sieb sees the script finish.
    (^setsid -f sh -c $program rbw $id ($TYPE_DELAY | into string)
        ($KEY_DELAY | into string) ($CLEAR_AFTER | into string)) o+e> /dev/null
}

def main [choice?: string] {
    let info = $env.ROFI_INFO? | default ""
    let data = $env.ROFI_DATA? | default ""
    match [$choice $data] {
        [null, _] => { entries-menu }
        [_, ""] if ($info | is-not-empty) => { actions-menu $info }
        [_, $id] if ($info in ($ACTIONS | get action)) => { act $info $id }
        _ => {}
    }
}
