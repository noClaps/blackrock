mod frontmatter;
mod render;

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

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
        Err(err) => {
            eprintln!("blackrock: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let source = match &cli.input {
        Some(path) if path.as_os_str() != "-" => std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?,
        _ => {
            let mut buf = String::new();
            io::stdin().read_to_string(&mut buf)?;
            buf
        }
    };

    let (yaml, body) = frontmatter::split(&source);
    let mut stdout = io::stdout().lock();
    if cli.metadata {
        let json = frontmatter::to_json(yaml).map_err(|e| format!("invalid frontmatter: {e}"))?;
        serde_json::to_writer_pretty(&mut stdout, &json)?;
        writeln!(stdout)?;
    } else {
        stdout.write_all(render::to_html(body).as_bytes())?;
    }
    Ok(())
}
