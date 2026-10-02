#!/usr/bin/env nu
# File browser, after rofi's filebrowser mode:
#
#     sieb --config examples/launcher/launcher.toml --script examples/launcher/files.nu
#
# Starts in the home directory. Picking a directory lists it, picking a
# file opens it with xdg-open. Typed text is taken as a path, relative to
# the directory on screen.

def list [dir: string] {
    # Remembered for the next call, which gets it back as ROFI_DATA.
    print $"\u{0}data\u{1f}($dir)"
    print $"\u{0}message\u{1f}($dir | str replace $env.HOME '~')"
    if $dir != "/" {
        print $"..\u{0}info\u{1f}($dir | path dirname)"
    }
    # Directories first, each group by name, hidden files last.
    ls --all --short-names $dir
    | insert dir {|it| $it.type == dir }
    | insert hidden {|it| $it.name | str starts-with "." }
    | sort-by hidden { not $in.dir } name
    | each {|it|
        let suffix = if $it.dir { "/" } else { "" }
        print $"($it.name)($suffix)\u{0}info\u{1f}($dir | path join $it.name)"
    }
    | ignore
}

# Detached and with its output closed, so sieb sees the script finish.
def open-file [path: string] {
    ^setsid -f xdg-open $path o+e> /dev/null
}

def visit [path: string] {
    match ($path | path type) {
        dir => { list ($path | path expand) }
        file | symlink => { open-file $path }
        _ => { list ($path | path dirname) }
    }
}

def main [choice?: string] {
    let here = $env.ROFI_DATA? | default $env.HOME
    match ($env.ROFI_RETV? | default "0") {
        "0" => { list $here }
        "1" => { visit $env.ROFI_INFO }
        # Absolute and home paths as they are, the rest under the directory
        # on screen, not sieb's working directory.
        _ => {
            let target = if ($choice | str starts-with "/") or ($choice | str starts-with "~") {
                $choice | path expand
            } else {
                $here | path join $choice | path expand
            }
            visit $target
        }
    }
}
