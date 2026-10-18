//! `xml-lsp format [--check] [--tab-size N] [--tabs] FILE...`: formats XML
//! files without an editor.
//!
//! An editor that does not hand a very large or very long-line document to
//! the language server cannot format it through LSP; this command runs the
//! same formatter directly, so it can be bound to an editor task. `-` reads
//! the standard input and writes the formatted text to the standard output.
//! Exit status: 0 when every file is formatted (or, with `--check`, already
//! formatted), 1 when `--check` finds a file to format, 2 on an error.

use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::Instant,
};

use xml_core::{FormatOptions, LineEnding, format_xml_with, text::strip_bom};

const USAGE: &str = "usage: xml-lsp format [--check] [--tab-size N] [--tabs] FILE...";

/// Command line of the `format` subcommand (the arguments after `format`).
#[derive(Debug, PartialEq, Eq)]
struct Command {
    check: bool,
    tab_size: usize,
    tabs: bool,
    files: Vec<String>,
}

fn parse(arguments: &[String]) -> Result<Command, String> {
    let mut command = Command {
        check: false,
        tab_size: FormatOptions::default().tab_size,
        tabs: false,
        files: Vec::new(),
    };
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--check" => command.check = true,
            "--tabs" => command.tabs = true,
            "--tab-size" => {
                command.tab_size = arguments
                    .next()
                    .and_then(|value| value.parse::<usize>().ok())
                    .filter(|size| *size <= 16)
                    .ok_or("--tab-size expects a number between 0 and 16")?;
            }
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            file => command.files.push(file.to_owned()),
        }
    }
    if command.files.is_empty() {
        return Err("no file given".to_owned());
    }
    Ok(command)
}

/// The formatted text of `source`, keeping a byte order mark and the line
/// ending of the document.
fn format_source(source: &str, command: &Command) -> Result<String, String> {
    let (body, bom) = strip_bom(source);
    let options = FormatOptions {
        tab_size: command.tab_size,
        insert_spaces: !command.tabs,
        line_ending: LineEnding::detect(body),
        ..FormatOptions::default()
    };
    let formatted = format_xml_with(body, &options)?;
    Ok(format!("{}{formatted}", &source[..bom]))
}

fn read_utf8(path: &str) -> Result<String, String> {
    let bytes = if path == "-" {
        let mut bytes = Vec::new();
        std::io::stdin()
            .read_to_end(&mut bytes)
            .map_err(|error| format!("cannot read the standard input: {error}"))?;
        bytes
    } else {
        fs::read(path).map_err(|error| format!("cannot read {path}: {error}"))?
    };
    String::from_utf8(bytes).map_err(|_| format!("{path} is not UTF-8"))
}

/// Runs the subcommand and returns the process exit status.
pub fn run(arguments: &[String]) -> i32 {
    let command = match parse(arguments) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("xml-lsp format: {error}\n{USAGE}");
            return 2;
        }
    };
    let mut status = 0;
    for path in &command.files {
        let started = Instant::now();
        let outcome = read_utf8(path).and_then(|source| {
            let formatted = format_source(&source, &command)
                .map_err(|error| format!("cannot format {path}: {error}"))?;
            Ok((source, formatted))
        });
        match outcome {
            Err(error) => {
                eprintln!("xml-lsp format: {error}");
                status = 2;
            }
            Ok((_, formatted)) if path == "-" => {
                if std::io::stdout().write_all(formatted.as_bytes()).is_err() {
                    status = 2;
                }
            }
            Ok((source, formatted)) if source == formatted => {
                eprintln!("{path}: already formatted");
            }
            Ok(_) if command.check => {
                eprintln!("{path}: needs formatting");
                status = status.max(1);
            }
            Ok((_, formatted)) => match fs::write(Path::new(path), formatted) {
                Ok(()) => eprintln!("{path}: formatted in {:.2?}", started.elapsed()),
                Err(error) => {
                    eprintln!("xml-lsp format: cannot write {path}: {error}");
                    status = 2;
                }
            },
        }
    }
    status
}

#[cfg(test)]
mod tests;
