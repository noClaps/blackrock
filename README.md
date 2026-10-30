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

## Features

- CommonMark plus tables, footnotes, strikethrough and task lists
- PNG, JPEG, GIF, SVG, WebP and AVIF images (`![[cat.png]]`, `![alt](cat.png)`) are embedded as
  `data:` URLs. Like Obsidian, they are looked up relative to the note, then the vault root, then
  anywhere in the vault by name. Remote images (`https://…`) are left as links. Sizes work as in Obsidian:
  `![[cat.png|300]]`, `![[cat.png|A cat|300x200]]`, `![A cat|300](cat.png)`
- Images with alt text become `<figure>`s with the alt text as the `<figcaption>`
- Obsidian syntax: `[[wikilinks]]`, `![[embeds]]` (embeds of other files become links), callouts
  (`> [!note]`, foldable with `+`/`-`), `==highlights==`, `#tags`, `%%comments%%`, `^block-ids`
- Headings get `id`s, so `[[Note#Heading]]` and `[[#Heading]]` links resolve
- Inline (`$…$`) and block (`$$…$$`) LaTeX math rendered to [MathML Core] with [math-core]
- Code blocks highlighted with Tree-sitter using GitHub's colors, as inline
  `style="color:light-dark(…)"` on `<span>`s. The `<pre>` and `<code>` elements get no styles.
  Supported: Bash, C, C++, CSS, Go, HTML (with embedded JS/CSS), Java, JavaScript/JSX, JSON,
  Python, Ruby, Rust, TOML, TypeScript/TSX and YAML

`light-dark()` picks a color based on the element's used color scheme, so the page needs to
declare which schemes it supports, e.g. `:root { color-scheme: light dark; }`.

[MathML Core]: https://www.w3.org/TR/mathml-core/
[math-core]: https://github.com/tmke8/math-core
