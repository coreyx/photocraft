# Process plug-ins

A process plug-in is a separate program that PhotoCraft starts and talks to over its standard
input and output. It can read and change the open documents, draw panels and dialogs with
PhotoCraft's own widgets, add menu commands and shortcuts, and keep data in documents. Because it
runs in its own process, it can be written in any language, under any license, and a crash or a
hang never takes PhotoCraft down with it.

Process plug-ins run on the desktop builds only. The web build has none.

- [Installing and the manifest](#installing-and-the-manifest)
- [Lifecycle](#lifecycle)
- [Frames](#frames)
- [Messages](#messages)
- [Documents and layers](#documents-and-layers)
- [Pixels, selections and masks](#pixels-selections-and-masks)
- [Change events](#change-events)
- [Data kept in documents](#data-kept-in-documents)
- [Vector content](#vector-content)
- [Previews](#previews)
- [Panels, dialogs and widgets](#panels-dialogs-and-widgets)
- [Actions](#actions)
- [Services](#services)
- [Errors](#errors)
- [Security model](#security-model)

## Installing and the manifest

A plug-in is a folder in PhotoCraft's `process-plugins` folder, next to its settings. Edit ›
Preferences › Plug-ins installs one from a `.zip` package (the folder's contents), and lists the
installed ones with their state, to restart, turn off or remove.

Every plug-in folder holds `photocraft-plugin.toml`:

```toml
id = "org.example.my-plugin"     # reverse-DNS, lowercase: letters, digits, '.', '-', '_'
name = "My Plug-in"
version = "1.2.0"
protocol = 1                     # the protocol version it speaks
license = "MIT"
description = "What it does."    # optional
command = ["bin/my-plugin", "--quiet"]   # program inside the folder, then its arguments
```

The program in `command` must be inside the folder: an absolute path, `..` or a bare name looked
up on `PATH` is refused. Unknown keys are refused too, so a typo is an error, not a silent default.

## Lifecycle

PhotoCraft starts every enabled plug-in on its first frame, with the folder as the working
directory and these environment variables:

| Variable | Value |
|---|---|
| `PHOTOCRAFT_PLUGIN_NONCE` | A random value the plug-in's `hello` must echo |
| `PHOTOCRAFT_PLUGIN_DIR` | The plug-in's installed folder |
| `PHOTOCRAFT_PLUGIN_DATA_DIR` | Where it keeps its settings and data (created for it) |
| `PHOTOCRAFT_PLUGIN_LOG_DIR` | Where it may write logs (created for it) |
| `PHOTOCRAFT_LOCALE` | The UI language, a BCP 47 tag such as `en` or `ja` |
| `PHOTOCRAFT_HOST_VERSION` | PhotoCraft's version |

On Windows the process gets no console window. Its standard error goes to a rotated log file in
PhotoCraft's `logs/plugins` folder.

1. The plug-in sends `hello` as its first frame, within 10 seconds.
2. PhotoCraft answers with `welcome`, or an error and closes the connection: a wrong nonce is
   `invalid`, another protocol version `unsupported` (the plug-in is then listed as
   incompatible).
3. PhotoCraft sends `ping` every 5 seconds. A plug-in that misses 3 in a row is shown as not
   responding: its panels are greyed and their input waits (at most 256 events, newest kept)
   until it answers again.
4. When PhotoCraft quits, or the user turns the plug-in off or restarts it, PhotoCraft sends
   `shutdown`. A plug-in still running 5 seconds later is ended.

A plug-in that exits on its own, crashes or breaks the protocol is shown as stopped, with a
notice and its exit reason. Its open undo groups end, its previews, dialogs and actions go, and
its panels keep their places in the dock, saying the plug-in isn't running, until it is
restarted. Documents are never touched by a plug-in stopping.

## Frames

Both directions use the same frames:

```text
"PCPP" (4 bytes) | header length (u32, little-endian) | payload length (u64, little-endian) | header | payload
```

The header is UTF-8 JSON. The payload is raw bytes whose meaning the method or topic defines
(pixel rows, a PNG, a blob); most frames have none. Headers are at most 1 MiB and payloads
512 MiB; a frame past either, or one that doesn't start with `PCPP`, closes the connection.

## Messages

A header is one of four kinds, told apart by `type`:

```json
{"type": "request", "id": 7, "method": "layer.tree", "params": {"doc": "3"}}
{"type": "response", "id": 7, "result": [ ... ]}
{"type": "error", "id": 7, "error": {"code": "notFound", "message": "no open document '3'"}}
{"type": "event", "topic": "pixels.changed", "data": {"doc": "3", "revision": 41, "layers": ["…"]}}
```

Requests carry an id chosen by the sender, and the response or error echoes it, so either side
can have several requests in flight. Events need no answer. PhotoCraft never waits for a plug-in
within a frame: a plug-in's requests are answered as they arrive, and what the user does reaches
the plug-in as events.

All names are camelCase. Documents are named by `doc` (a string id, valid while the document is
open), layers by `uid` (a UUID-formatted string that survives saving and reopening).

### Session

| Method | Direction | Params | Result |
|---|---|---|---|
| `hello` | plug-in → host | `{protocol: 1, nonce, plugin: {id, version}}` | `welcome` |
| `ping` | host → plug-in | `{}` | `{}` |
| `shutdown` | host → plug-in | `{}` | `{}`, then exit |

```json
{"type": "request", "id": 1, "method": "hello",
 "params": {"protocol": 1, "nonce": "…", "plugin": {"id": "org.example.my-plugin", "version": "1.2.0"}}}
{"type": "response", "id": 1,
 "result": {"hostVersion": "0.20.1", "protocol": 1, "locale": "en", "dataDir": "…", "logDir": "…", "theme": {"dark": true}}}
```

## Documents and layers

| Method | Params | Result |
|---|---|---|
| `doc.list` | `{}` | `[{doc, name, path?, width, height, dpi, depth: "u8"|"u16"|"f32", mode, active}]` |
| `doc.info` | `{doc}` | one entry of `doc.list` |
| `doc.setActive` | `{doc}` | `{}` |
| `doc.resizeCanvas` | `{doc, width, height, anchor?}` | `{}` (Image › Canvas Size; one undo step) |
| `doc.resizeImage` | `{doc, width, height, dpi?}` | `{}` (Image › Image Size, bilinear; one undo step) |
| `doc.open` | `{path}` | `{doc}` |
| `layer.tree` | `{doc}` | `[{uid, name, kind, visible, locked, opacity, blend, bounds, hasMask, animated, children}]`, bottom to top |
| `layer.active` | `{doc}` | `uid` or `null` |
| `layer.setActive` | `{doc, uid}` | `{}` |
| `layer.create` | `{doc, kind: "pixel"|"group", name, parent?, above?}` | `{uid}` |
| `layer.update` | `{doc, uid, name?, visible?, locked?, opacity?, blend?}` | `{}` |
| `layer.move` | `{doc, uid, parent?, above?}` | `{}` |
| `layer.delete` | `{doc, uid}` | `{}` |
| `layer.addMask` | `{doc, uid, from: "image"|"selection"|"alpha"}` (+ `gray8` canvas-size payload for `image`) | `{}` |
| `layer.thumbnail` | `{doc, uid, width, height}` | `{}` + PNG payload |
| `undo.begin` | `{doc, label}` | `{group}` |
| `undo.end` | `{group}` | `{}` |

Kinds are `pixel`, `group`, `shape`, `text`, `smart`, `adjustment`, `fill`, `video`, `other`. Blend
modes are camelCase (`normal`, `multiply`, `passThrough`, …). `parent` and `above` place a layer:
directly above the sibling `above`, else on top of `parent`'s children, else on top of the
document.

Every change is one undoable step named after it, or part of the plug-in's open undo group on
that document: the edits between `undo.begin` and `undo.end` undo as one step, labelled with the
group's label. A plug-in may change a document that isn't the active one; the window stays where
the user is.

```json
{"type": "request", "id": 12, "method": "undo.begin", "params": {"doc": "3", "label": "Apply Result"}}
{"type": "response", "id": 12, "result": {"group": 1}}
{"type": "request", "id": 13, "method": "layer.create", "params": {"doc": "3", "kind": "pixel", "name": "[Generated] a fox"}}
{"type": "response", "id": 13, "result": {"uid": "5f0c…"}}
{"type": "request", "id": 14, "method": "undo.end", "params": {"group": 1}}
```

## Pixels, selections and masks

| Method | Params | Result |
|---|---|---|
| `pixels.read` | `{doc, source, rect, format, exclude?: [uid], includePreview?: false}` | `{}` + pixel payload |
| `pixels.write` | `{doc, uid, rect, format}` + pixel payload | `{}` |
| `pixels.createLayer` | `{doc, name, parent?, above?, rect, format}` + pixel payload | `{uid}` |
| `selection.read` | `{doc, rect}` | `{}` + `gray8` payload, or `{none: true}` |
| `selection.bounds` | `{doc}` | `{bounds?}` |
| `selection.set` | `{doc, rect}` + `gray8` payload, or `{doc, clear: true}` | `{}` |
| `mask.read` | `{doc, uid, which: "alpha"|"layerMask", rect}` | `{}` + `gray8` payload |

`source` is `{"kind": "layer", "uid": …}` (the layer's own pixels, without its opacity, mask or
effects), `{"kind": "group", "uid": …}` (the group's composite) or `{"kind": "composite"}` (the
document, with the `exclude` layers left out as if hidden). `rect` is
`{x, y, width, height}` in canvas pixels.

Payloads are rows top to bottom with no padding, straight (not premultiplied) alpha:

| Format | Bytes per pixel |
|---|---|
| `bgra8` | 4 (blue, green, red, alpha) |
| `rgba8` | 4 |
| `rgba16` | 8, little-endian |
| `rgba32f` | 16, little-endian floats |
| `gray8` | 1, coverage 0–255 (selections and masks only) |

Colour pixels are always sRGB. PhotoCraft converts from and to each document's depth, colour
mode and profile through its own colour engine, the way the canvas displays it; an 8-bit sRGB
document needs no conversion, so its round trip is exact. Bitmap, Indexed, Multichannel and
Duotone documents are refused as `unsupported`.

## Change events

`events.subscribe {topics: [...]}` chooses which document topics a plug-in receives; an empty
list unsubscribes. Once per frame PhotoCraft compares the documents with the previous frame and
sends at most one event per kind per document:

| Topic | Data |
|---|---|
| `document.opened`, `document.closed`, `document.saved`, `document.renamed`, `document.activated` | `{doc, name, path?}` |
| `layers.changed` | `{doc, revision, tree}` (the whole tree, as `layer.tree`) |
| `pixels.changed` | `{doc, revision, layers: [uid], bounds?, origin?}` |
| `selection.changed` | `{doc, bounds?}` |
| `layer.activated` | `{doc, uid?}` |

`origin` names the plug-in whose own request made the change. A brush stroke is a pixel change,
not a tree change: layer bounds don't count towards `layers.changed`, but its tree carries them.

These events reach the plug-in they concern without a subscription:

| Topic | Data |
|---|---|
| `panel.input`, `dialog.input` | `{owner, nodeId, kind, value, modifiers: {shift, ctrl, alt, meta}}` |
| `dialog.closed` | `{dialogId}` |
| `action.triggered` | `{id, doc?}` |
| `progress.cancel` | `{taskId}` |
| `locale.changed` | `{locale}` |

## Data kept in documents

| Method | Params | Result |
|---|---|---|
| `blob.set` | `{doc, name}` + bytes | `{}` |
| `blob.get` | `{doc, name}` | `{found}` + bytes |
| `blob.delete` | `{doc, name}` | `{}` |
| `blob.list` | `{doc}` | `{names}` |
| `layerData.set` | `{doc, uid, key, value?}` (`null` removes it) | `{}` |
| `layerData.get` | `{doc, uid, key}` | `{value?}` |

Each plug-in sees only its own names. Blobs are saved in `.pcraft` and in PSD and PSB (image
resource 4242, "PhotoCraft plug-in data"); layer values in `.pcraft` and in each layer's XMP. They
are kept whether or not the plug-in is installed, and a format that can't hold them warns on save.
Storing data marks the document modified but is not an undo step, and undoing other edits keeps
it. A document holds at most 256 MiB of blobs, and a layer at most 1 MiB of values.

## Vector content

| Method | Params | Result |
|---|---|---|
| `vector.createFromSvg` | `{doc, name, parent?, above?, svg}` | `{uid}` |
| `vector.shapes` | `{doc, uid}` | `{shapes: [{id, handle, svg}]}` |
| `vector.addSvg` | `{doc, uid, svg}` | `{}` |
| `vector.removeShapes` | `{doc, uid, ids: [handle]}` | `{}` |

The SVG's `<circle>` and `<line>` elements become shape layers in a group, one per element, which
the user moves with the Move and Path Selection tools and copies with Duplicate Layer.
`vector.shapes` reads them back as SVG fragments with their current geometry. A duplicated
element keeps its SVG `id`; its `handle` (its layer's uid) is always its own.

## Previews

| Method | Params | Result |
|---|---|---|
| `preview.show` | `{doc, label, rect, format, above?, hatchOutside?}` + pixel payload | `{}` |
| `preview.hide` | `{doc}` | `{}` |

A preview is drawn on the canvas as a locked layer above `above` (or on top), optionally with a
diagonal hatch over the canvas outside `hatchOutside`. It is not part of the document: no undo
step, never saved, can't be selected or painted on. The Layers panel shows it as a dimmed locked
row. It goes when hidden, when its document closes or stops being the active one, and when the
plug-in stops. One preview per document; a newer one replaces it.

## Panels, dialogs and widgets

| Method | Params | Result |
|---|---|---|
| `panel.register` | `{panelId, title, icon?, tree}` | `{}` |
| `panel.patch` | `{panelId, ops: [patch]}` | `{}` (all the patches or none) |
| `panel.unregister` | `{panelId}` | `{}` |
| `image.put` | `{hash, encoding: "png"|"svg"}` + bytes | `{}` |
| `dialog.open` | `{dialogId, title, modal, tree}` | `{}` |
| `dialog.patch` | `{dialogId, ops}` | `{}` |
| `dialog.close` | `{dialogId}` | `{}` |
| `dialog.message` | `{kind: "information"|"question"|"warning"|"critical", title, text, buttons}` | `{button?}`, once the user has chosen |
| `dialog.file` | `{mode: "open"|"save"|"folder", title?, filters?: [{name, extensions}], start?}` | `{path?}`, once the user has chosen |

A panel is a dock panel like the built-in ones: it moves, tabs, floats and closes, has a Window
menu entry, and is saved with the workspace. A tree is a node `{id, kind, props, children}`; ids are
unique within the tree, at most 64 levels deep and 20,000 nodes. Patches:

```json
{"op": "replace", "node": { ... }}
{"op": "set", "id": "prompt", "props": {"text": "a fox", "tooltip": null}}
{"op": "insert", "parent": "regions", "index": 2, "node": { ... }}
{"op": "remove", "id": "region-3"}
{"op": "move", "id": "region-1", "parent": "regions", "index": 0}
```

The widget kinds and their props are listed on `photocraft_process_plugin_protocol::widget::WidgetKind`:
layouts (`row`, `column`, `grid`, `form`, `group`, `collapsible`, `tabs`, `stack`, `scroll`,
`splitter`, `spacer`), display (`label`, `icon`, `image`, `progress`, `separator`), input
(`button`, `toolButton`, `toggleButton`, `splitButton`, `checkbox`, `switch`, `radioGroup`,
`combo`, `slider`, `spin`, `sliderSpin`, `lineEdit`, `textEdit`, `pathField`), collections
(`list`, `thumbGrid`, `repeater`) and overlays (`popup`, `menu`). Common props are `visible`,
`enabled`, `tooltip` and `grow`. Images and icons are `"image:<hash>"` (sent once with
`image.put`) or `"icon:<name>"` (PhotoCraft's own icons). `widget::gallery()` is a tree with one
widget of every kind.

A change the user makes shows at once and is sent as an input event; it stays until the plug-in
sets that prop again, which confirms or corrects it.

A `textEdit`, `list` or `thumbGrid` can claim keys with a `keys` prop: a list of chords such as
`"Ctrl+Up"`, `"Shift+Enter"`, `"Tab"` or `"Delete"` (Ctrl is Command on macOS). A claimed key is
sent as a `key` event, its name as the value and the modifiers with the event, instead of doing
what the widget would do with it: a text field takes them while it has focus, a list while the
pointer is over it. A plug-in uses this for its own shortcuts, such as moving through the
`completions` of a text field (and setting `completionIndex`).

## Actions

| Method | Params | Result |
|---|---|---|
| `action.register` | `{id, label, icon?, shortcut?, menu?}` | `{}` |
| `action.update` | `{id, enabled?, checked?}` | `{}` |

An action is a row of Filter › Plug-ins › *plug-in name* (› `menu`, a `/`-separated submenu path),
and so also in the command palette and Edit › Keyboard Shortcuts. Shortcuts use PhotoCraft's
notation (`Cmd+Shift+G`; Cmd is Ctrl on Windows and Linux). The user's own shortcuts for an
action are kept by PhotoCraft and come back when the plug-in registers the action again. Running
an action sends `action.triggered`.

## Services

| Method | Params | Result |
|---|---|---|
| `notice.show` | `{level: "info"|"warning"|"error", text}` | `{}` |
| `progress.set` | `{taskId, fraction?, message, cancellable}` | `{}` |
| `progress.clear` | `{taskId}` | `{}` |
| `open.url` | `{url}` (`http`, `https` or `mailto` only) | `{}` |
| `open.path` | `{path}` (absolute, existing) | `{}` |
| `plugin.update` | `{path}` (absolute, an existing `.zip`) | `{}` |
| `clipboard.get` | `{}` | `{text}` |
| `clipboard.set` | `{text}` | `{}` |
| `events.subscribe` | `{topics}` | `{}` |

Progress shows in the status bar beside job progress, with a Cancel button when `cancellable`
(which sends `progress.cancel`). It locks nothing: the user keeps working on every document.

`plugin.update` installs a newer package of the plug-in that asks, as Preferences › Plug-ins
installs one: after the answer is sent, PhotoCraft stops the plug-in, replaces its folder and
starts it again, and tells the user. The package's manifest must have the asking plug-in's id;
a package for any other plug-in is refused, so a plug-in can update itself and nothing else.
Checking where the package came from (for example its checksum) is the plug-in's job.

## Errors

| Code | Meaning |
|---|---|
| `notFound` | A document, layer, panel or other object named doesn't exist (any more) |
| `invalid` | Malformed or out-of-range parameters |
| `unsupported` | Understood but not supported here: an unknown method, a colour mode that can't be converted |
| `busy` | The target is busy (a file dialog already open); retrying may work |
| `tooLarge` | More data than allowed |
| `cancelled` | The request was cancelled |
| `internal` | A bug on the answering side |

## Security model

A process plug-in is a program the user chose to install, and it runs with the user's rights,
like any other program they run. PhotoCraft doesn't sandbox it; what it does guarantee is that a
plug-in can't harm PhotoCraft or the user's documents beyond what the protocol lets it ask for,
and that it can't pass itself off as something else:

- **Only what was installed runs.** The manifest's program must be inside the plug-in's folder.
  Installing unpacks into a staging folder and refuses archives with absolute paths, `..` or
  links, and anything over the size limits.
- **The launched process, and no other.** Each launch gets a random nonce that the first
  message must echo, so another program can't take the connection over.
- **Every message is hostile input.** Frame lengths are checked before anything is allocated;
  every parameter is validated; stale documents and layers, absurd sizes and malformed values are
  typed errors, never a crash. The frame decoder is fuzzed, and every method is tested with
  hostile parameters.
- **PhotoCraft never waits on a plug-in.** Requests are answered as they arrive, input is sent
  as events, and the UI is drawn from PhotoCraft's own copy of the plug-in's widgets, so a slow
  or hung plug-in can't freeze the window.
- **Edits are the user's to undo.** Every document change a plug-in makes is an ordinary undo
  step. Previews and data in documents are kept apart from the document's pixels and history.
- **Plug-ins can't reach each other.** Panels, dialogs, actions, data in documents and vector
  layers are each plug-in's own; asking for another's is `notFound` or `invalid`.
- **Opening things is limited.** `open.url` opens only `http`, `https` and `mailto` links, and
  `open.path` only absolute paths that exist.
