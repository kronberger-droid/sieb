#!/usr/bin/env nu
# App launcher for `sieb --script contrib/drun.nu`.
#
# Lists the desktop entries in the XDG data dirs and launches the pick.
# Typed text that matches no app runs as a shell command instead.

# Data dirs in precedence order: an entry in an earlier dir hides one with
# the same id in a later dir, so user overrides win.
def data-dirs [] {
    let home = $env.XDG_DATA_HOME? | default ($env.HOME | path join .local/share)
    let system = $env.XDG_DATA_DIRS? | default "/usr/local/share:/usr/share" | split row ":"
    [$home ...$system] | where { is-not-empty }
}

# The `[Desktop Entry]` group as a record. Localized keys like `Name[de]`
# are skipped; the plain key is the fallback every entry has.
def parse-entry [path: string] {
    open --raw $path
    | lines
    | skip until { $in == "[Desktop Entry]" }
    | skip 1
    | take until { str starts-with "[" }
    | parse "{key}={value}"
    | where key !~ '\['
    | reduce --fold {} {|it, acc| $acc | upsert ($it.key | str trim) ($it.value | str trim) }
}

def visible [entry: record] {
    let desktops = $env.XDG_CURRENT_DESKTOP? | default "" | split row ":"
    let only = $entry.OnlyShowIn? | default "" | split row ";" | where { is-not-empty }
    let not = $entry.NotShowIn? | default "" | split row ";" | where { is-not-empty }
    (
        ($entry.Type? == "Application")
        and ($entry.NoDisplay? != "true")
        and ($entry.Hidden? != "true")
        and ($entry.Exec? | is-not-empty)
        and (($only | is-empty) or ($only | any { $in in $desktops }))
        and not ($not | any { $in in $desktops })
    )
}

def entries [] {
    data-dirs
    | each {|dir| $dir | path join applications }
    | where { path exists }
    | each {|dir|
        glob $"($dir)/**/*.desktop" | each {|path|
            # The desktop file id: the path below applications/, with
            # slashes as dashes.
            {id: ($path | path relative-to $dir | str replace --all "/" "-"), path: $path}
        }
    }
    | flatten
    | uniq-by id
    | each {|it| parse-entry $it.path | insert path $it.path }
    | where {|entry| visible $entry }
    | sort-by Name
}

def menu [] {
    print "\u{0}prompt\u{1f}\u{f002}"
    entries | each {|entry|
        # Matched but not shown: lets "browser" find Firefox.
        let meta = [
            ($entry.GenericName? | default "")
            ($entry.Keywords? | default "" | str replace --all ";" " ")
            (program $entry)
        ] | str join " "
        print $"($entry.Name)\u{0}info\u{1f}($entry.path)\u{1f}meta\u{1f}($meta)"
    } | ignore
}

# The program's name, past any `env VAR=value` prefix (wine entries).
def program [entry: record] {
    $entry.Exec
    | split row " "
    | skip while {|word| $word == "env" or ($word =~ '^[A-Za-z_]+=') }
    | first
    | default ""
    | str trim --char '"'
    | path basename
}

# `Exec` minus the field codes, which only make sense when files or URLs
# are being opened with the app.
def command [entry: record] {
    $entry.Exec
    | str replace --all --regex '%[fFuUdDnNvmick]' ""
    | str replace --all "%%" "%"
    | str trim
}

# The words that run a command in a terminal. Terminals disagree on this:
# foot and kitty take the command as plain arguments, wezterm wants
# `start --`, most others `-e`.
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

# The argument list that runs `entry`, wrapped in a terminal if it asks.
def argv [entry: record] {
    let shell = [sh -c (command $entry)]
    if $entry.Terminal? == "true" { [...(terminal) ...$shell] } else { $shell }
}

# Detached from sieb: its own session, and stdout closed so sieb sees the
# script finish and closes right away.
def launch [argv: list<string>, dir?: string] {
    # A stale `Path=` should not stop the app from starting.
    let dir = if ($dir | is-not-empty) and ($dir | path exists) { $dir } else { $env.HOME }
    cd $dir
    ^setsid -f ...$argv o+e> /dev/null
}

def main [choice?: string] {
    match ($env.ROFI_RETV? | default "0") {
        "0" => { menu }
        "1" => {
            let entry = parse-entry $env.ROFI_INFO
            launch (argv $entry) $entry.Path?
        }
        # Typed text that is not an app: run it.
        "2" => { launch [sh -c $choice] }
    }
}
