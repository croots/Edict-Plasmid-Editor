# Writing an Edict plugin

A plugin is a Python script that adds an action to Edict: today, an item under
right-click → **Plugins** on a feature, which takes the feature's protein and gives back
a 3D structure. The app handles the menu, the settings form, the terminal, cancelling,
locking the feature's bases and attaching the result. The plugin only computes.

Plugins are standalone. Edict ships with none installed. Each user decides which ones to
add by putting their folders in the plugins folder. This repository keeps one real
plugin, [`plugins/esmfold2`](../plugins/esmfold2/plugin.py), as a reference and for
people who want it.

- [Quick start](#quick-start)
- [Where plugins live](#where-plugins-live)
- [The manifest](#the-manifest)
- [Settings](#settings)
- [Environments](#environments)
- [How a run works](#how-a-run-works)
- [The request](#the-request)
- [Talking to the app: events](#talking-to-the-app-events)
- [Results](#results)
- [Success, failure and cancelling](#success-failure-and-cancelling)
- [The `edict` helper module](#the-edict-helper-module)
- [Memory](#memory)
- [Testing and debugging](#testing-and-debugging)
- [Sharing a plugin](#sharing-a-plugin)
- [Compatibility](#compatibility)

## Quick start

1. Make a folder for the plugin, named after its id:

   ```sh
   mkdir -p ~/.config/edict/plugins/ca-helix
   ```

2. Save this as `~/.config/edict/plugins/ca-helix/plugin.py`. It needs only the
   standard library, and builds the protein as an ideal helix of Cα atoms:

   ```python
   #!/usr/bin/env python3
   # /// script
   # requires-python = ">=3.9"
   # dependencies = []
   #
   # [tool.edict]
   # id = "ca-helix"
   # name = "Cα helix"
   # api = 1
   # description = "Builds the protein as one ideal helix of Cα atoms."
   #
   # [[tool.edict.env]]
   # kind = "python"
   # path = "python3"
   #
   # [[tool.edict.action]]
   # id = "build"
   # label = "Build Cα helix…"
   # on = "feature"
   # kinds = ["CDS", "ORF"]
   # input = "protein"
   # output = "structure"
   #
   # [[tool.edict.setting]]
   # key = "rise"
   # label = "Rise per residue (Å)"
   # type = "float"
   # min = 1.0
   # max = 3.5
   # default = 1.5
   # ///
   import math

   import edict

   THREE = dict(zip("ARNDCQEGHILKMFPSTWYV",
       "ALA ARG ASN ASP CYS GLN GLU GLY HIS ILE LEU LYS MET PHE PRO SER THR TRP TYR VAL".split()))

   req = edict.request()
   chain = req["input"]["sequences"][0]
   seq, rise = chain["seq"], req["settings"]["rise"]
   print(f"Building {len(seq)} residues of {req['input']['feature']['label']}")
   edict.progress("Building")

   rows = []
   for i, aa in enumerate(seq):
       phi = math.radians(i * 100.0)
       x, y, z = 2.3 * math.cos(phi), 2.3 * math.sin(phi), i * rise
       rows.append(f"ATOM {i + 1} C CA {THREE.get(aa, 'UNK')} A 1 {i + 1} "
                   f"{x:.3f} {y:.3f} {z:.3f} 1.00 0.00 {i + 1} A 1")
   columns = ("group_PDB id type_symbol label_atom_id label_comp_id label_asym_id "
              "label_entity_id label_seq_id Cartn_x Cartn_y Cartn_z occupancy "
              "B_iso_or_equiv auth_seq_id auth_asym_id pdbx_PDB_model_num").split()
   with open("helix.cif", "w") as fh:
       fh.write("data_helix\nloop_\n")
       fh.writelines(f"_atom_site.{c}\n" for c in columns)
       fh.write("\n".join(rows) + "\n#\n")

   edict.result(kind="structure", file="helix.cif", format="mmcif",
                chains=[chain["id"]], source=f"Ideal Cα helix, rise {rise} Å")
   ```

3. In Edict, open **Settings → Plugins** and click **Rescan**. The plugin is listed with
   the environment it will run in.
4. Right-click a CDS → **Plugins → Build Cα helix…**, then **Run**.

## Where plugins live

```text
~/.config/edict/plugins/          ($XDG_CONFIG_HOME/edict/plugins/)
  esmfold2/plugin.py
  ca-helix/plugin.py
  my-plugin/
    plugin.py                     the entry point; the only file the app reads
    .venv/                        anything else the plugin needs: an environment,
    weights.json                  data files, modules it imports
```

**Settings → Plugins → Open folder** opens it (creating it first if needed).

The app scans the folder at startup and when you click **Rescan**. The script itself
is read from disk on every run, so changes to its code apply on the next run. Changes to
the manifest (actions, settings, environments) need a **Rescan**. When it scans:

- Each subfolder holding a file named `plugin.py` is a plugin. Other folders and files
  are ignored.
- Folders are read in name order. If two plugins have the same `id`, the first one
  loads and the second is listed under **Not loaded**.
- A plugin whose manifest has a mistake is listed under **Not loaded** with the
  reason, such as ``in [tool.edict]: unknown field `lable` ``.

The folder name does not have to match the id, but keeping them the same avoids
confusion.

## The manifest

The top of `plugin.py` holds a [PEP 723](https://peps.python.org/pep-0723/) inline
script metadata block. `uv` and other tools read `requires-python` and `dependencies`
from it. Edict reads the `[tool.edict]` table. One readable file says what the plugin
needs and what it does.

### Block syntax

- It starts with a line that is exactly `# /// script` and ends with a line that is
  exactly `# ///`.
- Every line between them is either `#` alone or `# ` followed by TOML. Any other line
  (code, or a comment without the space) means the block was never closed, and the
  plugin doesn't load.
- The block doesn't have to be the first thing in the file. A shebang line before it is
  fine.

### `[tool.edict]`

| Key | Required | Meaning |
|---|---|---|
| `id` | yes | Unique among installed plugins. Lowercase letters, digits, `-` and `_`, at most 64 characters. Settings are saved under it. |
| `name` | yes | Shown in Settings, the task list and the task's title. |
| `api` | yes | The protocol version the plugin was written for. Must be `1`. |
| `version` | no | Free text, shown next to the id in Settings. |
| `description` | no | Shown in Settings. TOML's `"""…"""` strings work for long text. |
| `[[tool.edict.env]]` | no* | Where to get a Python, in order of preference. See [Environments](#environments). *A plugin with none can't run. |
| `[[tool.edict.action]]` | yes | At least one. What the plugin offers and where. |
| `[[tool.edict.setting]]` | no | The settings form. See [Settings](#settings). |

Unknown keys anywhere in `[tool.edict]` are an error, so a typo shows up in Settings
instead of being silently ignored. Other `[tool.*]` tables in the block are left alone.

### `[[tool.edict.action]]`

| Key | Required | Meaning |
|---|---|---|
| `id` | yes | Unique within the plugin, same characters as the plugin id. Sent to the plugin as `request.action`. |
| `label` | yes | The menu item. End it with `…` if it opens a dialog (it always does). |
| `on` | yes | Where it appears. Only `"feature"` (a feature's right-click menu). |
| `kinds` | no | Feature kinds it's offered on, matched without regard to case: `["CDS", "ORF"]`. Leave it out to offer it on every feature. |
| `input` | yes | `"protein"`: the feature translated. `"dna"` is accepted by the manifest for future use, but the app doesn't offer such actions yet. |
| `output` | yes | `"structure"`. |
| `description` | no | Free text. Not shown yet. |

A plugin can have several actions. Each is its own menu item. Settings can be limited to
some actions (see below), and the plugin tells them apart by `request["action"]`.

### What the app does with a `protein` input

The feature's bases are translated with its genetic code (`/transl_table`) and reading
frame (`/codon_start`), across all its segments and on its strand. A first codon that the table allows as a
start reads as M, as GenBank's `/translation` does. Trailing stop codons are dropped. A feature that encodes nothing, or has a stop codon inside it, is refused
with a message in the status bar before the dialog opens.

## Settings

Each `[[tool.edict.setting]]` becomes a field in the dialog that opens when the action
runs.

| Key | Required | Meaning |
|---|---|---|
| `key` | yes | The name the value arrives under in `request["settings"]`. Same characters as ids. `env` is reserved. |
| `label` | yes | The field's label. Also used in validation messages. |
| `type` | yes | `choice`, `bool`, `int`, `float` or `string`. |
| `default` | yes | Must be a valid value for the type (and one of the choices, for `choice`). The plugin doesn't load otherwise. |
| `choices` | for `choice` | `["a", "b"]`, or with labels and help: `[{ value = "fast", label = "Fast", help = "…" }]`. The two forms can be mixed. |
| `min`, `max` | no | Bounds for `int` and `float`, inclusive. |
| `help` | no | Shown under the field. For a choice, each option's `help` shows when it's selected. |
| `group` | no | A heading the setting is shown under ("Memory"). |
| `actions` | no | Only show the setting for these action ids. Every action when left out. Each must name an action in the manifest. |

`choice` shows as a row of chips and `bool` as a check box. The others are text fields.

### What the plugin receives

Values arrive already checked and typed:

| `type` | JSON value |
|---|---|
| `choice` | the chosen option's `value`, a string |
| `bool` | `true` or `false` |
| `int` | an integer |
| `float` | a number (not NaN or infinite) |
| `string` | the text, with surrounding whitespace trimmed |

If a field is invalid, clicking **Run** shows why ("Seed must be a whole number",
"Rise per residue (Å) must be at most 3.5") and nothing starts.

### Remembered values

When the user clicks **Run**, the values are saved under `[plugins.<id>]` in
`~/.config/edict/settings.conf`. The form opens with them next time. A saved value that
no longer fits (because the plugin changed its choices or bounds) is replaced by the
default. The same table holds `env` when the user pins an environment.

## Environments

Each `[[tool.edict.env]]` entry is one way to get a Python. On each run the app uses
the first entry that is usable on this machine, unless the user picked one in
**Settings → Plugins**. The Settings page shows which one will be used, or every reason
none is usable.

| `kind` | Fields | Usable when | Command the script runs under |
|---|---|---|---|
| `uv` | | `uv` is on `PATH` | `uv run --script plugin.py …`. uv installs the block's `dependencies` into a cached environment. |
| `venv` | `path` | `<path>/bin/python` or `bin/python3` exists | that interpreter |
| `conda` | `name` *or* `prefix` | `conda`, `mamba` or `micromamba` is on `PATH` (or `$CONDA_EXE` is set); with `prefix`, the folder has `conda-meta/` | `conda run -n <name> --no-capture-output python` (or `-p <prefix>`) |
| `nix` | `installable` | `nix` is on `PATH` | `nix --extra-experimental-features 'nix-command flakes' shell <installable> -c python` |
| `python` | `path` | the interpreter exists and is executable | that interpreter |

Paths:

- `venv.path`, `conda.prefix` and a `python.path` containing `/` may start with `~/`.
  A relative path is relative to the plugin's folder, so `path = ".venv"` means a
  virtualenv next to `plugin.py`.
- A `python.path` without `/` (`python3`) is looked up on `PATH`.
- In a nix `installable`, `{flake}` stands for Edict's own flake: `$EDICT_FLAKE` if it
  is set and holds a `flake.nix`, otherwise the source tree the app was built from if
  it's still there. The ESMFold2 plugin uses `{flake}#esmfold2-env`.

Good practice is to list the most reproducible option first and a general fallback
last. ESMFold2 lists nix, then a conda env named `esm`, then a `.venv` in its folder,
then uv.

The user's pinned choice is saved as `env = "<kind>:<arg>"` under `[plugins.<id>]`:
`uv`, `venv:<path>`, `conda:<name>`, `conda-prefix:<path>`, `nix:<installable>` or
`python:<path>`.

Startup costs vary. `conda run` and `nix shell` can take several seconds before Python
starts, and the first `uv run` or `nix shell` of a new environment downloads and builds
it, with the progress shown in the terminal.

## How a run works

1. The user picks the action. The app translates the feature and opens a task with
   the settings form.
2. On **Run**, the settings are saved, the feature's bases are **locked** (edits that
   would change them are refused), and the app makes a scratch folder:

   ```text
   ~/.cache/edict/tasks/<plugin-id>-<app-pid>-<task>-<ms>/     ($XDG_CACHE_HOME/edict/…)
     .edict/
       request.json      what to do (below)
       events.jsonl      where the plugin writes events (empty at the start)
       edict.py          the helper module, written fresh for this run
   ```

3. The app runs, on a pseudo-terminal, with the scratch folder as the working
   directory:

   ```text
   <environment command> /path/to/plugin.py run /path/to/.edict/request.json
   ```

   with these environment variables added to the app's own:

   | Variable | Value |
   |---|---|
   | `EDICT_REQUEST` | the path of `request.json` |
   | `EDICT_EVENTS` | the path of `events.jsonl` |
   | `EDICT_WORKDIR` | the scratch folder |
   | `EDICT_API` | `1` |
   | `PYTHONPATH` | `.edict/` first, then any `PYTHONPATH` the app had, so `import edict` works |
   | `PYTHONUNBUFFERED` | `1`, so output appears as it is printed |
   | `TERM`, `COLORTERM` | the terminal's, the same as Edict's terminal pane |

4. The terminal output is shown live in the task's view. Colours, `\r` progress bars and
   tqdm all work. The user can send the task to the background and keep editing.
5. On Linux the process's `oom_score_adj` is set to 1000, so if memory runs out the
   kernel stops the plugin before anything else.
6. When the process exits, the app reads the last events, checks the result, and shows
   a result card: metrics, a confidence strip, and **Attach to ‹feature›**,
   **Save file…** and **Dismiss**. Attaching adds the structure to the document and
   links the feature to its chain.
7. The lock is released and the scratch folder deleted when the result is dismissed
   (after attaching or not), or when the run fails or is cancelled.

Only one plugin run happens at a time, across all plugins, because they tend to want
the whole GPU. Later runs wait in the task list as **Queued**.

## The request

`request.json` (also passed as `argv[2]`):

```json
{
  "api": 1,
  "action": "predict",
  "settings": {"model": "biohub/ESMFold2-Fast", "seed": 0, "device": "auto"},
  "input": {
    "sequences": [{"id": "A", "seq": "MSKGEELFTGVVPILVELDGDVNGHKFSVSG…"}],
    "feature": {"label": "EGFP", "kind": "CDS"},
    "doc_name": "pEGFP-N1"
  },
  "workdir": "/home/you/.cache/edict/tasks/esmfold2-12345-3-1795633787547"
}
```

- `settings` holds only the settings that apply to this action.
- `input.sequences` is a list so that complexes can be sent later. Today it always holds
  one protein with id `"A"`, in one-letter codes, without a stop.
- `input.feature.label` is the feature's label, or its kind if it has none.

## Talking to the app: events

Whatever the plugin prints goes to the user, unparsed. Messages for the app go
separately: the plugin appends one JSON object per line to the file named by
`$EDICT_EVENTS`. The app reads new lines about ten times a second while the plugin runs,
and once more after it exits.

| `event` | Fields | Effect |
|---|---|---|
| `progress` | `text`, `fraction` (0–1, optional) | The status line above the terminal and in the task list: `Folding · 50%`. |
| `log` | `text` | A line in the task's log. |
| `warn` | `text` | A warning in the task's log. |
| `error` | `text` | A warning, and the reason shown if the run fails. The last one wins. |
| `result` | see [Results](#results) | The run's result. The last one wins. |

```json
{"event":"progress","text":"Folding","fraction":0.5}
{"event":"result","kind":"structure","file":"pred.cif","format":"mmcif","chains":["A"]}
```

Write each event with a single `write` of one complete line, in append mode, so the
app never sees half a line. A line the app can't parse is reported as a warning and
skipped. The helper module does all of this for you.

## Results

A `result` event's fields:

| Field | Required | Meaning |
|---|---|---|
| `kind` | yes | `"structure"`. |
| `file` | yes | The structure file, relative to the scratch folder (or absolute but inside it). Paths that lead outside it are refused. |
| `format` | no | `"mmcif"` (the default) or `"pdb"`. |
| `chains` | no | The chain id for each input sequence, in order. |
| `source` | no | Recorded with the attached structure, e.g. `"ESMFold2-Fast, seed 0"`. |
| `metrics` | no | `{"pTM": 0.84, "mean pLDDT": 88.1}`. Numbers are shown on the result card. Values that aren't numbers are ignored. |
| `plddt` | no | Per-residue confidence, 0–100, one per residue. Drawn as a strip on the result card. |

Before it is offered to the user, the structure is checked:

- it must parse as the given format;
- every sequence that was sent must appear as one of its chains, residue for residue
  (letters compared without regard to case). A Cα-only model is enough.

If either fails, the run fails with the reason, so the structure can never be attached
to a feature it doesn't match.

To have the 3D view colour the structure by confidence, write the per-residue values in
the B-factor column (`_atom_site.B_iso_or_equiv`, or columns 61–66 in PDB) and put
`pLDDT`, `ESMFold` or `AlphaFold` in the mmCIF `_struct.title`:

```text
data_prediction
_struct.title 'My model prediction (pLDDT in B-factor)'
```

## Success, failure and cancelling

| What happens | Outcome |
|---|---|
| Exit status 0 after a `result` | Success: the result card. |
| Exit status 0 without a `result` | Fails: "the plugin finished without giving a result". |
| Exit status 75 (`edict.MEMORY_GUARD_EXIT`) | Fails with the last `error`, or "stopped by the memory guard". |
| Any other non-zero status | Fails with the last `error`, or "the plugin exited with status N; its output above may say why". |
| Killed by a signal | Fails with the last `error`, or "the plugin was killed (…)", which mentions memory for SIGKILL. |
| The result doesn't pass the checks above | Fails with the reason. |

On a failure the terminal output stays visible until the task is cleared, so print
whatever would help someone work out what went wrong.

**Cancelling** (the task's ✕, or **Cancel** in its view) sends SIGTERM to the plugin's
whole process group, so `conda run`, `nix shell`, Python and anything Python started all
receive it. Anything still running 5 seconds later gets SIGKILL. When the plugin exits
by itself, whatever it left running in its group is killed too. To clean up on cancel,
handle SIGTERM (Python's default is to exit at once):

```python
import signal, sys
signal.signal(signal.SIGTERM, lambda *_: sys.exit(1))   # runs finally: blocks and atexit
```

## The `edict` helper module

[`plugins/edict.py`](../plugins/edict.py) is copied into each run's `.edict/` folder
and put on `PYTHONPATH`, so `import edict` always gets the version that matches the
running app. It uses only the standard library. Don't copy it into your plugin.

| Name | Does |
|---|---|
| `request() -> dict` | Reads the request from `$EDICT_REQUEST`, or from `argv[2]` when run by hand. Exits with a message if there's neither. |
| `progress(text, fraction=None)` | Sends a `progress` event. |
| `warn(text)` | Prints `warning: …` to stderr and sends a `warn` event. |
| `fail(message, code=1)` | Prints `error: …` to stderr, sends an `error` event, and exits immediately with `code` (`os._exit`: no cleanup handlers run). |
| `result(kind, **fields)` | Sends a `result` event with the fields above. |
| `in_app() -> bool` | Whether `$EDICT_EVENTS` is set. Without it, events are silently dropped, so a plugin can also run outside the app. |
| `mem_available() -> int \| None` | `MemAvailable` from `/proc/meminfo` in bytes (Linux). |
| `rss() -> int \| None` | This process's resident memory in bytes (Linux). |
| `first_to_be_killed()` | Sets this process's own `oom_score_adj` to 1000. The app already does this for the process it starts. |
| `RamGuard(floor, interval=0.25, what="this run")` | See [Memory](#memory). |
| `Timer()` | `with edict.Timer() as t: …` then `t.seconds`. |
| `API`, `MEMORY_GUARD_EXIT`, `GIB` | `1`, `75`, `1 << 30`. |

There is no helper for `log` events. Write `{"event": "log", "text": …}` yourself if you
want a line in the task's log rather than the terminal.

## Memory

A large model can use up the machine's RAM. When that happens the kernel's OOM killer
picks a victim, and on a desktop that can be the compositor, taking the whole session
down. Two protections apply:

- The app sets the plugin's `oom_score_adj` to 1000, so the plugin is the kernel's
  first choice.
- `edict.RamGuard` watches free memory from a thread and stops the run on purpose,
  with exit status 75 and a clear message, before the kernel has to:

  ```python
  with edict.RamGuard(floor=2 * edict.GIB, what="my model"):
      import torch               # start the guard before heavy imports:
      model = load()             # importing torch and loading weights are usually
      run(model)                 # where memory use peaks
  ```

  It does nothing where free memory can't be read (anything but Linux).

Let the user change the floor with a setting if your plugin may legitimately need to dip
into swap. ESMFold2 also checks free RAM and disk before it starts, sizes its GPU work
from free VRAM, and retries with smaller chunks on `torch.OutOfMemoryError`. These are
patterns worth copying for any large model.

## Testing and debugging

### Check that the plugin loads

**Settings → Plugins → Rescan**. If the plugin is under **Not loaded**, the reason says
which key or line is wrong. If it loads but says **No usable environment**, the reasons
list why each environment was rejected ("uv isn't on PATH", "no virtualenv at …").

### Run it outside the app

The plugin doesn't need Edict to run. Make a request by hand and call it the way the
app does:

```sh
work=$(mktemp -d) && cd "$work" && mkdir .edict
cp /path/to/edict/plugins/edict.py .edict/
cat > .edict/request.json <<'JSON'
{"api": 1, "action": "build", "settings": {"rise": 1.5},
 "input": {"sequences": [{"id": "A", "seq": "MSKGEELFTGVVPILVELDGDV"}],
           "feature": {"label": "test", "kind": "CDS"}, "doc_name": "test"},
 "workdir": "."}
JSON
PYTHONPATH=$PWD/.edict EDICT_EVENTS=$PWD/.edict/events.jsonl \
  python3 ~/.config/edict/plugins/ca-helix/plugin.py run .edict/request.json
cat .edict/events.jsonl
```

Swap `python3` for the environment's command (`uv run --script`, `conda run -n esm
--no-capture-output python`, …) to test that too. Include every setting the action
uses in `settings`, with the types listed under [Settings](#settings).

### Look at a real run

While a run is in progress, its folder is under `~/.cache/edict/tasks/`. The command
line is the first line in the task's terminal and log. `.edict/request.json` and
`.edict/events.jsonl` show exactly what was sent and received.

### Common problems

| Symptom | Likely cause |
|---|---|
| Not listed at all | The file isn't named `plugin.py`, or isn't directly inside a folder in the plugins folder. Or Rescan wasn't clicked. |
| "no `# /// script` block" | The opening line isn't exactly `# /// script`. |
| "the script block isn't valid TOML" | A line in the block lost its `# ` prefix, or the TOML has a syntax error. |
| "unknown field" | A typo in a key, or a key in the wrong table. |
| "its default doesn't fit" | The default is outside `min`/`max`, the wrong type (`1` for a `float` is fine, `"1"` is not), or not one of the choices. |
| `ModuleNotFoundError: edict` | The plugin is being run outside the app without `PYTHONPATH`. Or it changes `sys.path` or `PYTHONPATH` itself. |
| "finished without giving a result" | The script exited 0 without calling `edict.result`, or `$EDICT_EVENTS` was unset or changed. |
| "has no chain matching protein A" | The structure's residues don't spell the sequence that was sent: missing residues, an extra Met, a non-standard residue name, or chains in the wrong place. |
| "isn't in the run's folder" | `file` points outside the scratch folder. Write the structure in the working directory. |

## Sharing a plugin

A plugin is just its folder, so share it however you share files: a git repository, an
archive, a gist. To install it, a user puts the folder in their plugins folder and clicks
**Rescan**. For example:

```sh
git clone https://example.org/you/my-plugin ~/.config/edict/plugins/my-plugin
```

Some suggestions:

- Keep everything the plugin needs to *describe* itself in `plugin.py`. Put large or
  generated things (environments, weights, caches) outside the folder or in ignored
  paths.
- Say in `description` what it downloads, how much memory and disk it needs, and what
  hardware it expects.
- Offer more than one environment, so users without your preferred tool can still run
  it.
- Bump `version` when you change behaviour, and say what changed.
- Choose a distinctive `id`. Two plugins with the same id can't both be installed.

## Compatibility

`api = 1` is the protocol described here: the manifest keys, the request, the events and
the result. A plugin written for a different API version is listed under **Not loaded**
rather than run with the wrong protocol. Additions that old plugins can safely ignore
(new request fields, new optional manifest keys, new helper functions) don't change the
API version.
