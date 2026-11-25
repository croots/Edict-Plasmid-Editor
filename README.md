# Edict

Edict is a fast, GPU-rendered editor for plasmids and other DNA sequences, built on [GPUI](https://crates.io/crates/gpui) (Zed's UI framework). It opens GenBank, FASTA and SnapGene files and saves to its own `.edna` format, which keeps a full edit history and attached 3D structures alongside the sequence.



https://github.com/user-attachments/assets/ce45c983-ef63-4e29-ab27-d03fb6dc3b7c



Edict is free for noncommercial academic use. See [License](#license).

## Run

```sh
nix develop                     # Rust toolchain, Zig 0.17 (for libghostty-vt), GPUI's Vulkan/Wayland/X11/font libraries
cargo run --release -- samples  # explorer root; optional second arg opens a file
```

More documentation is in [`docs/`](docs/README.md): [writing plugins](docs/plugins.md),
[ESMFold2 structure prediction](docs/esmfold2.md) and [the `.edna` format](docs/edna-format.md).

## What it does

- **Explorer** (left): a folder tree showing native `.edna`, GenBank (`.gb .gbk .genbank .gbff .ape`), FASTA and SnapGene `.dna` files. Click to open. Files are recognised by content first (zip, SnapGene header, `>`), then by extension.
- **Tabs** (top): one per open file. A `●` marks unsaved changes. Close with `×` or a middle-click.
- **Sequence map**: double-stranded wrapped rows, a ruler (ticks every 10, labels every 50) and feature lanes packed per row, with strand arrows. The rows are a virtualized `gpui::list` with variable heights, so the 48 kb lambda genome scrolls smoothly.
- **Editing**: click or drag to select, Shift+click to extend, and click a feature bar to select it. Type `A C G T N…` (any IUPAC code, either case: lower case is kept, as is mixed case in files) to insert or replace, and use Backspace/Delete to remove. Features shift and resize with edits.
- **Keys**: Ctrl+Z/Ctrl+Shift+Z undo/redo, Ctrl+C/X/V clipboard (pasted text is filtered to bases), Ctrl+Shift+C copies the reverse complement, Ctrl+A selects all, Ctrl+S saves (GenBank keeps qualifiers), Ctrl+=/− zoom, arrows/Home/End/PageUp/PageDown to move.
- **Split panes**: each tab has its own pane layout. Split the active pane right (Ctrl+\\) or down (Ctrl+Shift+\\), or use the buttons in a pane's header. Splits nest. Drag the bar between two panes to resize them. Drag a pane by its header onto another pane to move it: drop near an edge to dock it on that side (left, right, top, bottom), or in the middle to swap the two. A highlight shows where it will land. Close a pane with its `×` or Ctrl+Shift+W. The sequence pane always stays open. The split button's menu picks what the new pane shows: a map, a terminal, a 3D structure or (for `.edna` files) the edit history; the keyboard shortcuts open a map.
- **DNA map**: features drawn with [SBOL Visual](https://sbolstandard.org/visual-glyphs/) glyphs, either **Circular** or **Linear** per pane (the toggle in the pane header; a new map follows the file's topology).
  - Glyphs: CDS, promoter, RBS, terminator, origin of replication, origin of transfer, primer binding site, operator, insulator, recombination site, polyA, ncRNA, engineered region, and "no glyph assigned". `regulatory` features use their `/regulatory_class`.
  - Placement: baseline glyphs (promoter, RBS, terminator, …) sit on the backbone and centered ones (CDS, origin, …) straddle it. Reverse-strand glyphs are rotated 180°. Overlaps stack into lanes, outward for the forward strand and inward for the reverse.
  - Circular maps put labels outside the circle with leader lines. On crowded maps the glyphs shrink and long labels are shortened.
  - Linear maps label features inline. When that doesn't fit the pane they keep only the labels that fit over their feature, and the pane opens scrolled to the backbone.
  - Sequence-record annotations (`variation`, `conflict`, `old_sequence`, …) are left off the map but still appear in the sequence pane.
  - Clicking a glyph selects the feature, scrolls the sequence to it and names it in the status bar. Clicking or dragging along the backbone sets the cursor or a selection. The selection and cursor are shared with the sequence pane.
- **Status bar**: length, topology, cursor position or selection range and length, and GC% of the selection.
- **Terminal pane**: your `$SHELL` in the open file's folder, emulated by [libghostty-vt](https://lib.ghostty.org/vt/) (Ghostty's terminal core) on a pty from `portable-pty`. It uses the app theme, including its ANSI colors (base16 schemes map onto them the way base16-shell does), sets `TERM=xterm-ghostty` with Ghostty's terminfo, and shows the shell's title in the pane header. While it has focus, Ctrl+W/E/N/O/Q/\\ and Escape go to the shell; the Ctrl+Shift shortcuts still work. Ctrl+Shift+V pastes (bracketed when the program asks), Ctrl+= / Ctrl+− change the text size and Ctrl+0 resets it, the wheel or Shift+PageUp/PageDown scroll the scrollback (full-screen programs get arrow keys). When the shell exits, Enter starts a new one. Mouse reporting and text selection are not done yet.
- **History pane** (`.edna` files only): one entry per save, newest first, with unsaved changes on top. Each entry shows its net effect (typing bases and deleting them again cancels out, as does an undone change), who made it and when: "Inserted 120 bp at 1,234 · Added CDS “lacZ”". Click an entry to select its net change in the sequence and expand it into the individual changes: bases inserted or deleted, features added, removed or edited, structures attached, removed or relinked, and renames, each with its revision, author and time. Clicking a change selects where it is in the sequence today, carried forward through every later edit; a deletion selects the gap it left. Right-click an entry to revert to that save, to before it, or (on unsaved changes) to discard them; right-click a change to revert to just before or just after it, or the "Start of history" line to go back to the start. A revert puts back the bases and features, is one undo step, and is logged as "Reverted to revision N" with the changes it made; attached structures stay as they are. Changes are credited to the `author` setting, or to your login name if it is unset.
- **BLAST** (right-click a selection → BLAST…): searches NCBI with megablast, discontiguous megablast, BLASTN, BLASTX or TBLASTX. The dialog offers:
  - the database, organisms to include or exclude (names or taxids, separated by commas), and whether to leave out models (XM/XP) and uncultured samples
  - the E-value threshold, maximum targets and word size
  - match/mismatch scores or a matrix and genetic code, gap costs, and compositional adjustment (BLASTX)
  - filters: low complexity, species-specific repeats, lookup-table-only masking, lower-case masking, short-query adjustment
  - a job title

  Defaults follow the program, and the options are remembered between searches.
  - **Running**: while it runs, the dialog shows a log; **Background** keeps it running behind you, and **Cancel** stops it. Only one search runs at NCBI at a time; later ones queue. Requests follow NCBI's rules: at least 10 s apart, a status check at most once a minute, and the first check only after NCBI's own estimate.
  - **Results** are a table sortable by any column (E-value first): description, organism, max/total score, query cover, E-value, identity, length and accession. Click a hit to see its alignments drawn NCBI-style, 60 columns of Query/midline/Sbjct, with nucleotide coordinates stepping by 3 and the frame for translated searches. Links open the record and the full NCBI report.
  - **Create feature from match…** opens the usual Create Feature dialog filled in:
    - span and strand from the match (split at the origin if needed)
    - `CDS` with `/product`, `/codon_start` and `/transl_table` for BLASTX, otherwise `misc_feature`; a BLASTX CDS takes in the stop codon right after the match, read with that genetic code
    - `/inference` naming the accession
    - an editable `/note`: "Identified by BLASTX against nr on 2026-11-23 by ‹author›: …, 98.8% identity over 240 aa, E-value 3e-150."

  Results live in memory only. NCBI keeps its own report for about 36 hours.
- **Plugins** (right-click a feature → Plugins): Python scripts that act on a feature, such as predicting a protein's structure. Edict installs none: each plugin is a standalone folder you add yourself (this repository keeps one, [ESMFold2](docs/esmfold2.md)). Each lives in `~/.config/edict/plugins/<id>/plugin.py` and says in its header which Python environments it can run in (uv, venv, conda, nix or a plain interpreter), what it offers, and its settings. Running one works like BLAST:
  - a settings form, remembered between runs
  - the plugin's live terminal output, which you can background or cancel (cancelling stops its child processes too)
  - a result card with its scores (pTM, pLDDT) and a per-residue confidence strip, from which you **Attach** the structure to the feature (it opens in a 3D pane, coloured by confidence), **Save** the file, or dismiss it

  A memory guard stops a run before the machine runs out of RAM. **Settings → Plugins** lists the installed plugins, which environment each will use (and lets you pick another), and any that failed to load. [`docs/plugins.md`](docs/plugins.md) explains how to write one.
- **Locked regions**: the bases a task is using stay locked until its results are dismissed, or until it fails or is cancelled. This keeps results from drifting out of step with the sequence.
  - Locked bases are tinted with a bar above them, and shown as a band on maps.
  - Typing, deleting, pasting, cutting, undo/redo and reverts that would change them are refused, with a note in the status bar.
  - Edits elsewhere move the lock with the bases, and reindexing rotates it. Features can still be edited.
  - Clicking locked bases (or the band on a map) brings their task forward; where locks overlap, a menu asks which. A drag that starts outside still selects through them.
- **Tasks** (bottom of the explorer): running, queued and finished tasks. Click one to show it (its file's tab comes forward too). `×` cancels a running task and clears a finished one. Escape sends the shown task to the background. A task that finishes out of view is highlighted until you open it, and if the window isn't focused a desktop notification says so. Closing a tab or quitting asks first if tasks would be lost.
- **Menu** (☰, top left of the explorer): New File (Ctrl+N), Open File… (Ctrl+O), Open Folder… (Ctrl+Shift+O), Save (Ctrl+S), Save As… (Ctrl+Shift+S), Export As… (Ctrl+E), Close Tab (Ctrl+W), Toggle Full Screen (F11; also ⌃⌘F on macOS), Settings… (Ctrl+,), About… (version and third-party licenses), Quit (Ctrl+Q; asks first if files are unsaved). New files are in memory until their first save, which asks where to put them. The extension you pick (`.edna`, `.gb` or `.fa`) sets the format; a name without one becomes `.edna`. Export As writes a copy in any writable format while the tab keeps editing the original.
- **Settings** (Ctrl+,): the **Appearance** page picks a theme and adds custom base16 schemes; the **Hotkeys** page lists every keyboard and mouse shortcut, grouped by where it applies (general, panes, sequence editor, terminal, 3D structure); the **Keys** page holds the email address BLAST sends to NCBI (with `tool=edict`), as NCBI's usage rules ask. BLAST won't run without it. The **Plugins** page is described above.
- **Saving other formats**: Ctrl+S on a GenBank, FASTA or SnapGene file asks first. **Convert** writes `name.edna` beside the original, leaves the original untouched and switches the tab to the new file. **Keep** saves in the original format and warns what is lost: edit history for GenBank, plus features and topology for FASTA. Keep is not offered for SnapGene, which is read-only. The choice lasts for the tab. Choosing a format by name in Save As counts as a choice and does not ask.
- **Settings → Theme**: the built-in light palette plus every base16 scheme from [tinted-theming/schemes](https://github.com/tinted-theming/schemes) (the successor to `chriskempson/base16-schemes-source`), with swatches and Dark/Light/Custom filters. Clicking applies it live.
- **Settings → Custom base16 themes**: add a scheme `.yaml` file or a folder of them. Both the current tinted-theming layout (`palette: { base00: "#…" }`) and the original chriskempson layout (`base00: "…"`) parse. Reload picks up edits. Settings are kept in `$XDG_CONFIG_HOME/edict/settings.conf` (`~/.config/…` by default). Add `author = Your Name` there to choose the name the edit history records.

## Layout

| File | Role |
|---|---|
| `src/model.rs` | Document: format detection, parsing (gb-io / FASTA / SnapGene / `.edna`), insert/delete with feature remapping, undo, the persistent edit log, save and export. Unit-tested. |
| `src/edna.rs` | The native `.edna` zip format: layout, compatibility rules, size-capped reading, unknown entries kept on save. Unit-tested. |
| `src/snapgene.rs` | Read-only SnapGene `.dna` import (sequence, topology, features, primers, notes). Unit-tested. |
| `src/layout.rs` | Pure wrapping and lane packing in base units. Unit-tested. |
| `src/editor.rs` | GPUI view: paints rows with `canvas`, hit-testing, keyboard. |
| `src/panes.rs` | Per-tab split tree: leaves are the sequence view, a map, a 3D structure, a terminal or the edit history, splits hold an axis and a ratio, the dividers drag to resize, and headers drag to move panes. |
| `src/sbol.rs` | SBOL Visual glyphs: GenBank feature key → glyph, and each glyph's outline in a local box. Unit-tested. |
| `src/map_layout.rs` | Pure placement of glyphs along a backbone, with lanes that wrap the origin on circular maps. Unit-tested. |
| `src/map.rs` | GPUI map view: linear/circular projection, painting, labels, click and drag. |
| `src/workspace.rs` | Explorer tree, tab strip, active editor, hamburger menu, settings panel. |
| `src/terminal.rs` | Terminal pane: pty and reader thread, libghostty-vt render state painted as a cell grid, key encoding, scrollback. Unit-tested. |
| `src/history.rs` | History pane: the change log grouped by save, each save's net effect (found by replaying the log backwards), finding where an old change is in the current sequence, and the right-click reverts. Unit-tested. |
| `src/notify.rs` | Desktop notifications (D-Bus on Linux, `osascript` on macOS) for tasks that finish while the window is in the background. |
| `src/tasks.rs` | Background tasks, shared by built-in features and plugins: a runner on its own thread, per-service limits (one at a time for NCBI) with a FIFO queue, a log, cancelling, region locks and which task is in front. Unit-tested. |
| `src/locks.rs` | Locked regions: how edits move them, which edits they refuse, and rotation on reindex. Unit-tested. |
| `src/blast/mod.rs` | The NCBI BLAST URL API: options and their per-program defaults, the Put form, RID/status parsing, JSON2_S results, request throttling. Unit-tested; `cargo test live_search -- --ignored` with `BLAST_EMAIL` set runs a real search. |
| `src/blast/results.rs` | Hit table sorting, NCBI-style alignment blocks, and the feature a match becomes. Unit-tested. |
| `src/blast/dialog.rs` | The BLAST task's view: options form, run log, results table and alignments. |
| `src/plugins/mod.rs` | The plugin registry: scanning the plugins folder, the actions offered for a feature kind. Unit-tested. |
| `src/plugins/manifest.rs` | Reading a plugin's `[tool.edict]` header: environments, actions, typed settings. Unit-tested. |
| `src/plugins/env.rs` | Turning an environment entry (uv, venv, conda, nix, python) into a command, or saying why it isn't usable. Unit-tested. |
| `src/plugins/runner.rs` | Running a plugin as a task: scratch folder, pty, the event stream, cancelling the process group, checking the result. Tested against real Python when it is on `PATH`. |
| `src/plugins/form.rs`, `dialog.rs` | The settings form built from the manifest, and the plugin task's view: form, terminal, result card, attach and save. |
| `plugins/` | The `esmfold2` plugin (install it yourself; see [docs/esmfold2.md](docs/esmfold2.md)) and the `edict.py` helper the app gives every run. |
| `src/about.rs` | About panel: version, third-party notices and the crate license table. |
| `licenses/` | License texts shown in About. `rust-crates.tsv` lists every linked Rust crate; regenerate it with `scripts/licenses.sh` after changing dependencies. |
| `src/theme.rs` | Active theme (thread-local), base16 → UI color mapping, YAML scheme parser, bundled and custom theme loading. Unit-tested. |
| `src/settings.rs` | Load and save the settings file. |
| `themes/base16/` | The bundled schemes (MIT, tinted-theming/schemes). `build.rs` embeds every `.yaml` here into the binary, so to update them copy a newer `base16/` folder over this one. |
| `vendor/xattr` | gpui 0.2.2 pulls in `xattr 0.2.3`, which no longer compiles against current `libc` (`ENOATTR`). This copy changes only that line to `ENODATA`, the same value on Linux. |

The font defaults to DejaVu Sans Mono (Menlo on macOS). Override it with `EDICT_FONT="JetBrains Mono"`.

## Not done (next steps)

Map zoom and pan, translations and ORFs, restriction sites, feature editing UI, the explorer's search box (GPUI needs a hand-built text input), multi-record files, alignments (the `.edna` format reserves `alignments/` and ties each one to a revision, so edits mark it stale), autoscroll while drag-selecting past the viewport edge.

BLAST: these parameters are what NCBI's web form and other clients send, but NCBI's URL API page doesn't list them, so treat them as unverified: `ENTREZ_QUERY` (organism limits), `EXCLUDE_MODELS`, `EXCLUDE_SEQ_UNCULT`, `LCASE_MASK`, `QUERY_GENETIC_CODE`/`GENETIC_CODE`, `JOB_TITLE`, the discontiguous-megablast `TEMPLATE_TYPE`/`TEMPLATE_LENGTH`, the `R` (repeats) filter letter, and polling with `FORMAT_OBJECT=SearchInfo`. Check them with a live search.

`samples/sample-d.dna` is a test file from [Biopython](https://github.com/biopython/biopython/tree/master/Tests/SnapGene) (Biopython License / BSD 3-Clause).

## License

Edict is source-available under the [Edict Academic Noncommercial License](LICENSE). In short: students, researchers and staff at universities and nonprofit research institutions, and individuals learning on their own, may use, modify and redistribute Edict free of charge for noncommercial academic teaching and research, as long as the license and copyright notice go with it and nothing is charged for it. Commercial use, including any use by or for a for-profit company, needs a separate license from the copyright holder.

This summary is for convenience; the [LICENSE](LICENSE) file is what governs. Third-party components keep their own licenses (see `licenses/` and the About panel).
