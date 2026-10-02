#!/usr/bin/env nu
# Bitwarden through rbw, after rofi-rbw:
#
#     rbw unlock; sieb --script examples/rbw/rbw.nu
#
# Unlock first. A locked vault makes rbw ask for the master password, and
# that prompt would open while sieb holds the keyboard. With sieb-pinentry
# as rbw's pinentry, the unlock is a sieb panel of its own. The actions
# menu needs nothing from rbw and the actions run once sieb has closed, so
# the only call that could prompt is the list, as sieb opens. Should the
# vault lock in that moment, sieb-pinentry shows that it lacks the keyboard
# instead of taking a password typed into the picker.
#
# Enter on a login types its username, Tab and password into the window
# sieb was opened over. Ctrl+Enter (ROFI_RETV 10) asks what else to do:
# type or copy one field, or copy the TOTP. Typing waits a moment for
# focus to return, and copies clear themselves. Needs rbw, wtype,
# wl-clipboard and notify-send.
#
# Only logins are listed: for a note or a card, `rbw get` prints the note
# or the card number, which is nothing to type into a login form.
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

# What rbw calls an entry id. Anything else in `info` or `data` did not come
# from this script, and is never handed to `rbw get`, which would match it
# as a name or URI against the whole vault.
const UUID = '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'

# sieb's JSON protocol: after the `{"sieb": 1}` header every line is one
# JSON value, so vault text cannot reach sieb's own syntax the way a NUL or
# a newline in an entry name could with rofi's in-band options.
def emit [value: record] {
    print ($value | to json --raw)
}

# Vault text, shown: control characters out, markup escaped.
def shown []: any -> string {
    default ""
    | str replace --all --regex '\p{Cc}' " "
    | str replace --all "&" "&amp;" | str replace --all "<" "&lt;" | str replace --all ">" "&gt;"
}

def entries-menu [] {
    print '{"sieb": 1}'
    emit {prompt: rbw, no-custom: true}
    if (^rbw unlocked | complete).exit_code != 0 {
        emit {message: "The vault is locked: run rbw unlock first"}
        emit {text: "locked", selectable: false}
        return
    }
    let list = ^rbw list --raw | complete
    if $list.exit_code != 0 {
        emit {message: $"rbw list failed: ($list.stderr | str trim)"}
        emit {text: "failed", selectable: false}
        return
    }
    emit {message: "Enter: type login    Ctrl+Enter: more", markup-rows: true}
    let logins = $list.stdout | from json | where type == "Login" and ($it.id =~ $UUID)
    for it in ($logins | sort-by {|e| $e.folder | default ""} name) {
        let folder = if ($it.folder | is-empty) { "" } else { $"($it.folder | shown)/" }
        let user = $"<span alpha='55%'>($it.user | shown)</span>"
        emit {text: $"($folder)($it.name | shown)  ($user)", info: $it.id}
    }
}

# `title` is the picked row as sieb showed it, so this menu asks rbw nothing.
def actions-menu [id: string, title: string] {
    print '{"sieb": 1}'
    emit {prompt: rbw, message: $title, no-custom: true, data: $id}
    for it in $ACTIONS {
        emit {text: $"($it.glyph) ($it.label)", info: $it.action}
    }
}

# The sh side, run detached so sieb closes and focus goes back. $1 is the
# entry id. `$(...)` drops rbw's trailing newline, which wtype would type
# as Enter, and printf is a shell builtin, so the secret is never an
# argument of a process. Every way out short of success says why in a
# notification, since nothing else is watching.
const SH_PRELUDE = r#'
id=$1 delay=$2 keys=$3 clear=$4
nl="
"
err=$(mktemp) || exit 1
trap 'rm -f "$err"' EXIT
fail() {
    notify-send -a rbw -u critical "$1" "$(printf %s "$2" | tr -d "<>&")"
    exit 1
}
# Sets $v to what rbw prints for its arguments, or fails with rbw stderr.
get() { v=$(rbw "$@" 2>"$err") || fail "rbw failed" "$(cat "$err")"; }
# A value to type: present, and on one line, since wtype types a newline
# as Enter and a terminal would run whatever came before it.
typeable() {
    [ -n "$1" ] || fail "Nothing to type" "The entry has no $2"
    case $1 in *"$nl"*) fail "Not typing the $2" "It spans several lines" ;; esac
}
type_out() { printf %s "$1" | wtype -d "$keys" - || fail "wtype failed" ""; }
copy() {
    [ -n "$1" ] || fail "Nothing to copy" "The entry has no $2"
    printf %s "$1" | wl-copy --sensitive || fail "wl-copy failed" ""
    notify-send -a rbw "Copied the $2" "Clears in ${clear}s"
    sleep "$clear"
    [ "$(wl-paste -n 2>/dev/null)" = "$1" ] && wl-copy --clear
}
'#

const SH_ACTIONS = {
    autotype: '
get get --field username "$id"; u=$v
get get "$id"; p=$v
typeable "$u" username; typeable "$p" password
sleep "$delay"
type_out "$u"; wtype -k Tab; type_out "$p"
# As rofi-rbw does: a TOTP is the next thing a login asks for.
c=$(rbw code "$id" 2>/dev/null) && [ -n "$c" ] && copy "$c" "TOTP code"
'
    password: 'get get "$id"; typeable "$v" password; sleep "$delay"; type_out "$v"'
    username: 'get get --field username "$id"; typeable "$v" username; sleep "$delay"; type_out "$v"'
    copy-password: 'get get "$id"; copy "$v" password'
    copy-username: 'get get --field username "$id"; copy "$v" username'
    copy-totp: '
c=$(rbw code "$id" 2>"$err") || fail "No TOTP" "$(cat "$err")"
copy "$c" "TOTP code"
'
}

def act [action: string, id: string] {
    if not ($id =~ $UUID) {
        ^notify-send -a rbw -u critical "Not an entry id" "Ignored the pick"
        return
    }
    let program = $SH_PRELUDE + ($SH_ACTIONS | get $action)
    # Detached and with its output closed, so sieb sees the script finish.
    (^setsid -f sh -c $program rbw $id ($TYPE_DELAY | into string)
        ($KEY_DELAY | into string) ($CLEAR_AFTER | into string)) o+e> /dev/null
}

def main [choice?: string] {
    let info = $env.ROFI_INFO? | default ""
    let data = $env.ROFI_DATA? | default ""
    let more = ($env.ROFI_RETV? | default "") == "10"
    match [$choice $data] {
        [null, _] => { entries-menu }
        [_, ""] if ($info =~ $UUID) and $more => { actions-menu $info $choice }
        [_, ""] if ($info =~ $UUID) => { act autotype $info }
        [_, $id] if ($info in ($ACTIONS | get action)) => { act $info $id }
        _ => {}
    }
}
