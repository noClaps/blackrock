use std::process::{Command, Output};

fn blackrock(dir: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_blackrock"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn renders_html() {
    let output = blackrock("examples", &["sample.md"]);
    assert!(output.status.success());
    let html = String::from_utf8(output.stdout).unwrap();
    assert!(
        html.starts_with("<h1 id=\"sample-note\">Sample note</h1>\n"),
        "{html}"
    );
}

#[test]
fn prints_metadata() {
    let output = blackrock("examples", &["--metadata", "sample.md"]);
    assert!(output.status.success());
    let json = String::from_utf8(output.stdout).unwrap();
    assert!(
        json.starts_with("{\n  \"title\": \"Sample note\",\n"),
        "{json}"
    );
}

#[test]
fn requires_vault_root() {
    let output = blackrock(".", &["examples/sample.md"]);
    assert!(!output.status.success());
    assert_eq!(output.stdout, b"");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("is not the root of an Obsidian vault"),
        "{stderr}"
    );
}

#[test]
fn reports_missing_files() {
    let output = blackrock("examples", &["missing.md"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.starts_with("blackrock: cannot read missing.md: "),
        "{stderr}"
    );
}

#[test]
fn prints_help_and_version() {
    let help = blackrock(".", &["--help"]);
    assert!(help.status.success());
    assert!(
        help.stdout
            .starts_with(b"Convert an Obsidian-flavored markdown file")
    );
    let version = blackrock(".", &["-V"]);
    assert_eq!(
        version.stdout,
        format!("blackrock {}\n", env!("CARGO_PKG_VERSION")).as_bytes()
    );
}

#[test]
fn rejects_bad_arguments() {
    for (args, error) in [
        (
            &["--nope"][..],
            "blackrock: unknown option --nope; see --help\n",
        ),
        (
            &["a.md", "b.md"],
            "blackrock: too many arguments; see --help\n",
        ),
    ] {
        let output = blackrock("examples", args);
        assert!(!output.status.success());
        assert_eq!(String::from_utf8(output.stderr).unwrap(), error);
    }
}
