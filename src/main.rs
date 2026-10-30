mod frontmatter;
mod highlight;
mod math;
mod render;
mod vault;

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use crate::vault::Vault;

/// Convert an Obsidian-flavored markdown file into an HTML fragment.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// The markdown file to read. Reads from stdin if omitted or `-`.
    input: Option<PathBuf>,

    /// Output the YAML frontmatter as JSON instead of rendering HTML.
    #[arg(long)]
    metadata: bool,
}

fn main() -> ExitCode {
    let errors = match run(&Cli::parse()) {
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

fn run(cli: &Cli) -> Result<String, Vec<String>> {
    let cwd =
        std::env::current_dir().map_err(|e| vec![format!("cannot get current directory: {e}")])?;
    let vault = Vault::open(&cwd).map_err(|e| vec![e])?;

    let path = cli.input.as_deref().filter(|path| *path != Path::new("-"));
    let source = match path {
        Some(path) => fs::read_to_string(path),
        None => io::read_to_string(io::stdin()),
    };
    let source = source.map_err(|e| {
        let name = path.map_or("stdin".into(), |path| path.display().to_string());
        vec![format!("cannot read {name}: {e}")]
    })?;

    let (yaml, body) = frontmatter::split(&source);
    if cli.metadata {
        let json =
            frontmatter::to_json(yaml).map_err(|e| vec![format!("invalid frontmatter: {e}")])?;
        Ok(format!("{json:#}\n"))
    } else {
        let note_dir = path.and_then(Path::parent).unwrap_or(Path::new(""));
        render::to_html(body, &vault, note_dir)
    }
}
