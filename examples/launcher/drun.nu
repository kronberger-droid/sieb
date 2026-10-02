#!/usr/bin/env nu
# App launcher for `sieb --script examples/launcher/drun.nu`.
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
    | update key { str trim }
    | update value { str trim }
    # The first of a repeated key wins, and the pairs become one record
    # in one go rather than one rebuild per key.
    | uniq-by key
    | transpose --header-row --as-record
}

def visible [entry: record, desktops: list<string>] {
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
    let desktops = $env.XDG_CURRENT_DESKTOP? | default "" | split row ":"
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
    # Files are independent, so parse them in parallel; sorting follows.
    | par-each {|it| parse-entry $it.path | insert path $it.path }
    | where {|entry| visible $entry $desktops }
    | sort-by Name
}

# Launch counts by desktop file, so the apps used most come first, like
# rofi's drun history.
def history-file [] {
    $env.XDG_CACHE_HOME? | default ($env.HOME | path join .cache) | path join sieb drun-history.nuon
}

def history [] {
    let file = history-file
    if ($file | path exists) { try { open $file } catch { {} } } else { {} }
}

def remember [path: string] {
    let file = history-file
    mkdir ($file | path dirname)
    let counts = history
    $counts | upsert $path (($counts | get --optional $path | default 0) + 1) | save --force $file
}

# Text that markup would read as tags or entities.
def escape [] {
    str replace --all "&" "&amp;" | str replace --all "<" "&lt;" | str replace --all ">" "&gt;"
}

def menu [] {
    print "\u{0}markup-rows\u{1f}true"
    let counts = history
    entries
    # Ascending on the negated count, since the sort is stable and apps
    # used equally often then keep their alphabetical order.
    | insert rank {|entry| 0 - ($counts | get --optional $entry.path | default 0) }
    | sort-by rank
    | each {|entry|
        # rofi's drun-display-format: `{name} [<span weight='light'
        # size='small'><i>({generic})</i></span>]`.
        let name = $entry.Name | escape
        let text = match ($entry.GenericName? | default "") {
            "" => $name
            $generic => $"($name) <span weight='light' size='small'><i>\(($generic | escape)\)</i></span>"
        }
        # Matched but not shown.
        let meta = [
            ($entry.Keywords? | default "" | str replace --all ";" " ")
            (program $entry)
        ] | str join " "
        print $"($text)\u{0}info\u{1f}($entry.path)\u{1f}meta\u{1f}($meta)"
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
            remember $env.ROFI_INFO
            launch (argv $entry) $entry.Path?
        }
        # Typed text that is not an app: run it.
        "2" => { launch [sh -c $choice] }
    }
}
