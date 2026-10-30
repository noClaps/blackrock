---
title: Sample note
tags: [rust, obsidian]
aliases:
  - Example
published: 2026-10-31
draft: false
rating: 4.5
---

# Sample note

Links to [[Other Note]], [[Other Note#Some Section|a section]] and [[#Code]]. Tagged #example and #nested/tag.

This is ==highlighted== text with a %%hidden%% comment and **bold** text. ^intro

> [!warning]- Watch out
> Callouts can be *folded*.
>
> > [!tip]
> > And nested.

## Math

Inline $e^{i\pi} + 1 = 0$ and a block:

$$
\int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}
$$

## Code

```rust
/// Adds two numbers.
fn add(a: i32, b: i32) -> i32 {
    a + b // sum
}
```

```python
def greet(name: str) -> None:
    print(f"Hello, {name}!")
```

```ts
interface User { id: number; name: string }
const users: User[] = [];
```

| Feature | Supported |
| ------- | --------- |
| Tables  | yes       |

- [x] Done
- [ ] Todo

![[diagram.svg|The conversion pipeline|240]]

An inline icon ![](attachments/diagram.svg) without alt text.

Footnote[^1].

[^1]: The footnote.
