# pLannotate (automatic annotation)

[pLannotate](https://github.com/mmcguffi/pLannotate) finds the common parts of
engineered DNA: promoters, resistance genes, origins, tags, fluorescent proteins and
so on. It searches SnapGene's feature set (BLAST), FPbase and Swiss-Prot (DIAMOND) and
Rfam (Infernal). Edict runs it through a plugin, which you install yourself. The app
doesn't ship with any plugins.

## Installing the plugin

Copy the plugin's folder from this repository into your plugins folder, then click
**Settings → Plugins → Rescan**:

```sh
mkdir -p ~/.config/edict/plugins
cp -r plugins/plannotate ~/.config/edict/plugins/
```

To follow changes in the repository instead of copying, symlink it:

```sh
ln -s "$PWD/plugins/plannotate" ~/.config/edict/plugins/plannotate
```

## Using it

Right-click the sequence → **Plugins → Annotate with pLannotate…**. The menu says
what it will annotate:

- **Selection**: the selected bases, annotated as linear DNA.
- **Whole sequence**: when nothing is selected, or the selection covers everything.
  A circular sequence is annotated as circular, so a part that runs through the
  origin is found whole.

The bases are locked while the plugin runs. When it finishes, the result card shows
what it found on a map, over the sequence's own features (faded), and lists each
feature with its location, database and identity. Click a feature on the map, or its
row, to tick or untick it. Partial matches
("fragments") are listed but not ticked. Features that are already in the document,
with the same location, strand and kind or label, are collapsed into one
"already in the document" group and are also not ticked. Click **Add N features** to
add the ticked ones as one edit, which Undo takes out again.

Settings:

| Setting | Default | |
|---|---|---|
| Search | Full | **Fast** skips Swiss-Prot proteins and Rfam RNAs. |
| Cores | 4 | How many of pLannotate's searches run at once. |

## Environments

The plugin tries these environments in order, and uses the first one available
(pick another in **Settings → Plugins**):

1. `nix`: this flake's `plannotate-env` (see below). `{flake}` is `$EDICT_FLAKE` if
   set, otherwise the source tree the app was built from.
2. `conda`: an environment named `plannotate`:

   ```sh
   mamba create -n plannotate -c conda-forge -c bioconda plannotate
   mamba run -n plannotate plannotate setupdb
   ```

pLannotate isn't on PyPI and needs BLAST, DIAMOND and Infernal, so there is no uv
environment.

See [Writing an Edict plugin](plugins.md) for how plugins work.

## The nix build

`nix/plannotate.nix` builds pLannotate 2.0.0 from its GitHub release, with BLAST and
DIAMOND from nixpkgs and Infernal 1.1.5 (not in nixpkgs, so it's packaged there):

```sh
nix run .#plannotate -- batch -i plasmid.fasta -o out   # the pLannotate CLI
nix build .#plannotate-env                              # what the plugin runs in
```

`plannotate setupdb` normally downloads the databases into pLannotate's own package
folder, which is read-only in the Nix store. Instead, the build fetches the same
release bundle (pinned by hash; about 256 MB to download and 1.2 GB unpacked) and
links it into place, so there is no setup step. pLannotate asks for pandas < 3;
the build relaxes that pin, since nixpkgs has pandas 3 and the results on pUC19 are
unchanged.

## Citing

If you publish results from this plugin, cite pLannotate (the plugin's result card
shows this, with a Copy button):

> McGuffie MJ, Barrick JE. pLannotate: engineered plasmid annotation. Nucleic Acids
> Res. 49(W1):W516–W522 (2021).
> [doi:10.1093/nar/gkab374](https://doi.org/10.1093/nar/gkab374)

## License

pLannotate is GPL-3.0. Edict doesn't include it, its databases or any of its code:
the plugin runs `plannotate` as a separate program and reads the GenBank file it
writes. The nix environment and the conda environment are both built on your
machine from upstream sources.

pLannotate's feature database is derived from SnapGene's standard feature set. Check
its terms yourself if you'll use the annotations commercially.
