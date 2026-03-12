# Editing API Specification

## Contract

Every editing tool takes a `cell_id` and resolves exactly one text span to
replace. If the resolution is ambiguous (0 or >1 matches), the tool fails
with match count, offsets, and a disambiguation hint. `content` is always
treated as literal replacement text — no escape expansion.

```json
{
  "cell_id": "...",
  "pattern_or_match": "...",
  "content": "literal replacement text"
}
```

Semantics:
- Resolve exactly one match in that cell
- Replace that span with literal `content`
- Fail otherwise

## Tools

### `replace_match` — default for simple edits

Literal text matching with optional context for disambiguation.

```python
replace_match(
    cell_id="abc",
    match="a + b",
    content="a * b",
    context_before="return ",  # optional
    context_after="\n",        # optional
)
```

- `match` is literal text (regex-escaped internally)
- `context_before` / `context_after` are literal anchors (also escaped)
- Must resolve to exactly one location

### `replace_regex` — for structural edits

Full Python regex when you need anchors, lookarounds, line boundaries, or
zero-width insertions.

```python
replace_regex(
    cell_id="abc",
    pattern=r"(?<=return )a \+ b",
    content="a * b",
)
```

- Pattern compiled with `re.MULTILINE`
- Must resolve to exactly one match
- Zero-width matches allowed (for insertions)
- `content` is literal — not `re.sub` replacement syntax

### `set_cell_source` — escape hatch

Full cell rewrite. Use only when targeted edits are too awkward or ambiguous.

## Tool Selection

| Situation | Tool |
|-----------|------|
| Replace a short, identifiable string | `replace_match` |
| Context disambiguates naturally | `replace_match` with `context_before`/`context_after` |
| Need anchors, lookarounds, or line boundaries | `replace_regex` |
| Zero-width insertion (before/after a target) | `replace_regex` with `(?=...)` or `(?<=...)` |
| Append to end of cell | `replace_regex` with `\Z` |
| Full cell rewrite | `set_cell_source` |

## Recommended Patterns

### Line-targeted edit

```python
# Match an entire line
replace_regex(cell_id, r"(?m)^print\(x\)$", "print(y)")
```

### String literal replacement

```python
# Replace only the contents, not the quotes
replace_match(cell_id, match="draft", content="final",
              context_before='label = "', context_after='"')
```

### Zero-width insertion before a function

```python
replace_regex(cell_id, r"(?=def calculate\()", "@cache\n")
```

### Zero-width insertion after an import

```python
replace_regex(cell_id, r"(?<=import math\n)", "import statistics\n")
```

### Append to end of cell

```python
replace_regex(cell_id, r"\Z", "\nprint(result)\n")
```

### Expression rewrite

```python
replace_match(cell_id, match="a + b", content="a * b",
              context_before="return ")
```

## Anti-patterns

### Broad tokens without context

```python
# BAD — "1" matches everywhere
replace_match(cell_id, match="1", content="2")

# GOOD — anchored with context
replace_match(cell_id, match="1", content="2",
              context_before="x = ")
```

### `$` for end-of-cell append

```python
# BAD — $ matches end of every line in MULTILINE mode
replace_regex(cell_id, r"$", "\nmore code")

# GOOD — \Z matches end of string only
replace_regex(cell_id, r"\Z", "\nmore code")
```

### Unanchored patterns that match comments/strings

```python
# BAD — matches "print(x)" in a comment too
replace_regex(cell_id, r"print\(x\)", "print(y)")

# GOOD — anchored to line start
replace_regex(cell_id, r"(?m)^print\(x\)$", "print(y)")
```

### Escaped newlines in content

```python
# BAD — content is literal, \n is two characters
replace_match(cell_id, match="pass", content="return 42\nprint('done')")
#                                              ↑ this is a real newline ✓

# Actually fine — Python string literals with real newlines work.
# The risk is JSON-level confusion where \n might be literal backslash-n.
# Always use real newlines in content, never escaped sequences.
```

### Multiline regex without `(?s)`

```python
# BAD — . doesn't match newlines by default
replace_regex(cell_id, r"def foo\(.*\):", "def foo(x, y):")

# GOOD — use (?s) for dotall, or be explicit about newlines
replace_regex(cell_id, r"(?s)def foo\(.*?\):", "def foo(x, y):")
```

## Failure Policy

All failures include:
- `match_count` — how many matches were found (0 or >1)
- `match offsets` — character positions of up to 5 matches (for >1 case)
- `disambiguation hint` — suggests using `context_before`/`context_after`

| Condition | Result |
|-----------|--------|
| 0 matches | Fail with "no match found" |
| >1 matches | Fail with count + offsets |
| Invalid regex | Fail with parse error |
| Source too large (>1MB) | Fail before regex runs |

Ambiguity failures are a feature. They force the agent to be more precise
rather than silently editing the wrong location.

## Prompting Guidance

When instructing a model to use these tools:

1. **Edit only within the provided `cell_id`.**
2. **Prefer the smallest possible edit.** Don't rewrite the whole cell when
   changing one expression.
3. **Your pattern must match exactly the text span to replace.** No more,
   no less.
4. **Put context in lookbehind/lookahead or `context_before`/`context_after`.**
   Don't widen the match to include context you don't want to replace.
5. **If a plain-text match can do it, prefer `replace_match`.** It's simpler
   and harder to get wrong.
6. **If the target may appear in strings, comments, or repeated lines, anchor
   more tightly.** Use line anchors (`^...$` with `(?m)`) or surrounding
   context.
7. **For insertion, use a zero-width regex.** `(?=target)` inserts before,
   `(?<=target)` inserts after.
8. **For appending to the end of a cell, use `\Z`.** Not `$` (which matches
   end of every line in multiline mode).
9. **`content` is literal text.** What you write is what appears in the cell.
   Real newlines, real indentation, no escape interpretation.

## Presence Side-effects

When `replace_match` or `replace_regex` execute:

1. The agent's cursor appears at the edit start position
2. The CRDT mutation is applied atomically
3. The cursor moves to the end of the replacement

This gives visual feedback in the notebook UI showing where the agent is
working. Presence is best-effort — failures don't block the edit.