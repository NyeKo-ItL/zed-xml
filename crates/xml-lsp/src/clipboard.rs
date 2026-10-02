//! Copies text to the system clipboard with the platform's tool: a language
//! server has no clipboard access in the LSP, and the `xml.copyXPath` command
//! must put the XPath where the user can paste it.

use std::{
    io::Write,
    process::{Command, Stdio},
};

/// Clipboard programs of the platform, in order of preference.
fn commands() -> &'static [&'static [&'static str]] {
    if cfg!(target_os = "macos") {
        &[&["pbcopy"]]
    } else if cfg!(windows) {
        &[&["clip"]]
    } else {
        &[
            &["wl-copy"],
            &["xclip", "-selection", "clipboard"],
            &["xsel", "--clipboard", "--input"],
        ]
    }
}

/// Copies `text` with the first clipboard program that works.
pub(crate) fn copy(text: &str) -> Result<(), String> {
    for command in commands() {
        let Ok(mut child) = Command::new(command[0])
            .args(&command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let written = child
            .stdin
            .take()
            .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
        if child.wait().is_ok_and(|status| status.success()) && written {
            return Ok(());
        }
    }
    let names = commands()
        .iter()
        .map(|command| command[0])
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!("no clipboard tool worked (tried {names})"))
}
