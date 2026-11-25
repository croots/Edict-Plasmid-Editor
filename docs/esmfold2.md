# ESMFold2 (structure prediction)

ESMFold2 predicts a protein's structure from its sequence. Edict runs it through a
plugin, which you install yourself. The app doesn't ship with any plugins.

## Installing the plugin

Copy the plugin's folder from this repository into your plugins folder, then click
**Settings → Plugins → Rescan**:

```sh
mkdir -p ~/.config/edict/plugins
cp -r plugins/esmfold2 ~/.config/edict/plugins/
```

To follow changes in the repository instead of copying, symlink it:

```sh
ln -s "$PWD/plugins/esmfold2" ~/.config/edict/plugins/esmfold2
```

Then right-click a CDS or ORF → **Plugins → Predict structure with ESMFold2…**.

The plugin tries these environments in order, and uses the first one available
(pick another in **Settings → Plugins**):

1. `nix`: this flake's `esmfold2-env` (see below). `{flake}` is `$EDICT_FLAKE` if set,
   otherwise the source tree the app was built from.
2. `conda`: an environment named `esm`.
3. `venv`: a `.venv` folder inside the plugin's folder.
4. `uv`: installs `esm==3.5.0` from PyPI. PyPI's torch is the CUDA build, so on AMD
   use one of the others.

See [Writing an Edict plugin](plugins.md) for how plugins work.

## The nix build

`nix/esmfold2.nix` builds Biohub's [`esm`](https://github.com/Biohub/esm) 3.5.0 against nixpkgs'
ROCm torch (prebuilt on cache.nixos.org; ~5 GB download) and wraps it in `esmfold2-predict`:

```sh
nix run .#esmfold2 -- -s MQIFVKTLTGKTITLEVEPSDTIENVKAKIQDKEGIPPDQQRLIFAGKQLEDGRTLSDYNIQKESTLHLVLRLRGG -o ubq.cif
nix run .#esmfold2 -- -f chains.fasta -o complex.cif --model biohub/ESMFold2   # each record is a chain
nix develop .#esmfold2          # Python with `esm` importable
nix build .#esmfold2-env        # just that Python (what the plugin runs in)
nix build .#esmfold2-cpu        # torch without ROCm (very slow for a 7B model)
```

It writes mmCIF (which `src/structure.rs` reads) and prints one JSON object on stdout
(`ptm`, `iptm`, `plddt_mean`, per-token `plddt`, `seconds`); progress goes to stderr. The weights
(~26 GB per model, MIT) download from Hugging Face into `$HF_HOME` on first run. The folding trunk
stays fp32; on ROCm the 6B ESMC language model runs in bf16 rather than esm's default fp8 (RDNA3
reports capability 11.0, which esm's fp8 check would otherwise accept but cannot run).

## Citing

If you publish results from this plugin, cite ESMFold2 (the plugin's result card shows
this, with a Copy button):

> Candido S, Hayes T, Derry A, et al. Language Modeling Materializes a World Model of
> Protein Biology. bioRxiv 2026.06.03.729735 (2026).
> [doi:10.64898/2026.06.03.729735](https://doi.org/10.64898/2026.06.03.729735)
