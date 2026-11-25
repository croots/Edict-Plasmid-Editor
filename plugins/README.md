# Writing a plasmid-gpui plugin

A plugin is one Python file, `plugin.py`, in its own folder under
`~/.config/plasmid-gpui/plugins/` (`$XDG_CONFIG_HOME/plasmid-gpui/plugins/`):

```text
~/.config/plasmid-gpui/plugins/
  esmfold2/plugin.py
  ideal-helix/plugin.py
  my-plugin/plugin.py      # plus anything else it needs (a .venv, data files)
```

**Settings → Plugins → Install bundled plugins** copies the two plugins in this folder
there. If you have edited an installed copy, it is kept as `plugin.py.bak` first. After
adding or editing a plugin, click **Rescan**. A plugin that fails to load is listed under
"Not loaded" with the reason.

[`ideal-helix/plugin.py`](ideal-helix/plugin.py) is the smallest complete example. It
needs only the standard library. [`esmfold2/plugin.py`](esmfold2/plugin.py) is a real one.

## The manifest

The file starts with a [PEP 723](https://peps.python.org/pep-0723/) `# /// script`
block. `uv` reads `dependencies` from it, and the app reads `[tool.plasmid]`:

```python
# /// script
# requires-python = ">=3.10"
# dependencies = ["numpy"]
#
# [tool.plasmid]
# id = "my-plugin"            # must be unique among installed plugins
# name = "My plugin"          # shown in menus, the task list and Settings
# api = 1                     # the protocol version below
# version = "0.1"             # optional
# description = "What it does."
#
# [[tool.plasmid.env]]        # where to run, in order of preference (see below)
# kind = "uv"
#
# [[tool.plasmid.action]]
# id = "predict"
# label = "Predict structure…"   # the item under right-click → Plugins
# on = "feature"
# kinds = ["CDS", "ORF"]         # feature kinds; leave out for every kind
# input = "protein"              # the feature, translated with its genetic code
# output = "structure"
#
# [[tool.plasmid.setting]]
# key = "seed"
# label = "Seed"
# type = "int"                # choice | bool | int | float | string
# default = 0
# min = 0                     # int and float only, optional
# help = "Shown under the field."
# group = "Sampling"          # optional heading
# actions = ["predict"]       # optional: only for these actions
# ///
```

A `choice` setting lists its options as `choices = ["a", "b"]`, or with labels and help
text as `choices = [{ value = "fast", label = "Fast", help = "…" }]`. Its `default` is one of
the values. A `bool` shows as a check box.

The settings form opens each time the action runs. It starts with the values used last
time, which are saved under `[plugins.<id>]` in `settings.conf`. Unknown keys in the
manifest are an error, so typos show up in Settings rather than being ignored.

Today the only action the app offers is `on = "feature"`, `input = "protein"`,
`output = "structure"`. The protein is the feature translated without its stop codon. A
feature with a stop codon inside it is refused before the plugin runs. As with BLAST,
the feature's bases are locked from Run until the result is dismissed (or the run fails
or is cancelled), so the structure still matches the feature when it is attached.

## Environments

Each `[[tool.plasmid.env]]` entry is one way to get a Python. The app uses the first one
that is available on this machine. In **Settings → Plugins**, the user can pin one
instead.

| `kind` | Fields | Runs |
|---|---|---|
| `uv` | | `uv run --script plugin.py …`, so uv installs `dependencies` itself |
| `venv` | `path` | `<path>/bin/python`; a relative `path` is relative to the plugin's folder, and `~/` works |
| `conda` | `name` or `prefix` | `conda run -n <name>` (or `mamba` / `micromamba`, whichever is installed) |
| `nix` | `installable` | `nix shell <installable> -c python`; `{flake}` stands for this app's flake |
| `python` | `path` | that interpreter, by path or by name on `PATH` (`python3`) |

`{flake}` is `$PLASMID_FLAKE` if set, otherwise the source tree the app was built from.
This flake's `esmfold2-env` package is the ESMFold2 plugin's first choice.

## The protocol

The app runs `<env> plugin.py run <request.json>` on a terminal (a pty), in a scratch
folder made for the run: `~/.cache/plasmid-gpui/tasks/<id>-…/`. The user sees the
terminal, so print freely; colours and progress bars work. The folder is deleted when the
result is dismissed.

`request.json` looks like this:

```json
{
  "api": 1,
  "action": "predict",
  "settings": {"seed": 0, "model": "fast"},
  "input": {
    "sequences": [{"id": "A", "seq": "MSKGEELF…"}],
    "feature": {"label": "GFP", "kind": "CDS"},
    "doc_name": "pEGFP-N1"
  },
  "workdir": "/home/…/.cache/plasmid-gpui/tasks/…"
}
```

Settings arrive already typed (numbers as numbers, bools as bools). The environment also
has `PLASMID_REQUEST`, `PLASMID_EVENTS`, `PLASMID_WORKDIR` and `PLASMID_API` set, with
`PYTHONUNBUFFERED=1`.

The plugin talks to the app by appending JSON lines to `$PLASMID_EVENTS`. The bundled
helper module writes them for you: `import plasmid` works because the app puts
[`plasmid.py`](plasmid.py) on `PYTHONPATH`, fresh for every run.

```python
import plasmid

req = plasmid.request()
plasmid.progress("Folding", 0.5)      # the line above the terminal; fraction optional
plasmid.warn("no GPU; using the CPU") # printed, and kept as the reason if the run fails
plasmid.fail("sequence too long")     # exits now; the message is the reason shown
plasmid.result(
    kind="structure",
    file="prediction.cif",            # inside the run folder
    format="mmcif",                   # or "pdb"
    chains=["A"],                     # one per input sequence, in order
    source="My model v1, seed 0",     # recorded with the structure
    metrics={"pTM": 0.84},            # shown on the result card
    plddt=[91.2, 88.0],               # per residue, 0–100; drawn as a strip
)
```

A run succeeds when the plugin sends a `result` and exits with status 0. A non-zero exit
fails the run, and the last `error` (or `warn`) becomes the reason shown. Exit status 75
(`plasmid.MEMORY_GUARD_EXIT`) means a memory guard stopped the run on purpose.

The structure is checked before it is offered. It must parse, and its protein chain must
match the protein that was sent. The user can then attach it to the feature, save the
file, or dismiss it. Putting `pLDDT` (or `ESMFold`/`AlphaFold`) in the mmCIF
`_struct.title` makes the 3D view colour by confidence from the B-factor column.

Cancelling sends SIGTERM to the plugin's whole process group, then SIGKILL after 5 s.
Child processes are stopped too.

## Memory

A large model can use up the machine's RAM. When that happens, the kernel's OOM killer
picks a victim, and on a desktop that can be the compositor. The app makes the plugin
the first thing the OOM killer picks (`oom_score_adj` 1000), and the helper offers a
guard that stops the run first:

```python
with plasmid.RamGuard(floor=2 * plasmid.GIB, what="my model"):
    model = load()          # exits with status 75 and a clear message if free RAM
    run(model)              # drops below the floor
```

Start the guard before importing heavy libraries: importing torch and loading weights
are usually where memory use peaks. The ESMFold2 plugin also checks free RAM and disk
before it starts. It sizes its GPU work from free VRAM, and retries with smaller chunks
on `torch.OutOfMemoryError`.

## Trying a plugin outside the app

```sh
cd "$(mktemp -d)" && mkdir .plasmid && cp path/to/plugins/plasmid.py .plasmid/
# write .plasmid/request.json as above, then:
PYTHONPATH=$PWD/.plasmid PLASMID_EVENTS=$PWD/.plasmid/events.jsonl python path/to/plugin.py run .plasmid/request.json
cat .plasmid/events.jsonl
```
