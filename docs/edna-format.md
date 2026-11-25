# The `.edna` format

`.edna` is Edict's native file format. It is a zip archive (like ODF or EPUB), so any
unzip tool can open it:

```text
mimetype               first entry, stored uncompressed: application/vnd.edict.edna+zip
manifest.json          {format: "edna", format_version, app_version, revision}
sequence.json          source of truth: sequence, topology, features (+ qualifiers), record metadata
sequence.gb            GenBank copy for other tools; regenerated on every save and never read back
history/ops.jsonl      one change per line: {rev, time, author?, op, …}: insert|delete {pos, bases},
                       add_feature|remove_feature {ix, feature}, edit_feature {ix, before, after},
                       attach_structure, remove_structure, link_structure, rename, revert {to},
                       features {changed?, all?} (bookkeeping: the features a base edit cut, dropped
                       or re-translated, as they were, so features read back exactly)
history/revisions.json save points: [{rev, time, author?, label?}]
structures/            attached 3D structures
```

- **Revisions.** Every change a save keeps (bases, features, structures, the record name), including undo and redo, gets a revision number and the name of whoever made it. Structure files themselves live under `structures/`, not in the log. Each sequence edit stores the bases it inserted or removed, so the log can be replayed backwards from `sequence.json` to any earlier revision; together with the `features` records, that gives back the exact bases and features of any revision. (Logs written before `features` records existed give back exact bases, and features as close as the log allows.)
- **Format versions.** A reader refuses a file with a newer `format_version` instead of guessing at it. Version 2 added the non-sequence changes and authors to the log; version 1 files still open.
- **Unknown entries.** A reader keeps entries it doesn't recognise and writes them back on save. An older build never strips data a newer one added.
- **Stale data.** Anything tied to coordinates (alignments, later) records the revision it was made against. If that differs from `manifest.json`'s revision, it is stale.
- **Safe reading.** Entries are read in memory and size-capped. Nothing is extracted to disk.

The code is in `src/edna.rs` (the archive) and `src/model.rs` (the document and its log).
