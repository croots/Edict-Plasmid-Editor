"""Fold protein sequence(s) with ESMFold2 and write an mmCIF file.

Built for being driven by another program (the Edict plugin):
progress goes to stderr, and stdout carries exactly one JSON object:

    {"out": "pred.cif", "model": "...", "device": "cuda", "ptm": 0.81,
     "iptm": null, "plddt_mean": 0.87, "plddt": [...], "chains": ["A"],
     "seconds": 12.3}

Each FASTA record (or each --sequence) becomes one chain of the complex.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time


def log(msg: str) -> None:
    print(f"esmfold2: {msg}", file=sys.stderr, flush=True)


def read_fasta(path: str) -> list[str]:
    text = sys.stdin.read() if path == "-" else open(path).read()
    seqs, cur = [], []
    for line in text.splitlines():
        line = line.strip()
        if line.startswith(">"):
            if cur:
                seqs.append("".join(cur))
            cur = []
        elif line:
            cur.append(line)
    if cur:
        seqs.append("".join(cur))
    return seqs


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("-s", "--sequence", action="append", help="protein sequence (repeat for a complex)")
    src.add_argument("-f", "--fasta", help="FASTA file, '-' for stdin; each record is a chain")
    ap.add_argument("-o", "--out", default="pred.cif", help="output mmCIF path (default: pred.cif)")
    ap.add_argument("--model", default="biohub/ESMFold2-Fast",
                    help="HF repo or local dir (biohub/ESMFold2-Fast, biohub/ESMFold2)")
    ap.add_argument("--device", default="auto", help="auto, cuda (also ROCm), cpu")
    ap.add_argument("--esmc-precision", choices=["auto", "fp8", "bf16", "fp32"], default="auto",
                    help="ESMC language-model precision; auto = fp8 on NVIDIA sm_89+, bf16 on other GPUs, "
                         "fp32 on CPU. The folding trunk stays fp32 (it autocasts itself); the 6B "
                         "ESMC is most of the weight, so bf16 keeps the whole model near 16 GB")
    ap.add_argument("--kernels", choices=["auto", "reference", "fused", "fast"], default="auto",
                    help="kernel backend; auto = fused Triton on NVIDIA, reference elsewhere")
    ap.add_argument("--num-loops", type=int, help="recycling loops (default: 3 for -Fast, 20 otherwise)")
    ap.add_argument("--steps", type=int, help="diffusion sampling steps (default: 50 for -Fast, 100 otherwise)")
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--chunk-size", type=int, default=64,
                    help="cap on L^2 op chunk; lower to save memory, 0 = unchunked")
    args = ap.parse_args()

    seqs = args.sequence if args.sequence else read_fasta(args.fasta)
    seqs = [s.strip().upper().replace("*", "") for s in seqs if s.strip()]
    if not seqs:
        log("no sequences given")
        return 2

    os.environ.setdefault("PYTORCH_CUDA_ALLOC_CONF", "expandable_segments:True")
    import torch
    from esm.models.esmfold2 import (
        ESMFold2InputBuilder,
        EsmFold2Model,
        ProteinInput,
        StructurePredictionInput,
    )

    device = args.device
    if device == "auto":
        device = "cuda" if torch.cuda.is_available() else "cpu"
    on_gpu = device.startswith("cuda")
    rocm = on_gpu and torch.version.hip is not None
    nvidia = on_gpu and not rocm

    esmc_precision = args.esmc_precision
    if esmc_precision == "auto":
        # esm's fp8 path keys off get_device_capability() >= (8, 9), which ROCm
        # also satisfies (RDNA3 reports 11.x) without having the fp8 GEMMs.
        fp8_ok = nvidia and torch.cuda.get_device_capability() >= (8, 9)
        esmc_precision = "fp8" if fp8_ok else ("bf16" if on_gpu else "fp32")
    kernels = args.kernels if args.kernels != "auto" else ("fused" if nvidia else "reference")

    fast = "fast" in args.model.lower()
    num_loops = args.num_loops if args.num_loops is not None else (3 if fast else 20)
    steps = args.steps if args.steps is not None else (50 if fast else 100)

    gpu_name = torch.cuda.get_device_name() if on_gpu else "cpu"
    log(f"device={device} ({gpu_name}{', ROCm' if rocm else ''}) "
        f"esmc={esmc_precision} kernels={kernels} loops={num_loops} steps={steps}")
    log(f"loading {args.model} (first run downloads ~26 GB to $HF_HOME)")

    t0 = time.time()
    model = EsmFold2Model.from_pretrained(
        args.model,
        device=device,
        esmc_precision=esmc_precision,
    ).eval()
    model.set_kernel_backend(None if kernels == "reference" else kernels)
    model.set_chunk_size(args.chunk_size or None)
    log(f"loaded in {time.time() - t0:.1f}s; folding {len(seqs)} chain(s), "
        f"{sum(map(len, seqs))} residues")

    chain_ids = [chr(ord("A") + i) for i in range(len(seqs))]
    spi = StructurePredictionInput(
        sequences=[ProteinInput(id=c, sequence=s) for c, s in zip(chain_ids, seqs)]
    )
    t1 = time.time()
    with torch.inference_mode():
        result = ESMFold2InputBuilder().fold(
            model, spi, num_loops=num_loops, num_sampling_steps=steps,
            num_diffusion_samples=1, seed=args.seed,
        )
    if isinstance(result, list):
        result = result[0]
    secs = time.time() - t1

    with open(args.out, "w") as fh:
        fh.write(result.complex.to_mmcif())

    def scalar(x):
        if x is None:
            return None
        return float(x.float().mean()) if torch.is_tensor(x) else float(x)

    plddt = result.plddt.float().flatten().cpu().tolist() if result.plddt is not None else []
    summary = {
        "out": args.out,
        "model": args.model,
        "device": device,
        "ptm": scalar(result.ptm),
        "iptm": scalar(result.iptm),
        "plddt_mean": (sum(plddt) / len(plddt)) if plddt else None,
        "plddt": plddt,
        "chains": chain_ids,
        "seconds": round(secs, 2),
    }
    log(f"folded in {secs:.1f}s: pTM={summary['ptm']} mean pLDDT={summary['plddt_mean']} -> {args.out}")
    print(json.dumps(summary))
    return 0


if __name__ == "__main__":
    sys.exit(main())
