use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use ferromark::{Allocator, ast::FrontMatterKind};
use serde_json::{Value as Json, json};

use crate::vault::Vault;

mod escape;
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

    let arena = Allocator::for_source_len(source.len());
    let document = render::parse(&arena, &source).context("cannot parse markdown")?;
    if metadata {
        // TOML frontmatter isn't supported.
        let yaml = (document.front_matter.as_deref())
            .filter(|frontmatter| frontmatter.kind == FrontMatterKind::Yaml)
            .map_or("", |frontmatter| frontmatter.value);
        let json = match serde_yaml_ng::from_str(yaml).context("invalid frontmatter")? {
            // Empty frontmatter is an empty object.
            Json::Null => json!({}),
            json => json,
        };
        println!("{json:#}\n");
    } else {
        let vault = Vault::open(Path::new("."))?;
        let note_dir = input
            .as_deref()
            .and_then(Path::parent)
            .unwrap_or(Path::new(""));
        let mut warnings = Vec::new();
        let html = render::to_html(&document, &source, &vault, note_dir, &mut warnings);
        for warning in warnings {
            eprintln!("warning: {warning}");
        }
        println!("{}", html.map_err(|es| anyhow!("{}", es.join("\n")))?);
    }
    Ok(())
}
