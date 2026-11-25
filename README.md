# Edict

Edict is a fast, GPU-rendered editor for plasmids and other DNA sequences, built on [GPUI](https://crates.io/crates/gpui) (Zed's UI framework). It opens GenBank, FASTA and SnapGene files and saves to its own `.edna` format, which keeps a full edit history and attached 3D structures alongside the sequence.

https://github.com/user-attachments/assets/ce45c983-ef63-4e29-ab27-d03fb6dc3b7c

Edict is free for noncommercial academic use. See [License](#license).

## Why Edict

Edict is a plasmid editor built for researchers.

- **Free and open to read.** The source is available, and Edict costs nothing for academic teaching and research.
- **Local.** Everything runs on your machine. Your sequences stay in files you own, in formats other tools can read, and nothing is uploaded unless you ask for it (a BLAST search, say).
- **Extensible.** If a bioinformatics job can run in Python, it can run inside Edict. A plugin is a single Python script. It declares its settings and the environment it needs (uv, venv, conda or nix), and Edict takes care of the form, the live log, cancelling, and putting the results back on the sequence. Think of it as ImageJ for DNA.
- **Fast and good-looking.** The UI is drawn on the GPU, so even a whole genome scrolls smoothly. It ships with hundreds of base16 themes, including Nord, Gruvbox and Solarized, and you can add your own.

Edict is still in development. The aim is to make a first-class structure viewer, alignment tools and cloning tools part of the app itself. [docs/features.md](docs/features.md) lists what works today and what is next.

## Plugins

Edict installs no plugins by default. [`plugins/`](plugins/README.md) has the guide to writing your own and two examples to start from:

- **[ESMFold2](plugins/esmfold2/plugin.py)** predicts a protein's 3D structure from a CDS and attaches it to the feature, colored by confidence ([setup](docs/esmfold2.md)).
- **[pLannotate](plugins/plannotate/plugin.py)** automatically annotates the common parts of engineered DNA: promoters, resistance genes, origins, tags and fluorescent proteins ([setup](docs/plannotate.md)).

To install one, copy or symlink its folder into `~/.config/edict/plugins/`, then click **Settings → Plugins → Rescan**. [docs/plugins.md](docs/plugins.md) covers how to write one.

## Run

```sh
nix develop                     # Rust toolchain, Zig 0.17 (for libghostty-vt), GPUI's Vulkan/Wayland/X11/font libraries
cargo run --release -- samples  # explorer root; optional second arg opens a file
```

## Repository

| Folder | Contents |
|---|---|
| `src/` | The app. [docs/features.md](docs/features.md#source-layout) describes each module. |
| `plugins/` | The example plugins and `edict.py`, the helper module every plugin run gets. |
| `docs/` | [Feature reference](docs/features.md), [plugin guide](docs/plugins.md), [ESMFold2 setup](docs/esmfold2.md), [pLannotate setup](docs/plannotate.md) and [the `.edna` format](docs/edna-format.md). |
| `nix/` | Nix builds of the plugins' Python environments. |
| `samples/` | Example sequences to try. |
| `themes/` | The bundled base16 schemes. |
| `licenses/`, `vendor/`, `scripts/` | Third-party license texts, a patched dependency, and maintenance scripts. |

## License

Edict is source-available under the [Edict Academic Noncommercial License](LICENSE). In short: students, researchers and staff at universities and nonprofit research institutions, and individuals learning on their own, may use, modify and redistribute Edict free of charge for noncommercial academic teaching and research, as long as the license and copyright notice go with it and nothing is charged for it. Commercial use, including any use by or for a for-profit company, needs a separate license from the copyright holder.

This summary is for convenience; the [LICENSE](LICENSE) file is what governs. Third-party components keep their own licenses (see `licenses/` and the About panel).
