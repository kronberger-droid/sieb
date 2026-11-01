#!/usr/bin/env nu
# Bitwarden through rbw, after rofi-rbw:
#
#     rbw unlock; sieb --script examples/rbw/rbw.nu
#
# Unlock first. A locked vault makes rbw ask for the master password, and
# that prompt would open while sieb holds the keyboard. With sieb-pinentry
# as rbw's pinentry, the unlock is a sieb panel of its own. The menus only
# read what rbw already has unlocked, and the actions run once sieb has
# closed, so the only call that could prompt is the list, as sieb opens.
# Should the vault lock in that moment, sieb-pinentry shows that it lacks
# the keyboard instead of taking a password typed into the picker.
#
# Enter on a login types its username, Tab and password into the window
# sieb was opened over. Ctrl+Enter (ROFI_RETV 10) lists the entry's fields
# instead, and so does Enter on a note, card, identity or SSH key. There,
# Enter queues a field and goes back to the entries, so a form can be
# filled from the username of one entry and the password of another.
# Ctrl+Enter types or copies that one field right away. Once something is
# queued, a row on top of the entries types the queue in order, Tab
# between the fields.
#
# Notes are shown in full under the fields. They can be copied, or opened
# in $EDITOR through `rbw edit`, which has a login's password on the first
# line and its notes below it. rbw edits logins and notes only.
#
# sieb holds the keyboard while it is open, so typing waits until it has
# closed and focus is back, and copies clear themselves. Needs rbw, wtype,
# wl-clipboard, notify-send, and a terminal for `rbw edit`.
#
# The secrets never pass through this script. The field list comes from
# `rbw get --list-fields`, which prints names only, and the only values
# read for display are those in SHOWN. Each action is a small sh program
# that asks rbw itself and pipes the answer straight into wtype or
# wl-copy, so no password lands in nu's memory or in anyone's argv. Only
# entry ids and field names are passed along, and the queue rides between
# calls in sieb's `data`.

# Seconds for focus to return to the window sieb was opened over.
const TYPE_DELAY = 1
# Milliseconds between typed keys. Some apps drop keys typed faster.
const KEY_DELAY = 12
# Seconds before a copied secret is cleared, if it is still the clipboard.
const CLEAR_AFTER = 30

# Fields whose values are shown next to their names. The rest show their
# name alone, since a password, TOTP seed, card number or a hidden custom
# field must not be read here.
const SHOWN = ["username" "uris" "email" "notes"]

# What `rbw edit` accepts, by the type `rbw list --raw` reports.
const EDITABLE = ["Login" "Note"]

const GLYPHS = {
    username: "\u{f007}"
    password: "\u{f084}"
    totp: "\u{f017}"
    uris: "\u{f0c1}"
    notes: "\u{f249}"
}
const GLYPH_OTHER = "\u{f02b}"

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

# Vault text for the prompt and message, which sieb draws as plain text:
# control characters out.
def plain []: any -> string {
    default "" | str replace --all --regex '\p{Cc}' " "
}

# Vault text in a row, where markup-rows is on: markup escaped too.
def shown []: any -> string {
    plain | str replace --all "&" "&amp;" | str replace --all "<" "&lt;" | str replace --all ">" "&gt;"
}

# A record this script put in `info` or `data` earlier, or an empty one
# for anything that does not parse as one.
def decode []: any -> record {
    let value = try { default "" | from json } catch { null }
    if ($value | describe) starts-with "record" { $value } else { {} }
}

def encode []: record -> string {
    to json --raw
}

# An entry as `info` or `data` carries it, or null unless its id is a real
# one.
def valid-entry []: any -> any {
    let entry = $in
    if ($entry | describe) starts-with "record" and ($entry.id? | default "") =~ $UUID {
        {id: $entry.id, name: ($entry.name? | default ""), type: ($entry.type? | default "")}
    }
}

# A field name: one line, as `rbw get --list-fields` prints it.
def valid-field []: any -> any {
    let field = $in
    if ($field | describe) == "string" and ($field | is-not-empty) and not ($field =~ '\n') {
        $field
    }
}

def valid-queue []: any -> list {
    default []
    | where {|it| ($it | describe) starts-with "record" }
    | where {|it| ($it.id? | default "") =~ $UUID and ($it.field? | valid-field) != null }
    | each {|it| {id: $it.id, field: $it.field, name: ($it.name? | default "")} }
}

def queue-text [queue: list]: nothing -> string {
    $queue | each {|it| $"($it.field | plain)@($it.name | plain)" } | str join ", "
}

def menu-header [options: record] {
    print '{"sieb": 1}'
    emit ({prompt: "rbw", no-custom: true, markup-rows: true} | merge $options)
}

def entries-menu [queue: list] {
    menu-header {data: ({queue: $queue} | encode)}
    if (^rbw unlocked | complete).exit_code != 0 {
        emit {message: "The vault is locked: run rbw unlock first"}
        emit {text: "locked", selectable: false}
        return
    }
    let list = ^rbw list --raw | complete
    if $list.exit_code != 0 {
        emit {message: $"rbw list failed: ($list.stderr | str trim | plain)"}
        emit {text: "failed", selectable: false}
        return
    }
    if ($queue | is-empty) {
        emit {message: "Enter: type login    Ctrl+Enter: fields"}
    } else {
        emit {message: $"queued: (queue-text $queue)"}
        emit {text: "\u{f11c} type the queue", info: ({do: "type-queue"} | encode)}
        emit {text: "\u{f1f8} clear the queue", info: ({do: "clear"} | encode)}
    }
    let entries = $list.stdout | from json | where {|it| $it.id =~ $UUID }
    for it in ($entries | sort-by {|e| $e.folder | default ""} name) {
        let folder = if ($it.folder | is-empty) { "" } else { $"($it.folder | shown)/" }
        let detail = if $it.type == "Login" { $it.user } else { $it.type }
        let entry = {do: "entry", id: $it.id, name: $it.name, type: $it.type}
        emit {
            text: $"($folder)($it.name | shown)  <span alpha='55%'>($detail | shown)</span>"
            info: ($entry | encode)
        }
    }
}

# A field's value for display, only ever one in SHOWN.
def read-shown [id: string, field: string]: nothing -> string {
    if not ($field in $SHOWN) { return "" }
    let got = ^rbw get --field $field $id | complete
    if $got.exit_code == 0 { $got.stdout | str trim --right } else { "" }
}

def fields-menu [entry: record, queue: list] {
    let hint = "Enter: queue    Ctrl+Enter: type or copy"
    let message = if ($queue | is-empty) { $hint } else { $"($hint)    queued: (queue-text $queue)" }
    menu-header {
        prompt: ($entry.name | plain)
        message: $message
        data: ({queue: $queue, entry: $entry} | encode)
    }
    emit {text: "\u{f060} back", info: ({do: "back"} | encode)}
    let listed = ^rbw get --list-fields $entry.id | complete
    if $listed.exit_code != 0 {
        emit {text: $"rbw failed: ($listed.stderr | str trim | shown)", selectable: false}
        return
    }
    let fields = $listed.stdout | lines | where {|f| $f | is-not-empty }
    for field in ($fields | where {|f| $f != "notes" }) {
        let glyph = $GLYPHS | get --optional $field | default $GLYPH_OTHER
        let value = read-shown $entry.id $field | lines | str join ", "
        let value = if ($value | is-empty) { "" } else { $"  <span alpha='55%'>($value | shown)</span>" }
        emit {text: $"($glyph) ($field | shown)($value)", info: ({do: "field", field: $field} | encode)}
    }
    if $entry.type in $EDITABLE {
        let what = if $entry.type == "Login" { "password and notes" } else { "note" }
        emit {text: $"\u{f040} edit the ($what) in $EDITOR", info: ({do: "edit"} | encode)}
    }
    if "notes" in $fields {
        emit {text: $"($GLYPHS.notes) copy the notes", info: ({do: "copy", field: "notes"} | encode)}
        for line in (read-shown $entry.id "notes" | lines) {
            emit {text: $"<span alpha='70%'>($line | shown)</span>", selectable: false}
        }
    }
}

# Ctrl+Enter on a field: what to do with it on its own.
def field-menu [entry: record, field: string, queue: list] {
    menu-header {
        prompt: ($entry.name | plain)
        message: ($field | plain)
        data: ({queue: $queue, entry: $entry} | encode)
    }
    emit {text: "\u{f11c} type it", info: ({do: "type", field: $field} | encode)}
    emit {text: "\u{f0c5} copy it", info: ({do: "copy", field: $field} | encode)}
    emit {text: "\u{f060} back", info: ({do: "fields"} | encode)}
}

# The sh side, run detached so sieb closes and focus goes back. $1 to $3
# are the delays, the rest is the action's. `$(...)` drops rbw's trailing
# newline, which wtype would type as Enter, and printf is a shell builtin,
# so the secret is never an argument of a process. Every way out short of
# success says why in a notification, since nothing else is watching.
const SH_PRELUDE = r#'
delay=$1 keys=$2 clear=$3
shift 3
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
    # $1 the entry id.
    autotype: '
get get --field username "$1"; u=$v
get get "$1"; p=$v
typeable "$u" username; typeable "$p" password
sleep "$delay"
type_out "$u"; wtype -k Tab; type_out "$p"
# As rofi-rbw does: a TOTP is the next thing a login asks for.
c=$(rbw code "$1" 2>/dev/null) && [ -n "$c" ] && copy "$c" "TOTP code"
'
    # $1 the entry id, $2 the field.
    type: 'get get --field "$2" "$1"; typeable "$v" "$2"; sleep "$delay"; type_out "$v"'
    copy: 'get get --field "$2" "$1"; copy "$v" "$2"'
    # Pairs of entry id and field. Every value is read and checked before
    # the first key goes out, so a missing one types nothing at all. The
    # eval only ever names the variables, the values stay in them.
    queue: '
n=0
while [ $# -ge 2 ]; do
    get get --field "$2" "$1"; typeable "$v" "$2"
    n=$((n + 1)); eval "q$n=\$v"
    shift 2
done
sleep "$delay"
i=1
while [ "$i" -le "$n" ]; do
    [ "$i" -gt 1 ] && wtype -k Tab
    eval "type_out \"\$q$i\""
    i=$((i + 1))
done
'
}

def act [action: string, ...args: string] {
    let program = $SH_PRELUDE + ($SH_ACTIONS | get $action)
    let delays = [$TYPE_DELAY $KEY_DELAY $CLEAR_AFTER] | each { into string }
    # Detached and with its output closed, so sieb sees the script finish.
    ^setsid -f sh -c $program rbw ...$delays ...$args o+e> /dev/null
}

# The words that run a command in a terminal, as in launcher/drun.nu.
def terminal [] {
    let term = if ($env.TERMINAL? | is-not-empty) {
        $env.TERMINAL
    } else {
        [xdg-terminal-exec foot kitty alacritty wezterm ghostty rio]
        | where {|term| which $term | is-not-empty }
        | get 0?
        | default xterm
    }
    match ($term | path basename) {
        "xdg-terminal-exec" | "foot" | "kitty" => [$term]
        "wezterm" => [$term start --]
        _ => [$term -e]
    }
}

# `rbw edit` in a terminal of its own, which runs $EDITOR on the entry.
def edit [id: string] {
    ^setsid -f ...(terminal) rbw edit $id o+e> /dev/null
}

def main [choice?: string] {
    if $choice == null { return (entries-menu []) }
    let data = $env.ROFI_DATA? | decode
    let queue = $data.queue? | valid-queue
    let entry = $data.entry? | valid-entry
    let pick = $env.ROFI_INFO? | decode
    let field = $pick.field? | valid-field
    let more = ($env.ROFI_RETV? | default "") == "10"
    match ($pick.do? | default "") {
        "entry" => {
            let picked = $pick | valid-entry
            if $picked == null { return }
            if $picked.type == "Login" and not $more {
                act autotype $picked.id
            } else {
                fields-menu $picked $queue
            }
        }
        "type-queue" if ($queue | is-not-empty) => {
            act queue ...($queue | each {|it| [$it.id $it.field] } | flatten)
        }
        "clear" => { entries-menu [] }
        "back" => { entries-menu $queue }
        "fields" if $entry != null => { fields-menu $entry $queue }
        "field" if $entry != null and $field != null => {
            if $more {
                field-menu $entry $field $queue
            } else {
                entries-menu ($queue | append {id: $entry.id, field: $field, name: $entry.name})
            }
        }
        "type" if $entry != null and $field != null => { act type $entry.id $field }
        "copy" if $entry != null and $field != null => { act copy $entry.id $field }
        "edit" if $entry != null and $entry.type in $EDITABLE => { edit $entry.id }
        _ => {}
    }
}
