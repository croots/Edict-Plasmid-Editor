#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# # pLannotate isn't on PyPI and needs BLAST, DIAMOND and Infernal, so there is no
# # uv environment: use the nix or the conda one.
# dependencies = ["biopython"]
#
# [tool.edict]
# id = "plannotate"
# name = "pLannotate"
# api = 1
# version = "1"
# description = """
# Finds the common parts of engineered DNA (promoters, resistance genes, origins,
# tags, fluorescent proteins...) with pLannotate, which searches SnapGene's feature
# set (BLAST), FPbase and Swiss-Prot (DIAMOND) and Rfam (Infernal). Runs locally;
# its databases take about 1.2 GB of disk. A whole circular sequence is annotated as
# circular, and a selection as linear DNA."""
#
# # Shown with every result, as a reminder to cite pLannotate.
# [[tool.edict.cite]]
# text = "McGuffie MJ, Barrick JE. pLannotate: engineered plasmid annotation. Nucleic Acids Res. 49(W1):W516–W522 (2021)."
# doi = "10.1093/nar/gkab374"
#
# # Tried in order; the first that works here is used (choose one in Settings → Plugins).
# # The app's own flake: pLannotate, the search tools and the databases, built by nix.
# [[tool.edict.env]]
# kind = "nix"
# installable = "{flake}#plannotate-env"
#
# # `mamba create -n plannotate -c conda-forge -c bioconda plannotate`, then
# # `mamba run -n plannotate plannotate setupdb`.
# [[tool.edict.env]]
# kind = "conda"
# name = "plannotate"
#
# [[tool.edict.action]]
# id = "annotate"
# label = "Annotate with pLannotate…"
# on = "region"
# input = "dna"
# output = "features"
#
# [[tool.edict.setting]]
# key = "search"
# label = "Search"
# type = "choice"
# default = "full"
# choices = [
#   { value = "full", label = "Full", help = "SnapGene features, FPbase, Swiss-Prot and Rfam." },
#   { value = "fast", label = "Fast", help = "SnapGene features and FPbase only: skips Swiss-Prot proteins and Rfam RNAs." },
# ]
#
# [[tool.edict.setting]]
# key = "cores"
# label = "Cores"
# type = "int"
# min = 1
# max = 256
# default = 4
# help = "How many of pLannotate's searches run at once."
# ///
"""Annotate the bases Edict sends with pLannotate and offer what it finds as features.

pLannotate runs as its own program (`plannotate batch`), and its GenBank output is
read back here, so this script only prepares the input and translates the output.
"""

import os
import shutil
import signal
import subprocess
import sys

import edict

# Qualifiers worth keeping on an added feature, in this order. pLannotate also writes
# /fragment and /other (the type again), which the label and type already say.
KEEP = ("label", "note", "annotator", "database", "identity", "match_length")


def main() -> None:
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(1))
    req = edict.request()
    dna = req["input"]["sequences"][0]
    seq, circular = dna["seq"].upper(), bool(dna.get("circular"))
    settings = req["settings"]

    exe = shutil.which("plannotate")
    if exe is None:
        edict.fail("plannotate isn't on PATH in this environment. Use the nix environment, "
                   "or make a conda env named plannotate (see the plugin's description).")
    try:
        from Bio import SeqIO
    except ImportError:
        edict.fail("Biopython isn't installed in this environment; pLannotate needs it too.")

    with open("input.fasta", "w") as fh:
        fh.write(">region\n")
        fh.writelines(seq[i:i + 80] + "\n" for i in range(0, len(seq), 80))

    cmd = [exe, "batch", "-i", "input.fasta", "-o", "out", "-f", "result", "-s", "",
           "--cores", str(settings["cores"])]
    if not circular:
        cmd.append("--linear")
    if settings["search"] == "fast":
        cmd.append("--fast")
    shape = "circular" if circular else "linear"
    print(f"Annotating {len(seq):,} bp as {shape} DNA", flush=True)
    print("$ " + " ".join(cmd), flush=True)
    edict.progress("Searching")
    status = subprocess.run(cmd).returncode
    gbk = os.path.join("out", "result.gbk")
    if status != 0 or not os.path.exists(gbk):
        edict.fail(f"pLannotate exited with status {status}; its output above says why. "
                   "If it says the databases aren't downloaded, run `plannotate setupdb` "
                   "in this environment.")

    edict.progress("Reading results")
    record = SeqIO.read(gbk, "genbank")
    comment = record.annotations.get("comment", "")
    version = comment.split("pLannotate ", 1)[1].split()[0].rstrip(".") if "pLannotate v" in comment else ""
    features = [proposal(f) for f in record.features]
    features = [f for f in features if f]
    print(f"pLannotate found {len(features)} feature{'s' if len(features) != 1 else ''}", flush=True)
    edict.result(kind="features", features=features, source=f"pLannotate {version}".strip())


def proposal(f) -> dict | None:
    """A feature in Edict's form: segments in forward order, 0-based half-open."""
    parts = list(f.location.parts)
    if not parts:
        return None
    strand = -1 if f.location.strand == -1 else 1
    # Biopython lists a reverse feature's parts in the order they're read (3' to 5'
    # on the forward strand); Edict keeps them in forward order, as GenBank's
    # complement(join(...)) does.
    if strand < 0:
        parts.reverse()
    segments = [[int(p.start), int(p.end)] for p in parts]
    q = {k: v[0] if isinstance(v, list) else v for k, v in f.qualifiers.items()}
    label = str(q.get("label", f.type))
    fragment = str(q.get("fragment", "")).lower() == "true"
    qualifiers = [[k, str(q[k])] for k in KEEP if k in q]
    detail = [str(q.get("database", "")).strip()]
    if "identity" in q:
        detail.append(f"{float(q['identity']):g}% identity")
    if "match_length" in q:
        share = float(q["match_length"])
        detail.append("full length" if share >= 100 else f"{share:g}% of its length")
    return {
        "type": f.type,
        "label": label,
        "strand": strand,
        "segments": segments,
        "qualifiers": qualifiers,
        "detail": " · ".join(d for d in detail if d),
        # Fragments are offered but not ticked: a partial match is often noise.
        "selected": not fragment,
    }


if __name__ == "__main__":
    main()
