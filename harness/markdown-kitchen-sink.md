---
title: Kitchen sink
tags: [docs, markdown]
---

# GitPulse *Markdown* `viewer`

A paragraph with **bold**, _emphasis_, ***both***, ~~struck~~, ==highlighted **strong**==, `inline code`, a [relative link](docs/guide.md#install), a [root link](/README.md), an [external link](https://example.com/path?q=1), a [mail link](mailto:someone@example.com), a [section link](#tables), a [script link](javascript:alert(1)) and an autolink <https://example.org>.

Hard line break follows\
next line. Inline math $e^{i\pi} + 1 = 0$ and a #tag and a [[Wiki Link]].

## App

Heading named "App" — its id must not shadow the app's `#app`.

## 1. Lists

- one
- two
  - nested **two.a**
  - nested two.b
    1. deep ordered
    2. deep ordered two
- three

1. first
2. second
   > quoted inside a list

- [ ] task open
- [x] task done

Term
: Definition of the term.

## Tables

| Left | Center | Right |
|:-----|:------:|------:|
| a | **b** | `c` |
| long cell with a [link](https://example.com) | x | 1.0 |

## Quotes and callouts

> A plain quote
> spanning two lines.
>
> > Nested quote.

> [!NOTE]
> A note callout with **markup**.

> [!WARNING] Custom title
> A warning callout.

> [!TIP]
> Tip.

> [!IMPORTANT]
> Important.

> [!CAUTION]
> Caution.

## Code

```rust
fn main() {
    println!("hello <world> & \"friends\"");
}
```

    indented code block

```
no language
```

## Math

$$
\int_0^1 x^2 \, dx = \frac{1}{3}
$$

## Media

![A badge](https://img.shields.io/badge/build-passing-green.svg)
![Missing local picture](images/missing.png)

## Footnotes

A claim with a footnote.[^1] And another.[^note]

[^1]: The first footnote.
[^note]: A named footnote with `code`.

---

<div onclick="alert(1)">raw html is shown as text</div>

Final paragraph.
