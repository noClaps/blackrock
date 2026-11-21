use std::{
    io::Write,
    process::{Command, Output, Stdio},
};

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

/// Runs `blackrock --metadata` on `input` from stdin, returning stdout.
fn metadata(input: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_blackrock"))
        .arg("--metadata")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{input:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn converts_metadata_to_json() {
    assert_eq!(
        metadata("---\ntitle: Hi\ntags: [a, b]\n1: one\ndate: 2024-01-01\n---\n# Body\n"),
        "{\n  \"title\": \"Hi\",\n  \"tags\": [\n    \"a\",\n    \"b\"\n  ],\n  \
         \"1\": \"one\",\n  \"date\": \"2024-01-01\"\n}\n\n"
    );
    // CRLF, a byte order mark, trailing spaces and a closing line at the end all work, and only
    // a whole `---` line closes the frontmatter.
    for input in [
        "---\r\nt: 1\r\n---\r\nBody",
        "\u{feff}---\nt: 1\n---\nBody",
        "---  \nt: 1\n---  \nBody",
        "---\nt: 1\n---",
    ] {
        assert_eq!(metadata(input), "{\n  \"t\": 1\n}\n\n", "{input:?}");
    }
    assert_eq!(
        metadata("---\nt: a---\n---\n"),
        "{\n  \"t\": \"a---\"\n}\n\n"
    );
}

#[test]
fn missing_metadata_is_empty() {
    for input in [
        "# Body\n",
        "---\n---\nBody",
        "---\nt: 1\n",
        "+++\nt = 1\n+++\nBody",
    ] {
        assert_eq!(metadata(input), "{}\n\n", "{input:?}");
    }
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
fn reports_missing_files() {
    let output = blackrock("examples", &["missing.md"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.starts_with("Error: cannot read missing.md"),
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
            "Error: unknown option --nope; see --help\n",
        ),
        (&["a.md", "b.md"], "Error: too many arguments; see --help\n"),
    ] {
        let output = blackrock("examples", args);
        assert!(!output.status.success());
        assert_eq!(String::from_utf8(output.stderr).unwrap(), error);
    }
}
