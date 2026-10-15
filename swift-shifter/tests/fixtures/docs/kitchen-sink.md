---
title: Kitchen Sink
author: [Ann Lee, Bo Chen]
---

# Kitchen Sink

A paragraph with *emphasis*, **strong**, ~~strike~~, `code`, H~2~O, x^2^,
a [link](https://example.org "Example"), an autolink <https://rust-lang.org>,
and inline math $e^{i\pi} + 1 = 0$.[^first]

## Lists

1. First
2. Second
   - nested *bullet*
   - [x] done task
3. Third

- Loose item one

  with a second paragraph

- Loose item two

## Code and math

```rust
fn main() {
    println!("hello");
}
```

$$
\int_0^1 x^2 \, dx = \frac{1}{3}
$$

> A block quote
> spanning two lines.

| Name | Qty | Note |
|:-----|----:|:----:|
| apple | 3 | crisp |
| 梨子 | 12 | 很甜 |

---

Snake_case_identifiers and 3 * 4 stay readable.

[^first]: A footnote with **bold**.
