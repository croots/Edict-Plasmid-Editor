#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# # For `uv run --script`. PyPI's torch is the CUDA build; on AMD, use the nix or a
# # conda environment with ROCm torch instead.
# dependencies = ["esm==3.5.0"]
#
# [tool.plasmid]
# id = "esmfold2"
# name = "ESMFold2"
# api = 1
# version = "1"
# description = """
# Predicts a protein's structure from its sequence with Biohub's ESMFold2
# (the `esm` package, 3.5). Weights (~26 GB per model) download from Hugging
# Face on first use into $HF_HOME. Needs a GPU with about 16 GB free for
# 600-residue proteins; loading briefly needs about 34 GB of RAM."""
#
# # Tried in order; the first that works here is used (choose one in Settings → Plugins).
# # The app's own flake: ROCm torch + esm, built by nix (prebuilt torch from the cache).
# [[tool.plasmid.env]]
# kind = "nix"
# installable = "{flake}#esmfold2-env"
#
# [[tool.plasmid.env]]
# kind = "conda"
# name = "esm"
#
# [[tool.plasmid.env]]
# kind = "venv"
# path = ".venv"
#
# [[tool.plasmid.env]]
# kind = "uv"
#
# [[tool.plasmid.action]]
# id = "predict"
# label = "Predict structure with ESMFold2…"
# on = "feature"
# kinds = ["CDS", "ORF"]
# input = "protein"
# output = "structure"
#
# [[tool.plasmid.setting]]
# key = "model"
# label = "Model"
# type = "choice"
# default = "biohub/ESMFold2-Fast"
# choices = [
#   { value = "biohub/ESMFold2-Fast", label = "Fast", help = "3 recycles, 50 diffusion steps: seconds per protein." },
#   { value = "biohub/ESMFold2", label = "Full", help = "20 recycles, 100 steps: slower, a little more accurate." },
# ]
#
# [[tool.plasmid.setting]]
# key = "seed"
# label = "Seed"
# type = "int"
# min = 0
# default = 0
# help = "Diffusion sampling is random; another seed gives another sample."
#
# [[tool.plasmid.setting]]
# key = "device"
# label = "Device"
# type = "choice"
# default = "auto"
# choices = [
#   { value = "auto", label = "Auto", help = "The GPU if torch sees one (CUDA or ROCm), else the CPU." },
#   { value = "cuda", label = "GPU" },
#   { value = "cpu", label = "CPU", help = "Works anywhere, but very slow, and needs ~40 GB of RAM." },
# ]
#
# [[tool.plasmid.setting]]
# key = "chunk"
# label = "Chunk size"
# type = "choice"
# default = "auto"
# group = "Memory"
# choices = [
#   { value = "auto", label = "Auto", help = "The largest that fits in free GPU memory." },
#   "64", "32", "16",
#   { value = "big", label = "Big", help = "esm's lowest-memory mode, for very long proteins." },
# ]
# help = "Smaller chunks fold longer proteins in less GPU memory, more slowly."
#
# [[tool.plasmid.setting]]
# key = "ram_floor"
# label = "Keep free RAM (GiB)"
# type = "float"
# min = 0.5
# max = 64
# default = 2.0
# group = "Memory"
# help = "The memory guard stops the run if free RAM falls below this, before the system starts killing programs."
#
# [[tool.plasmid.setting]]
# key = "checks"
# label = "Check memory before starting"
# type = "bool"
# default = true
# group = "Memory"
# help = "Refuse to start when there clearly isn't enough RAM or GPU memory. Turn off if you know better (e.g. with lots of swap)."
# ///
"""ESMFold2 structure prediction, as a plasmid-gpui plugin.

The app runs ``python plugin.py run request.json`` in a scratch folder and shows this
script's output in a terminal. Outside the app it works the same way, given a
request file:

    {"settings": {"model": "biohub/ESMFold2-Fast"},
     "input": {"sequences": [{"id": "A", "seq": "MQIFVKTLTGKTITLEVEPSDTIENVKAKIQDKEGIPPDQQRLIFAGKQLEDGRTLSDYNIQKESTLHLVLRLRGG"}]}}

Memory: the 6B-parameter ESMC language model dominates. Loading builds the model on
the CPU at full size before casting and moving it (~34 GB peak RAM); on the GPU the
bf16 weights take ~12.5 GiB, and folding needs ~13.3·(L/1000)² GiB more at chunk 64
(measured on an RX 7900 XTX: L=600 used 4.8 GiB). A thread watches free RAM and
stops the run cleanly before the kernel's OOM killer would pick something.
"""

from __future__ import annotations

import os
import shutil
import sys
import time

import plasmid

GIB = plasmid.GIB
LOAD_RAM = 34 * GIB  # peak RSS while from_pretrained builds the model on the CPU
WEIGHTS_VRAM = 12.5 * GIB
DOWNLOAD = 26 * 10**9
CHUNKS = [64, 32, 16]


def activations(length: int, chunk: int) -> float:
    """Rough GPU memory for folding `length` residues at `chunk`, beyond the weights.

    Measured at chunk 64; smaller chunks are assumed to shrink the L² part in
    proportion, which overestimates the saving a little (some of it isn't chunked),
    hence the retry on a real out-of-memory error."""
    return 13.3 * GIB * (length / 1000) ** 2 * (chunk / 64)


def hf_home() -> str:
    return os.environ.get("HF_HOME") or os.path.join(
        os.environ.get("XDG_CACHE_HOME") or os.path.expanduser("~/.cache"), "huggingface"
    )


def cached(repo: str) -> bool:
    if os.path.isdir(repo):
        return True
    snapshots = os.path.join(hf_home(), "hub", "models--" + repo.replace("/", "--"), "snapshots")
    return os.path.isdir(snapshots) and any(os.scandir(snapshots))


def gib(n: float) -> str:
    return f"{n / GIB:.1f} GiB"


def main() -> None:
    req = plasmid.request()
    s = req.get("settings", {})
    repo = s.get("model", "biohub/ESMFold2-Fast")
    seed = int(s.get("seed", 0))
    floor = float(s.get("ram_floor", 2.0)) * GIB
    checks = bool(s.get("checks", True))
    chains = req["input"]["sequences"]
    seqs = [c["seq"].strip().upper().rstrip("*") for c in chains]
    ids = [c.get("id") or chr(ord("A") + i) for i, c in enumerate(chains)]
    length = sum(map(len, seqs))
    if not length:
        plasmid.fail("no protein sequence to fold")
    bad = sorted({ch for sq in seqs for ch in sq if not ch.isalpha()})
    if bad:
        plasmid.fail(f"the sequence has characters that aren't amino acids: {''.join(bad)}")

    plasmid.first_to_be_killed()
    print(f"ESMFold2: {len(seqs)} chain(s), {length} residues, model {repo}")

    # ---- checks that need nothing heavy imported ---------------------------------
    plasmid.progress("Checking memory")
    free = plasmid.mem_available()
    if free is not None:
        print(f"  RAM available: {gib(free)} (guard floor {gib(floor)})")
        if checks and free < LOAD_RAM + floor:
            plasmid.fail(
                f"loading ESMFold2 needs about {gib(LOAD_RAM)} of RAM and only {gib(free)} is available "
                f"(keeping {gib(floor)} free). Close other programs, or turn off "
                "'Check memory before starting' if you have swap to spare.",
                plasmid.MEMORY_GUARD_EXIT,
            )
    if not cached(repo):
        home = hf_home()
        os.makedirs(home, exist_ok=True)
        disk = shutil.disk_usage(home).free
        print(f"  {repo} isn't downloaded yet; {disk / 1e9:.0f} GB free in {home}")
        if checks and disk < DOWNLOAD:
            plasmid.fail(f"the first run downloads ~{DOWNLOAD / 1e9:.0f} GB into {home}, which has {disk / 1e9:.0f} GB free; set HF_HOME to a bigger disk")

    # The guard runs from here to the end: importing torch and loading are the peaks.
    guard = plasmid.RamGuard(int(floor), what="ESMFold2").start()

    plasmid.progress("Starting torch")
    os.environ.setdefault("PYTORCH_CUDA_ALLOC_CONF", "expandable_segments:True")
    import torch
    try:
        # esm unpickles RDKit molecules from a newer RDKit; each one warns. They work.
        from rdkit import RDLogger
        RDLogger.DisableLog("rdApp.*")
    except ImportError:
        pass
    from esm.models.esmfold2 import ESMFold2InputBuilder, EsmFold2Model, ProteinInput, StructurePredictionInput

    device = s.get("device", "auto")
    if device == "auto":
        device = "cuda" if torch.cuda.is_available() else "cpu"
    if device == "cuda" and not torch.cuda.is_available():
        plasmid.fail("torch sees no GPU here (for AMD, this needs a ROCm build of torch); choose CPU to run anyway")
    on_gpu = device == "cuda"
    rocm = on_gpu and torch.version.hip is not None
    nvidia = on_gpu and not rocm
    # esm's fp8 path keys off compute capability >= 8.9, which ROCm also reports
    # without the fp8 GEMMs, so fp8 only on NVIDIA.
    precision = "fp8" if nvidia and torch.cuda.get_device_capability() >= (8, 9) else ("bf16" if on_gpu else "fp32")
    kernels = "fused" if nvidia else None

    chunk_setting = str(s.get("chunk", "auto"))
    chunk: int | str
    if on_gpu:
        gpu_free, gpu_total = torch.cuda.mem_get_info()
        print(f"  GPU: {torch.cuda.get_device_name()}{' (ROCm)' if rocm else ''}, {gib(gpu_free)} free of {gib(gpu_total)}")
        headroom = 1 * GIB
        budget = gpu_free - WEIGHTS_VRAM - headroom
        if checks and budget < activations(length, 16) * 0.25:
            plasmid.fail(
                f"the GPU has {gib(gpu_free)} free; ESMFold2's weights alone take {gib(WEIGHTS_VRAM)}. "
                "Close what else is using it, or choose the CPU.",
                plasmid.MEMORY_GUARD_EXIT,
            )
        if chunk_setting == "auto":
            chunk = next((c for c in CHUNKS if activations(length, c) < budget), "big")
            need = activations(length, chunk) if chunk != "big" else None
            print(f"  chunk size {chunk}" + (f" (needs ~{gib(need)} beyond the weights; {gib(max(budget, 0))} to spare)" if need else " (low-memory mode)"))
        else:
            chunk = chunk_setting if chunk_setting == "big" else int(chunk_setting)
        # Fail as an OutOfMemoryError we can catch rather than thrash or take the
        # whole card from the desktop.
        try:
            torch.cuda.set_per_process_memory_fraction(max(0.1, min(1.0, (gpu_total - headroom) / gpu_total)))
        except Exception:  # noqa: BLE001 - best effort
            pass
    else:
        chunk = 64 if chunk_setting == "auto" else (chunk_setting if chunk_setting == "big" else int(chunk_setting))
        print("  running on the CPU; this will be slow")

    fast = "fast" in repo.lower()
    loops, steps = (3, 50) if fast else (20, 100)

    plasmid.progress("Loading the model" + ("" if cached(repo) else " (downloading ~26 GB)"))
    print(f"Loading {repo} (esmc {precision}, kernels {kernels or 'reference'})")
    with plasmid.Timer() as t:
        model = EsmFold2Model.from_pretrained(repo, device=device, esmc_precision=precision).eval()
        model.set_kernel_backend(kernels)
    print(f"  loaded in {t.seconds:.1f} s" + (f"; GPU now has {gib(torch.cuda.mem_get_info()[0])} free" if on_gpu else ""))
    if guard.lowest is not None:
        print(f"  lowest free RAM while loading: {gib(guard.lowest)}")

    def set_chunk(c: int | str) -> None:
        if c == "big":
            if hasattr(model, "set_memory_mode"):
                model.set_memory_mode("big")
            model.set_chunk_size(16)
        else:
            model.set_chunk_size(c)

    spi = StructurePredictionInput(sequences=[ProteinInput(id=i, sequence=sq) for i, sq in zip(ids, seqs)])
    retries = 2
    while True:
        set_chunk(chunk)
        plasmid.progress(f"Folding {length} residues (chunk {chunk})")
        print(f"Folding: {loops} recycles, {steps} diffusion steps, seed {seed}, chunk {chunk}")
        try:
            with plasmid.Timer() as t, torch.inference_mode():
                out = ESMFold2InputBuilder().fold(
                    model, spi, num_loops=loops, num_sampling_steps=steps, num_diffusion_samples=1, seed=seed
                )
            break
        except torch.OutOfMemoryError:
            out = None
            if on_gpu:
                torch.cuda.empty_cache()
            smaller = {64: 32, 32: 16, 16: "big"}.get(chunk) if chunk != "big" else None
            if retries == 0 or smaller is None:
                plasmid.fail(
                    f"ran out of GPU memory folding {length} residues even at chunk {chunk}. "
                    "Close other programs using the GPU, or fold a shorter region.",
                    plasmid.MEMORY_GUARD_EXIT,
                )
            plasmid.warn(f"ran out of GPU memory at chunk {chunk}; trying {smaller}")
            chunk, retries = smaller, retries - 1
    if isinstance(out, list):
        out = out[0]
    peak = f", peak GPU {gib(torch.cuda.max_memory_allocated())}" if on_gpu else ""
    print(f"  folded in {t.seconds:.1f} s{peak}")
    guard.stop()

    cif = out.complex.to_mmcif()
    # The app colours by confidence when the title says where the B-factors came from.
    head, _, rest = cif.partition("\n")
    cif = f"{head}\n#\n_struct.title 'ESMFold2 prediction (B-factor = pLDDT)'\n{rest}"
    with open("prediction.cif", "w", encoding="utf-8") as fh:
        fh.write(cif)

    def scalar(x):
        if x is None:
            return None
        return float(x.float().mean()) if torch.is_tensor(x) else float(x)

    plddt = [100 * float(v) for v in out.plddt.float().flatten().cpu().tolist()] if out.plddt is not None else []
    metrics = {"pTM": scalar(out.ptm), "mean pLDDT": sum(plddt) / len(plddt) if plddt else None}
    if len(seqs) > 1:
        metrics["ipTM"] = scalar(out.iptm)
    metrics = {k: v for k, v in metrics.items() if v is not None}
    print("  " + ", ".join(f"{k} {v:.2f}" if v < 2 else f"{k} {v:.1f}" for k, v in metrics.items()))
    plasmid.result(
        kind="structure",
        file="prediction.cif",
        format="mmcif",
        chains=ids,
        source=f"{repo.split('/')[-1]}, seed {seed}",
        metrics=metrics,
        plddt=plddt,
    )


if __name__ == "__main__":
    if len(sys.argv) < 2 or sys.argv[1] != "run":
        sys.exit("usage: plugin.py run request.json")
    started = time.time()
    main()
    print(f"Done in {time.time() - started:.0f} s")
