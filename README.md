# blackrock

Converts an Obsidian-flavored markdown file into an HTML fragment.

Run it from the root of an Obsidian vault (the directory containing `.obsidian`):

```sh
cd ~/my-vault
blackrock notes/note.md > note.html     # render HTML
blackrock --metadata notes/note.md      # print the YAML frontmatter as JSON
cat notes/note.md | blackrock           # read from stdin
```

If anything can't be rendered (a missing image, invalid math), every error is printed to stderr,
nothing is written to stdout, and the exit status is 1.

## Installing in CI

This repository is an action that downloads a release binary for the runner (Linux or macOS,
x86-64 or ARM64) and adds it to the `PATH`. It works on GitHub Actions:

```yaml
- uses: noClaps/blackrock@master
  with:
    version: v0.1.0 # optional, defaults to the latest release
- run: blackrock notes/note.md > note.html
```

And on Forgejo Actions, using the full URL:

```yaml
- uses: https://github.com/noClaps/blackrock@master
```

The `version` input picks which release binary to install; `@master` only picks the version of
the action itself. It needs `curl` or `wget`, which the usual runner images have. The Linux
binaries need glibc, so they don't run on Alpine.

## Features

- CommonMark plus tables, footnotes, strikethrough and task lists
- PNG, JPEG, GIF, SVG, WebP and AVIF images (`![[cat.png]]`, `![alt](cat.png)`) are embedded as
  `data:` URLs. Like Obsidian, they are looked up relative to the note, then the vault root, then
  anywhere in the vault by name. Remote images (`https://…`) are left as links. Sizes work as in Obsidian:
  `![[cat.png|300]]`, `![[cat.png|A cat|300x200]]`, `![A cat|300](cat.png)`
- Images with alt text become `<figure>`s with the alt text as the `<figcaption>`. Links,
  formatting and math are rendered in the caption; the `alt` attribute gets their plain text, with
  math as LaTeX
- Obsidian syntax: `[[wikilinks]]`, `![[embeds]]` (embeds of other files become links), callouts
  (`> [!note]`, foldable with `+`/`-`), `==highlights==`, `#tags`, `%%comments%%`, `^block-ids`
- Headings get `id`s, so `[[Note#Heading]]` and `[[#Heading]]` links resolve
- Inline (`$…$`) and block (`$$…$$`) LaTeX math rendered to [MathML Core] with [math-core]
- Code blocks highlighted with Tree-sitter using GitHub's colors, as inline
  `style="color:light-dark(…)"` on `<span>`s. The `<pre>` and `<code>` elements get no styles.
  Languages use [Shiki]'s ids and aliases: `c`, `cpp` (`c++`), `css`, `go`, `html` (with embedded
  JavaScript and CSS), `java`, `javascript` (`js`, `cjs`, `mjs`), `json`, `jsx`, `python` (`py`),
  `ruby` (`rb`), `rust` (`rs`), `shellscript` (`bash`, `sh`, `shell`, `zsh`), `toml`, `tsx`,
  `typescript` (`ts`, `cts`, `mts`) and `yaml` (`yml`)

`light-dark()` picks a color based on the element's used color scheme, so the page needs to
declare which schemes it supports, e.g. `:root { color-scheme: light dark; }`.

[MathML Core]: https://www.w3.org/TR/mathml-core/
[math-core]: https://github.com/tmke8/math-core
[Shiki]: https://shiki.style/languages
