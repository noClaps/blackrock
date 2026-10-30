mod frontmatter;
mod highlight;
mod math;
mod render;
mod vault;

use std::io::{self, Read, Write};
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
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(errors) => {
            for error in errors {
                eprintln!("blackrock: {error}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<(), Vec<String>> {
    let fail = |message: String| vec![message];
    let cwd =
        std::env::current_dir().map_err(|e| fail(format!("cannot get current directory: {e}")))?;
    let vault = Vault::open(&cwd).map_err(fail)?;

    let (source, note_dir) = match &cli.input {
        Some(path) if path.as_os_str() != "-" => {
            let source = std::fs::read_to_string(path)
                .map_err(|e| fail(format!("cannot read {}: {e}", path.display())))?;
            (source, path.parent().unwrap_or(Path::new("")))
        }
        _ => {
            let mut source = String::new();
            io::stdin()
                .read_to_string(&mut source)
                .map_err(|e| fail(format!("cannot read stdin: {e}")))?;
            (source, Path::new(""))
        }
    };

    let (yaml, body) = frontmatter::split(&source);
    let output = if cli.metadata {
        let json =
            frontmatter::to_json(yaml).map_err(|e| fail(format!("invalid frontmatter: {e}")))?;
        let mut json = serde_json::to_string_pretty(&json).expect("JSON values serialize");
        json.push('\n');
        json
    } else {
        render::to_html(body, &vault, note_dir)?
    };
    io::stdout()
        .write_all(output.as_bytes())
        .map_err(|e| fail(format!("cannot write output: {e}")))
}
