#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = []
#
# [tool.edict]
# id = "ideal-helix"
# name = "Ideal helix (demo)"
# api = 1
# version = "1"
# description = """
# A small example plugin: builds the protein as one ideal alpha helix. It needs only
# the Python standard library, so it shows how plugins work without installing
# anything. The confidence it reports is made up, to show the colouring."""
#
# # No dependencies, so any Python 3 will do.
# [[tool.edict.env]]
# kind = "python"
# path = "python3"
#
# [[tool.edict.env]]
# kind = "python"
# path = "python"
#
# [[tool.edict.action]]
# id = "helix"
# label = "Build ideal helix (demo)"
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
# help = "1.5 Å for an alpha helix; 2.0 for a 3-10 helix."
#
# [[tool.edict.setting]]
# key = "delay"
# label = "Pretend to work for"
# type = "choice"
# choices = [{ value = "0", label = "No time" }, { value = "3", label = "3 s" }, { value = "20", label = "20 s" }]
# default = "3"
# help = "Time to try out Background and Cancel."
# ///
"""Build a protein as an ideal helix: the smallest complete Edict plugin.

Run by the app as ``python plugin.py run request.json``; ``edict`` is the helper
module the app puts on PYTHONPATH.
"""

import math
import sys
import time

import edict

THREE = {
    "A": "ALA", "R": "ARG", "N": "ASN", "D": "ASP", "C": "CYS", "Q": "GLN", "E": "GLU",
    "G": "GLY", "H": "HIS", "I": "ILE", "L": "LEU", "K": "LYS", "M": "MET", "F": "PHE",
    "P": "PRO", "S": "SER", "T": "THR", "W": "TRP", "Y": "TYR", "V": "VAL",
}

# Backbone atoms of an ideal alpha helix in cylindrical coordinates relative to
# the residue's CA: (name, element, radius Å, angle offset °, height offset Å).
BACKBONE = [
    ("N", "N", 1.55, -28.1, -0.89),
    ("CA", "C", 2.30, 0.0, 0.0),
    ("C", "C", 1.61, 28.4, 0.99),
    ("O", "O", 1.76, 38.0, 2.20),
]


def helix_cif(seq: str, rise: float, confidence: list) -> str:
    rows = []
    n = 0
    for i, aa in enumerate(seq):
        for name, element, r, dphi, dz in BACKBONE:
            n += 1
            phi = math.radians(i * 100.0 + dphi)
            x, y, z = r * math.cos(phi), r * math.sin(phi), i * rise + dz
            rows.append(
                f"ATOM {n} {element} {name} {THREE.get(aa, 'UNK')} A 1 {i + 1} "
                f"{x:.3f} {y:.3f} {z:.3f} 1.00 {confidence[i]:.2f} {i + 1} A 1"
            )
    return "\n".join(
        [
            "data_helix",
            "#",
            "_struct.title 'Ideal helix (demo; plddt made up)'",
            "#",
            "loop_",
            "_atom_site.group_PDB",
            "_atom_site.id",
            "_atom_site.type_symbol",
            "_atom_site.label_atom_id",
            "_atom_site.label_comp_id",
            "_atom_site.label_asym_id",
            "_atom_site.label_entity_id",
            "_atom_site.label_seq_id",
            "_atom_site.Cartn_x",
            "_atom_site.Cartn_y",
            "_atom_site.Cartn_z",
            "_atom_site.occupancy",
            "_atom_site.B_iso_or_equiv",
            "_atom_site.auth_seq_id",
            "_atom_site.auth_asym_id",
            "_atom_site.pdbx_PDB_model_num",
            *rows,
            "#",
            "",
        ]
    )


def main() -> None:
    req = edict.request()
    settings = req["settings"]
    chain = req["input"]["sequences"][0]
    seq = chain["seq"]
    print(f"Building {len(seq)} residues of {req['input']['feature']['label']} as an ideal helix")

    delay = float(settings["delay"])
    steps = 20
    for k in range(steps):
        edict.progress("Pretending to fold", k / steps)
        print(f"  step {k + 1}/{steps}", flush=True)
        time.sleep(delay / steps)

    # Made-up confidence: high in the middle, fraying at the ends.
    conf = [95 - 50 * (abs(i - (len(seq) - 1) / 2) / max(len(seq) / 2, 1)) ** 2 for i in range(len(seq))]
    with open("helix.cif", "w", encoding="ascii") as fh:
        fh.write(helix_cif(seq, float(settings["rise"]), conf))
    print("Wrote helix.cif")
    edict.result(
        kind="structure",
        file="helix.cif",
        format="mmcif",
        chains=[chain["id"]],
        source="Ideal helix (demo)",
        metrics={"mean confidence": sum(conf) / len(conf)},
        plddt=conf,
    )


if __name__ == "__main__":
    if len(sys.argv) < 2 or sys.argv[1] != "run":
        sys.exit("usage: plugin.py run request.json")
    main()
