# blackrock

Converts an Obsidian-flavored markdown file into an HTML fragment.

Run it from the root of your project:

```sh
cd ~/my-vault
blackrock notes/note.md > note.html     # render HTML
blackrock --metadata notes/note.md      # print the YAML frontmatter as JSON
cat notes/note.md | blackrock           # read from stdin
```

If anything can't be rendered (a missing image, invalid math), every error is printed to stderr, nothing is written to stdout, and the exit status is 1.

## Installing in CI

This repository is an action that downloads a release binary for the runner (Linux or macOS, x86-64 or ARM64) and adds it to the `PATH`. It works on GitHub Actions:

```yaml
- uses: noClaps/blackrock@v0.6.0
  with:
    version: v0.6.0 # optional, defaults to the latest release
- run: blackrock notes/note.md > note.html
```

The `version` input picks which release binary to install; the tag after `@` only picks the version of the action itself. It needs `curl` or `wget`, which the usual runner images have. The Linux binaries need glibc, so they don't run on Alpine.

## Features

- Markdown parsed with [ferromark](https://github.com/sebastian-software/ferromark): CommonMark plus tables, footnotes, strikethrough and task lists. Tables are wrapped in a `<div>`
- PNG, JPEG, GIF, SVG, WebP and AVIF images (`![alt](cat.png)`) are embedded as `data:` URLs, base64-encoded except for SVGs, which are percent-encoded to stay smaller. Like Obsidian, they are looked up relative to the note, then the vault root, then anywhere in the vault by name. Remote images (`https://…`) are left as links. Sizes work as in Obsidian: `![|300](cat.png)`, `![A cat|300x200](cat.png)`
- Images with alt text become `<figure>`s with the alt text as the `<figcaption>`. Links, formatting and math are rendered in the caption; the `alt` attribute gets their plain text, with math as LaTeX
- Obsidian syntax: callouts (`> [!note]`, foldable with `+`/`-`), `==highlights==`, `#tags`, `^block-ids`. `%%comments%%` aren't supported
- Headings get `id`s from ferromark's slugs (lowercase, with punctuation and spaces as `-`, e.g. `what-s-new`). Links to headings, written the way Obsidian writes them (`[x](#Some%20heading)`, `[x](Note#Some%20heading)`, or `[x](Note#Heading#Subheading)` for a heading under another), are rewritten to the `id` of the heading they point to, looked up in the linked note (found like images, with or without `.md`). With duplicate headings, this picks the right one, like `same-1`. Links written with the slug, like `[x](#some-heading)`, work too. Links to notes, found like images, with or without `.md`, become absolute URLs from the vault root without `.md`, like `[x](b.md#Some%20Heading)` in `notes/a.md` becoming `/notes/b#some-heading`. Links to other files are left as they are. If the heading can't be found, its fragment is left as it is, and if the note can't be found, the whole link is; either way, a warning is printed to stderr. Wikilinks aren't supported; use markdown links
- Footnotes (`[^1]`) are numbered in the order they're first referenced and collected at the end in a `<section class="footnotes">`, after an `<hr />`, each with a link back to its first reference
- Inline (`$…$`) and block (`$$…$$`) LaTeX math rendered to [MathML Core](https://www.w3.org/TR/mathml-core/) with [math-core](https://github.com/tmke8/math-core). A `$$` line starts a block that ends at the next `$$` line, and the lines in between are never parsed as markdown, so a LaTeX line starting with `- ` can't become a list. Blocks, and `$$…$$` on its own line, are wrapped in a `<div>`
- Code blocks highlighted with [Irosashi](https://github.com/frostybee/irosashi) using [Shiki](https://shiki.style/languages)'s grammars and Xcode's default light and dark themes, as inline `style="color:light-dark(…)"` on `<span>`s. The `<pre>` and `<code>` elements get no styles. Every language Shiki bundles is supported, by its Shiki id or alias

`light-dark()` picks a color based on the element's used color scheme, so the page needs to declare which schemes it supports, e.g. `:root { color-scheme: light dark; }`.
