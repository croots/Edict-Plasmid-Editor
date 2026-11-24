# plasmid-gpui

Proof of concept for a Benchling-style plasmid editor built on [GPUI](https://crates.io/crates/gpui) (Zed's UI framework).

## Run

```sh
nix develop                     # Rust toolchain + GPUI's Vulkan/Wayland/X11/font libraries
cargo run --release -- samples  # explorer root; optional second arg opens a file
```

## What it does

- **Explorer** (left): a folder tree showing native `.pgx`, GenBank (`.gb .gbk .genbank .gbff .ape`), FASTA and SnapGene `.dna` files. Click to open. Files are recognised by content first (zip, SnapGene header, `>`), then by extension.
- **Tabs** (top): one per open file. A `●` marks unsaved changes. Close with `×` or a middle-click.
- **Sequence map**: double-stranded wrapped rows, a ruler (ticks every 10, labels every 50) and feature lanes packed per row, with strand arrows. The rows are a virtualized `gpui::list` with variable heights, so the 48 kb lambda genome scrolls smoothly.
- **Editing**: click or drag to select, Shift+click to extend, and click a feature bar to select it. Type `A C G T N…` (any IUPAC code) to insert or replace, and use Backspace/Delete to remove. Features shift and resize with edits.
- **Keys**: Ctrl+Z/Ctrl+Shift+Z undo/redo, Ctrl+C/X/V clipboard (pasted text is filtered to bases), Ctrl+Shift+C copies the reverse complement, Ctrl+A selects all, Ctrl+S saves (GenBank keeps qualifiers), Ctrl+=/− zoom, arrows/Home/End/PageUp/PageDown to move.
- **Split panes**: each tab has its own pane layout. Split the active pane right (Ctrl+\\) or down (Ctrl+Shift+\\), or use the buttons in a pane's header. Splits nest. Drag the bar between two panes to resize them. Drag a pane by its header onto another pane to move it: drop near an edge to dock it on that side (left, right, top, bottom), or in the middle to swap the two. A highlight shows where it will land. Close a pane with its `×` or Ctrl+Shift+W. The sequence pane always stays open. New panes open a map.
- **DNA map**: features drawn with [SBOL Visual](https://sbolstandard.org/visual-glyphs/) glyphs, either **Circular** or **Linear** per pane (the toggle in the pane header; a new map follows the file's topology).
  - Glyphs: CDS, promoter, RBS, terminator, origin of replication, origin of transfer, primer binding site, operator, insulator, recombination site, polyA, ncRNA, engineered region, and "no glyph assigned". `regulatory` features use their `/regulatory_class`.
  - Placement: baseline glyphs (promoter, RBS, terminator, …) sit on the backbone and centered ones (CDS, origin, …) straddle it. Reverse-strand glyphs are rotated 180°. Overlaps stack into lanes, outward for the forward strand and inward for the reverse.
  - Circular maps put labels outside the circle with leader lines. On crowded maps the glyphs shrink and long labels are shortened.
  - Linear maps label features inline. When that doesn't fit the pane they keep only the labels that fit over their feature, and the pane opens scrolled to the backbone.
  - Sequence-record annotations (`variation`, `conflict`, `old_sequence`, …) are left off the map but still appear in the sequence pane.
  - Clicking a glyph selects the feature, scrolls the sequence to it and names it in the status bar. Clicking or dragging along the backbone sets the cursor or a selection. The selection and cursor are shared with the sequence pane.
- **Status bar**: length, topology, cursor position or selection range and length, and GC% of the selection.
- **Menu** (☰, top left of the explorer): New File (Ctrl+N), Open File… (Ctrl+O), Open Folder… (Ctrl+Shift+O), Save (Ctrl+S), Save As… (Ctrl+Shift+S), Export As… (Ctrl+E), Close Tab (Ctrl+W), Settings… (Ctrl+,), Quit (Ctrl+Q; asks first if files are unsaved). New files are in memory until their first save, which asks where to put them. The extension you pick (`.pgx`, `.gb` or `.fa`) sets the format; a name without one becomes `.pgx`. Export As writes a copy in any writable format while the tab keeps editing the original.
- **Saving other formats**: Ctrl+S on a GenBank, FASTA or SnapGene file asks first. **Convert** writes `name.pgx` beside the original, leaves the original untouched and switches the tab to the new file. **Keep** saves in the original format and warns what is lost: edit history for GenBank, plus features and topology for FASTA. Keep is not offered for SnapGene, which is read-only. The choice lasts for the tab. Choosing a format by name in Save As counts as a choice and does not ask.
- **Settings → Theme**: the built-in light palette plus every base16 scheme from [tinted-theming/schemes](https://github.com/tinted-theming/schemes) (the successor to `chriskempson/base16-schemes-source`), with swatches and Dark/Light/Custom filters. Clicking applies it live.
- **Settings → Custom base16 themes**: add a scheme `.yaml` file or a folder of them. Both the current tinted-theming layout (`palette: { base00: "#…" }`) and the original chriskempson layout (`base00: "…"`) parse. Reload picks up edits. Settings are kept in `$XDG_CONFIG_HOME/plasmid-gpui/settings.conf` (`~/.config/…` by default).

## Layout

| File | Role |
|---|---|
| `src/model.rs` | Document: format detection, parsing (gb-io / FASTA / SnapGene / `.pgx`), insert/delete with feature remapping, undo, the persistent edit log, save and export. Unit-tested. |
| `src/pgx.rs` | The native `.pgx` zip format: layout, compatibility rules, size-capped reading, unknown entries kept on save. Unit-tested. |
| `src/snapgene.rs` | Read-only SnapGene `.dna` import (sequence, topology, features, primers, notes). Unit-tested. |
| `src/layout.rs` | Pure wrapping and lane packing in base units. Unit-tested. |
| `src/editor.rs` | GPUI view: paints rows with `canvas`, hit-testing, keyboard. |
| `src/panes.rs` | Per-tab split tree: leaves are the sequence view or a map, splits hold an axis and a ratio, the dividers drag to resize, and headers drag to move panes. |
| `src/sbol.rs` | SBOL Visual glyphs: GenBank feature key → glyph, and each glyph's outline in a local box. Unit-tested. |
| `src/map_layout.rs` | Pure placement of glyphs along a backbone, with lanes that wrap the origin on circular maps. Unit-tested. |
| `src/map.rs` | GPUI map view: linear/circular projection, painting, labels, click and drag. |
| `src/workspace.rs` | Explorer tree, tab strip, active editor, hamburger menu, settings panel. |
| `src/theme.rs` | Active theme (thread-local), base16 → UI color mapping, YAML scheme parser, bundled and custom theme loading. Unit-tested. |
| `src/settings.rs` | Load and save the settings file. |
| `themes/base16/` | The bundled schemes (MIT, tinted-theming/schemes). `build.rs` embeds every `.yaml` here into the binary, so to update them copy a newer `base16/` folder over this one. |
| `vendor/xattr` | gpui 0.2.2 pulls in `xattr 0.2.3`, which no longer compiles against current `libc` (`ENOATTR`). This copy changes only that line to `ENODATA`, the same value on Linux. |

The font defaults to DejaVu Sans Mono (Menlo on macOS). Override it with `PLASMID_FONT="JetBrains Mono"`.

## Not done (next steps)

Map zoom and pan, translations and ORFs, restriction sites, feature editing UI, the explorer's search box (GPUI needs a hand-built text input), multi-record files, alignments (the `.pgx` format reserves `alignments/` and ties each one to a revision, so edits mark it stale), autoscroll while drag-selecting past the viewport edge.

## The `.pgx` format

A zip archive (like ODF or EPUB), so any unzip tool can open it:

```text
mimetype               first entry, stored uncompressed: application/vnd.plasmid-gpui+zip
manifest.json          {format: "plasmid-gpui", format_version, app_version, revision}
sequence.json          source of truth: sequence, topology, features (+ qualifiers), record metadata
sequence.gb            GenBank copy for other tools; regenerated on every save and never read back
history/ops.jsonl      one edit per line: {rev, time, op: insert|delete, pos, bases}
history/revisions.json save points: [{rev, time, label?}]
```

- **Revisions.** Every edit, including undo and redo, gets a revision number. Each edit stores the bases it inserted or removed, so the log can be replayed backwards from `sequence.json` to any earlier revision.
- **Format versions.** A reader refuses a file with a newer `format_version` instead of guessing at it.
- **Unknown entries.** A reader keeps entries it doesn't recognise and writes them back on save. An older build never strips data a newer one added.
- **Stale data.** Anything tied to coordinates (alignments, later) records the revision it was made against. If that differs from `manifest.json`'s revision, it is stale.
- **Safe reading.** Entries are read in memory and size-capped. Nothing is extracted to disk.

`samples/sample-d.dna` is a test file from [Biopython](https://github.com/biopython/biopython/tree/master/Tests/SnapGene) (Biopython License / BSD 3-Clause).
