# blackrock

Converts an Obsidian-flavored markdown file into an HTML fragment.

```sh
blackrock note.md > note.html     # render HTML
blackrock --metadata note.md      # print the YAML frontmatter as JSON
cat note.md | blackrock           # read from stdin
```

## Features

- CommonMark plus tables, footnotes, strikethrough and task lists
- Obsidian syntax: `[[wikilinks]]`, `![[embeds]]` (images, audio, video, PDFs), callouts
  (`> [!note]`, foldable with `+`/`-`), `==highlights==`, `#tags`, `%%comments%%`, `^block-ids`
- Headings get `id`s, so `[[Note#Heading]]` and `[[#Heading]]` links resolve
- Inline (`$…$`) and block (`$$…$$`) LaTeX math rendered to [MathML Core] with [math-core].
  Invalid math is shown as `<code class="math-error">` and reported on stderr
- Code blocks highlighted with Tree-sitter using GitHub's colors, as inline
  `style="color:light-dark(…)"` on `<span>`s. The `<pre>` and `<code>` elements get no styles.
  Supported: Bash, C, C++, CSS, Go, HTML (with embedded JS/CSS), Java, JavaScript/JSX, JSON,
  Python, Ruby, Rust, TOML, TypeScript/TSX and YAML

`light-dark()` picks a color based on the element's used color scheme, so the page needs to
declare which schemes it supports, e.g. `:root { color-scheme: light dark; }`.

[MathML Core]: https://www.w3.org/TR/mathml-core/
[math-core]: https://github.com/tmke8/math-core
