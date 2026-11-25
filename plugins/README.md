# Plugins

Edict doesn't install any plugins by itself. Each one is a standalone folder that you
copy (or symlink) into `~/.config/edict/plugins/`, then click
**Settings → Plugins → Rescan**.

This folder holds:

| Path | What |
|---|---|
| [`esmfold2/`](esmfold2/plugin.py) | Protein structure prediction with ESMFold2. See [docs/esmfold2.md](../docs/esmfold2.md). |
| [`edict.py`](edict.py) | The helper module the app gives every plugin run (`import edict`). Part of the app, not a plugin: don't copy it. |

To write a plugin, see [docs/plugins.md](../docs/plugins.md).
