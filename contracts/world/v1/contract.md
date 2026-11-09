# World contract v1

The machine-owned window layout for one computer. herdr-core stores it as a
document named `herdr-app`; the Desktop reads and writes that document and
renders it. Nothing in this contract describes what the user was looking at —
that is the Desktop's private view.

Schemas: `document.schema.json` (document, space, tab) and `node.schema.json`
(the recursive layout tree it references). `fixtures/document.json` is the
normative example.

## Document

```json
{"spaces": [{"id": "...", "name": "...", "folder": "...",
             "tabs": [{"id": "...", "name": "...", "root": <node>}]}]}
```

- `spaces` is ordered; the order is the order the user arranged them in.
- `folder` is the space's working folder (a path, possibly empty).
- `root` is a layout node tree; a tab with a single block has a one-node tree.

## Node

Every node is `{"type": "row" | "column" | "stack" | "block", ...}`.

| `type` | Structural fields | Meaning |
| --- | --- | --- |
| `row` | `children` | Children side by side. |
| `column` | `children` | Children stacked vertically. |
| `stack` | `children`, `active`? | Children overlaid; `active` names the visible one. |
| `block` | `content` | One pane. |

`content` is exactly one of:

- `{"terminal": "<terminal id>"}` — a herdr terminal.
- `{"mod": "<mod name>", "props"?: <any JSON>}` — a mod pane; `props` is
  mod-defined and opaque to this contract.
- `{"tab": "<tab id>", "pane"?: <pane frame>}` — a tab's terminals arranged in
  place. Layouts written before 0.2 used `context` for `tab`; decoders accept
  it, encoders always write `tab`.
- `{"agent": "<agent id>"}` — an agent's terminal at full size.

Common optional fields on any node: `id`, `size` (`{"px": n}` or
`{"weight": n}`), `min`, `max`, `padding` (a number or `[vertical,
horizontal]`), `gap`, `hidden`, `title`, `background`, `border`, `radius`,
`shadow` (`sm`/`md`/`lg`), `focus`, `divider`, `movable`, `closable`,
`resizable`, `window_drag`, and `floating` (blocks snapped to an edge, each
`{id, content, edge?, align?, peek?, hidden?}`).

A pane frame (`content.tab.pane`) styles a context's panes: `gap`, `radius`,
`background`, `border`, `focus`, `header` (a header mod or the builtin `"tabs"`
strip) and `lone` (frame even an unsplit tab). A `background` is a theme color
name/hex string, `{gradient: [a, b], angle?}`, or `{image: <path>, fit?}`.

## Storage

The document lives in herdr-core and is exchanged with the generation-1 core
methods:

- `doc.get` with `{"name": "herdr-app", "on": <machine>}` returns `{value,
  version}`. A missing document is the empty `{"spaces": []}` with version 0.
- `doc.set` with `{"name": "herdr-app", "on": <machine>, "version": <version>,
  "value": <document>}` returns the new `{version}`.

`version` is a compare-and-swap token: a write is accepted only when it names
the version it read. A stale version fails with the core error kind `conflict`
and the caller re-reads before retrying. There is no partial update; the whole
document is replaced.

## Machine qualification

Documents are per machine. Identifiers inside the document are local to it:
space, tab, node, terminal, agent and float ids are stored without the machine
prefix. Applying a document to the local model qualifies them as
`<machine>/<id>`; capturing a document strips the prefix again. A document
never stores two machines' layouts in one place, so an id is only meaningful
together with the machine it was read from. Ids are opaque strings (never
parsed, other than the prefix) and may be empty for a node the app has not
named.

## Private view

`active` (the active space), per-space and per-tab selection, `focused`,
`before`, `zoomed`, `settled`, `recent` and per-agent seen state (`attention`)
are the Desktop window's view state. They are per screen, never shared, and
are **not** part of this document. A document is safe to hand to another
window or to persist without leaking or overwriting what anyone was looking
at.

## Unknown fields

- The document object, a space and a tab are closed: an unknown field is a
  protocol error, so a writer cannot smuggle view state or another machine's
  data into them.
- A node, a float and their content allow additional properties. herdr adds
  optional look fields between releases, and a document written by a newer
  build must still be accepted (and preserved on round-trip) by an older one.
  Consumers ignore fields they do not understand.

## Fixtures

- `fixtures/document.json` — two spaces: a split tab with two terminals and a
  mod pane plus a floating mod, and a second space showing an agent. It must
  validate against `document.schema.json`.
