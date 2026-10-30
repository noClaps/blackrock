mod frontmatter;
mod highlight;
mod render;
mod vault;

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::vault::Vault;

const HELP: &str = "\
Convert an Obsidian-flavored markdown file into an HTML fragment.

Usage: blackrock [--metadata] [INPUT]

Arguments:
  [INPUT]  The markdown file to read. Reads from stdin if omitted.

Options:
      --metadata  Output the YAML frontmatter as JSON instead of rendering HTML.
  -h, --help      Print help.
  -V, --version   Print version.
";

fn main() -> ExitCode {
    let errors = match run() {
        Ok(output) => match io::stdout().write_all(output.as_bytes()) {
            Ok(()) => return ExitCode::SUCCESS,
            Err(e) => vec![format!("cannot write output: {e}")],
        },
        Err(errors) => errors,
    };
    for error in errors {
        eprintln!("blackrock: {error}");
    }
    ExitCode::FAILURE
}

fn run() -> Result<String, Vec<String>> {
    let mut input: Option<PathBuf> = None;
    let mut metadata = false;
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("-h" | "--help") => return Ok(HELP.into()),
            Some("-V" | "--version") => {
                return Ok(concat!("blackrock ", env!("CARGO_PKG_VERSION"), "\n").into());
            }
            Some("--metadata") => metadata = true,
            Some(option) if option.starts_with('-') => {
                return Err(vec![format!("unknown option {option}; see --help")]);
            }
            _ if input.is_none() => input = Some(arg.into()),
            _ => return Err(vec!["too many arguments; see --help".into()]),
        }
    }

    let cwd =
        std::env::current_dir().map_err(|e| vec![format!("cannot get current directory: {e}")])?;
    let vault = Vault::open(&cwd).map_err(|e| vec![e])?;

    let path = input.as_deref();
    let source = match path {
        Some(path) => fs::read_to_string(path),
        None => io::read_to_string(io::stdin()),
    };
    let source = source.map_err(|e| {
        let name = path.map_or("stdin".into(), |path| path.display().to_string());
        vec![format!("cannot read {name}: {e}")]
    })?;

    let (yaml, body) = frontmatter::split(&source);
    if metadata {
        let json =
            frontmatter::to_json(yaml).map_err(|e| vec![format!("invalid frontmatter: {e}")])?;
        Ok(format!("{json:#}\n"))
    } else {
        let note_dir = path.and_then(Path::parent).unwrap_or(Path::new(""));
        render::to_html(body, &vault, note_dir)
    }
}
