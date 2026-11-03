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
- uses: noClaps/blackrock@v0.3.1
  with:
    version: v0.3.1 # optional, defaults to the latest release
- run: blackrock notes/note.md > note.html
```

And on Forgejo Actions, using the full URL:

```yaml
- uses: https://github.com/noClaps/blackrock@v0.3.1
```

The `version` input picks which release binary to install; the tag after `@` only picks the
version of the action itself. It needs `curl` or `wget`, which the usual runner images have. The Linux
binaries need glibc, so they don't run on Alpine.

## Features

- CommonMark plus tables, footnotes, strikethrough and task lists. Tables are wrapped in a `<div>`
- PNG, JPEG, GIF, SVG, WebP and AVIF images (`![[cat.png]]`, `![alt](cat.png)`) are embedded as
  `data:` URLs, base64-encoded except for SVGs, which are percent-encoded to stay smaller. Like
  Obsidian, they are looked up relative to the note, then the vault root, then
  anywhere in the vault by name. Remote images (`https://…`) are left as links. Sizes work as in Obsidian:
  `![[cat.png|300]]`, `![[cat.png|A cat|300x200]]`, `![A cat|300](cat.png)`
- Images with alt text become `<figure>`s with the alt text as the `<figcaption>`. Links,
  formatting and math are rendered in the caption; the `alt` attribute gets their plain text, with
  math as LaTeX
- Obsidian syntax: `[[wikilinks]]`, `![[embeds]]` (embeds of other files become links), callouts
  (`> [!note]`, foldable with `+`/`-`), `==highlights==`, `#tags`, `%%comments%%`, `^block-ids`
- Headings get `id`s, so `[[Note#Heading]]` and `[[#Heading]]` links resolve
- Footnotes (`[^1]`) are numbered in the order they're first referenced and collected at the end
  in a `<section class="footnotes">`, after an `<hr />`, each with a link back to its first
  reference
- Inline (`$…$`) and block (`$$…$$`) LaTeX math rendered to [MathML Core] with [math-core]. A
  `$$…$$` block on its own line is wrapped in a `<div>`
- Code blocks highlighted with [Irosashi] using [Shiki]'s grammars and Xcode's default light and
  dark themes, as inline `style="color:light-dark(…)"` on `<span>`s. The `<pre>` and `<code>`
  elements get no styles. Every language Shiki bundles is supported, by its Shiki id or alias

`light-dark()` picks a color based on the element's used color scheme, so the page needs to
declare which schemes it supports, e.g. `:root { color-scheme: light dark; }`.

[MathML Core]: https://www.w3.org/TR/mathml-core/
[math-core]: https://github.com/tmke8/math-core
[Irosashi]: https://github.com/frostybee/irosashi
[Shiki]: https://shiki.style/languages
