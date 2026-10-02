//! The server side of the pinentry protocol, the Assuan dialect gpg-agent
//! and rbw speak to `pinentry-*` programs over stdin and stdout.
//!
//! Only what a password prompt needs: the `SET*` texts, `GETPIN`, `GETINFO`
//! and the housekeeping commands. Clients parse replies strictly (rbw
//! treats any line not starting `OK`, `D `, `S ` or `ERR ` as an error), so
//! nothing else is ever written.

use std::io::{self, BufRead, Write};

use zeroize::Zeroizing;

use crate::secret::Secret;

/// What a client has set up for the next `GETPIN`.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Prompt {
    pub title: Option<String>,
    pub description: Option<String>,
    pub prompt: Option<String>,
    /// Why the last attempt failed. Shown once, then cleared.
    pub error: Option<String>,
}

impl Prompt {
    /// The one line the message box has room for: the error when there is
    /// one, since that is the news, else the description.
    pub fn message(&self) -> Option<String> {
        let text = self.error.as_ref().or(self.description.as_ref())?;
        Some(text.split_whitespace().collect::<Vec<_>>().join(" "))
    }
}

/// How the window answered a `GETPIN`.
pub enum Answer {
    Pin(Secret),
    Cancel,
    /// The window could not be shown at all.
    Failed(String),
}

// gpg-error codes: source in the top byte, code below. The comments are the
// texts libgpg-error gives them.
/// GPG_ERR_SOURCE_PINENTRY, GPG_ERR_CANCELED. rbw matches this number.
const CANCELED: &str = "ERR 83886179 Operation cancelled <Pinentry>";
/// GPG_ERR_SOURCE_USER_1, GPG_ERR_ASS_UNKNOWN_CMD, as libassuan sends it.
const UNKNOWN_COMMAND: &str = "ERR 536871187 Unknown IPC command <User defined source 1>";
/// GPG_ERR_SOURCE_USER_1, GPG_ERR_ASS_PARAMETER.
const BAD_PARAMETER: &str = "ERR 536871192 IPC parameter error <User defined source 1>";
/// GPG_ERR_SOURCE_PINENTRY, GPG_ERR_GENERAL, followed by the reason.
const GENERAL: &str = "ERR 83886081";

/// Assuan lines are at most this long, newline included.
const LINE_MAX: usize = 1000;

/// Serves one client until it says `BYE` or closes stdin. `ask` puts up
/// the window for each `GETPIN`.
pub fn serve<R, W, F>(input: R, mut out: W, mut ask: F) -> io::Result<()>
where
    R: BufRead,
    W: Write,
    F: FnMut(&Prompt) -> Answer,
{
    let mut prompt = Prompt::default();
    reply(&mut out, "OK Pleased to meet you")?;
    for line in input.lines() {
        let line = line?;
        let (command, arg) = line.split_once(' ').unwrap_or((&line, ""));
        let text = || Some(decode(arg)).filter(|text| !text.is_empty());
        match command.to_ascii_uppercase().as_str() {
            // Blank lines and comments are allowed and get no reply.
            "" => continue,
            _ if command.starts_with('#') => continue,
            "SETTITLE" => prompt.title = text(),
            "SETDESC" => prompt.description = text(),
            "SETPROMPT" => prompt.prompt = text(),
            "SETERROR" => prompt.error = text(),
            // Button labels, quality bars, key ids, timeouts, repeat
            // prompts and flags like OPTION ttyname= mean nothing here.
            // Accepting them keeps clients that send them working.
            "OPTION" | "SETOK" | "SETCANCEL" | "SETNOTOK" | "SETKEYINFO" | "SETQUALITYBAR"
            | "SETQUALITYBAR_TT" | "SETTIMEOUT" | "SETREPEAT" | "SETREPEATERROR"
            | "SETREPEATOK" | "SETGENPIN" | "SETGENPIN_TT" | "NOP" => {}
            "RESET" => prompt = Prompt::default(),
            "GETINFO" => {
                let value = match arg.trim() {
                    "flavor" => "sieb".to_owned(),
                    "version" => env!("CARGO_PKG_VERSION").to_owned(),
                    "pid" => std::process::id().to_string(),
                    _ => {
                        reply(&mut out, BAD_PARAMETER)?;
                        continue;
                    }
                };
                reply(&mut out, &format!("D {value}"))?;
            }
            "GETPIN" => {
                let answer = ask(&prompt);
                // Like the reference pinentry: an error is for one attempt.
                prompt.error = None;
                match answer {
                    Answer::Pin(pin) => send_pin(&mut out, &pin)?,
                    Answer::Cancel => {
                        reply(&mut out, CANCELED)?;
                        continue;
                    }
                    Answer::Failed(reason) => {
                        reply(&mut out, &format!("{GENERAL} {}", one_line(&reason)))?;
                        continue;
                    }
                }
            }
            "BYE" => {
                reply(&mut out, "OK closing connection")?;
                return Ok(());
            }
            _ => {
                reply(&mut out, UNKNOWN_COMMAND)?;
                continue;
            }
        }
        reply(&mut out, "OK")?;
    }
    Ok(())
}

fn reply(out: &mut impl Write, line: &str) -> io::Result<()> {
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}

/// Writes the PIN as `D` lines, escaped as Assuan requires. An empty PIN
/// sends no `D` line at all, as the reference pinentry does.
///
/// The escaped copy is the one other place the PIN exists, so it lives in
/// a `Zeroizing` buffer and goes out with `write_all` on `out` itself:
/// callers hand in an unbuffered writer, so no `LineWriter` keeps a copy.
fn send_pin(out: &mut impl Write, pin: &Secret) -> io::Result<()> {
    let bytes = pin.as_bytes();
    let mut line = Zeroizing::new(Vec::with_capacity(LINE_MAX));
    for &byte in bytes {
        if line.is_empty() {
            line.extend_from_slice(b"D ");
        }
        match byte {
            b'%' => line.extend_from_slice(b"%25"),
            b'\r' => line.extend_from_slice(b"%0D"),
            b'\n' => line.extend_from_slice(b"%0A"),
            byte => line.push(byte),
        }
        // Room for one more escape and the newline, or start a new line.
        if line.len() + 3 + 1 > LINE_MAX {
            line.push(b'\n');
            out.write_all(&line)?;
            zeroize::Zeroize::zeroize(&mut *line);
        }
    }
    if !line.is_empty() {
        line.push(b'\n');
        out.write_all(&line)?;
    }
    out.flush()
}

/// Undoes Assuan's percent escaping in a command's argument. Anything that
/// is not a valid escape stays as it is.
fn decode(arg: &str) -> String {
    let bytes = arg.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(h), Some(l)) = (
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// An error reason squeezed onto the reply line.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(text: &str) -> Secret {
        let mut secret = Secret::new();
        assert!(text.is_empty() || secret.insert(text));
        secret
    }

    /// Runs `input` through the server, answering every GETPIN with the
    /// next of `answers`, and returns what it wrote and what it was asked.
    fn session(input: &str, answers: Vec<Answer>) -> (String, Vec<Prompt>) {
        let mut out = Vec::new();
        let mut answers = answers.into_iter();
        let mut asked = Vec::new();
        serve(input.as_bytes(), &mut out, |prompt| {
            asked.push(prompt.clone());
            answers.next().expect("an answer for every GETPIN")
        })
        .unwrap();
        (String::from_utf8(out).unwrap(), asked)
    }

    /// Byte for byte what rbw's `pinentry::getpin` writes.
    const RBW: &str = "SETTITLE rbw\nSETPROMPT Master Password\n\
                       SETDESC Unlock the local database for 'rbw'\nGETPIN\n";

    #[test]
    fn rbw_unlock() {
        let (out, asked) = session(RBW, vec![Answer::Pin(pin("hunter2"))]);
        assert_eq!(out, "OK Pleased to meet you\nOK\nOK\nOK\nD hunter2\nOK\n");
        assert_eq!(
            asked,
            [Prompt {
                title: Some("rbw".into()),
                description: Some("Unlock the local database for 'rbw'".into()),
                prompt: Some("Master Password".into()),
                error: None,
            }]
        );
    }

    #[test]
    fn rbw_retry_shows_the_error_once() {
        let input = RBW.replace("GETPIN", "SETERROR Invalid password\nGETPIN\nGETPIN");
        let (_, asked) = session(&input, vec![Answer::Cancel, Answer::Cancel]);
        assert_eq!(asked[0].message().as_deref(), Some("Invalid password"));
        assert_eq!(asked[1].error, None);
        assert_eq!(
            asked[1].message().as_deref(),
            Some("Unlock the local database for 'rbw'")
        );
    }

    #[test]
    fn cancel_is_the_code_rbw_knows() {
        let (out, _) = session("GETPIN\n", vec![Answer::Cancel]);
        assert_eq!(out, format!("OK Pleased to meet you\n{CANCELED}\n"));
        assert!(out.contains("ERR 83886179 "));
    }

    #[test]
    fn escapes_the_pin() {
        let (out, _) = session("GETPIN\n", vec![Answer::Pin(pin("50%\u{e9}"))]);
        assert!(out.ends_with("D 50%25\u{e9}\nOK\n"), "{out}");
    }

    #[test]
    fn empty_pin_sends_no_data() {
        let (out, _) = session("GETPIN\n", vec![Answer::Pin(pin(""))]);
        assert_eq!(out, "OK Pleased to meet you\nOK\n");
    }

    #[test]
    fn long_pins_split_into_lines() {
        let long = "%".repeat(400);
        let (out, _) = session("GETPIN\n", vec![Answer::Pin(pin(&long))]);
        let data: Vec<&str> = out.lines().filter(|l| l.starts_with("D ")).collect();
        assert!(data.len() > 1);
        // Each line plus its newline fits.
        assert!(data.iter().all(|l| l.len() < LINE_MAX));
        let joined: String = data.iter().map(|l| &l[2..]).collect();
        assert_eq!(joined, "%25".repeat(400));
    }

    #[test]
    fn failure_reason_follows_the_code() {
        let reason = "no Wayland\ncompositor".to_owned();
        let (out, _) = session("GETPIN\n", vec![Answer::Failed(reason)]);
        assert!(out.ends_with("ERR 83886081 no Wayland compositor\n"), "{out}");
    }

    #[test]
    fn decodes_arguments() {
        let (_, asked) = session("SETDESC a%0Ab%25c%zz\nGETPIN\n", vec![Answer::Cancel]);
        assert_eq!(asked[0].description.as_deref(), Some("a\nb%c%zz"));
        assert_eq!(asked[0].message().as_deref(), Some("a b%c%zz"));
    }

    #[test]
    fn housekeeping() {
        let input = "# hi\n\nOPTION ttyname=/dev/pts/1\nGETINFO flavor\nGETINFO nope\n\
                     CONFIRM\nSETPROMPT x\nRESET\nGETPIN\nBYE\nGETPIN\n";
        let (out, asked) = session(input, vec![Answer::Cancel]);
        assert_eq!(
            out,
            format!(
                "OK Pleased to meet you\nOK\nD sieb\nOK\n{BAD_PARAMETER}\n{UNKNOWN_COMMAND}\n\
                 OK\nOK\n{CANCELED}\nOK closing connection\n"
            )
        );
        // RESET dropped the prompt, and nothing after BYE ran.
        assert_eq!(asked, [Prompt::default()]);
    }
}
