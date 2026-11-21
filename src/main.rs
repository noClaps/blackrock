use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};

use crate::vault::Vault;

mod escape;
mod frontmatter;
mod highlight;
mod render;
mod vault;

const HELP: &str = "\
Convert an Obsidian-flavored markdown file into an HTML fragment.

Usage: blackrock [--metadata] [input]

Arguments:
  [input]        The markdown file to read. Reads from stdin if omitted.

Options:
  --metadata     Output the YAML frontmatter as JSON instead of rendering HTML.
  -h, --help     Print help.
  -V, --version  Print version.";

fn main() -> Result<()> {
    let mut input: Option<PathBuf> = None;
    let mut metadata = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{HELP}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("blackrock {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--metadata" => metadata = true,
            option if option.starts_with('-') => bail!("unknown option {option}; see --help"),
            _ if input.is_none() => input = Some(arg.into()),
            _ => bail!("too many arguments; see --help"),
        }
    }

    let source = match &input {
        Some(path) => {
            fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?
        }
        None => io::read_to_string(io::stdin()).context("cannot read stdin")?,
    };

    let (yaml, body) = frontmatter::split(&source);
    if metadata {
        let json = frontmatter::to_json(yaml).context("invalid frontmatter")?;
        println!("{json:#}\n");
    } else {
        let vault = Vault::open(&std::env::current_dir().context("cannot get current directory")?)?;
        let note_dir = input
            .as_deref()
            .and_then(Path::parent)
            .unwrap_or(Path::new(""));
        println!(
            "{}",
            render::to_html(body, &vault, note_dir).map_err(|es| anyhow!("{}", es.join("\n")))?
        );
    }
    Ok(())
}
