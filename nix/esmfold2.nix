# ESMFold2 (Biohub's `esm` package) as a Nix-built Python environment plus an
# `esmfold2-predict` CLI that folds a sequence and writes mmCIF — the format
# `src/structure.rs` already parses, so a plugin can shell out to it.
#
# `accel`:
#   "rocm" — AMD GPUs. Uses nixpkgs' torchWithRocm, which cache.nixos.org has
#            prebuilt (overriding `torch` the same way nixpkgs does keeps the
#            store path identical, so nothing heavy compiles locally).
#   "cpu"  — plain torch. Works anywhere but a 7B model on CPU is very slow.
#
# Weights (~26 GB per model) are not in the store: they download from Hugging
# Face on first use into $HF_HOME (default ~/.cache/huggingface).
{ pkgs, accel ? "rocm" }:
let
  inherit (pkgs) lib fetchurl;

  python = pkgs.python3.override {
    self = python;
    packageOverrides = pyself: pysuper: {
      torch =
        if accel == "rocm" then
          # Mirrors `torchWithRocm` in nixpkgs' python-packages.nix.
          pysuper.torch.override {
            triton = pyself.triton-no-cuda;
            rocmSupport = true;
            cudaSupport = false;
          }
        else
          pysuper.torch;

      # --- dependencies of `esm` that nixpkgs lacks -------------------------

      # Cython extension with no cp314 wheel, so it builds from the sdist.
      biotraj = pyself.buildPythonPackage rec {
        pname = "biotraj";
        version = "1.2.2";
        pyproject = true;
        src = fetchurl {
          url = "https://files.pythonhosted.org/packages/07/21/2287edfd0d2569639eea706e25c39e63b46a384cf1712db8ea05768317b0/biotraj-${version}.tar.gz";
          sha256 = "4bcba92101ed50f369cc1487fb5dfcfe1d8402ad47adaa9232b080553271663a";
        };
        build-system = with pyself; [ setuptools setuptools-scm wheel numpy cython ];
        dependencies = with pyself; [ numpy scipy ];
        doCheck = false;
        pythonImportsCheck = [ "biotraj" ];
      };

      # Compiled extensions with a heavier build; the manylinux wheel is simpler.
      biotite = pyself.buildPythonPackage rec {
        pname = "biotite";
        version = "1.6.0";
        format = "wheel";
        src = fetchurl ({
          x86_64-linux = {
            url = "https://files.pythonhosted.org/packages/62/b7/fb3b2fa155a3a5cf00f8e970e82a7554070c59a127b326375aa36d5452db/biotite-${version}-cp314-cp314-manylinux_2_24_x86_64.manylinux_2_28_x86_64.whl";
            sha256 = "ef558c9bb1715753acf5819593fd9f3f80bc2feaa02a7d9f0d7d8d15a284d182";
          };
          aarch64-linux = {
            url = "https://files.pythonhosted.org/packages/dd/65/b5358c885a9c04c3988678cb352e53edfb5cf234f8c653159d0830a58a42/biotite-${version}-cp314-cp314-manylinux_2_24_aarch64.manylinux_2_28_aarch64.whl";
            sha256 = "24cd941932987569be48b85219d7a97bacad865532a3e5b31998cc5f192f9fd7";
          };
        }.${pkgs.stdenv.hostPlatform.system});
        nativeBuildInputs = [ pkgs.autoPatchelfHook ];
        buildInputs = [ pkgs.stdenv.cc.cc.lib ];
        dependencies = with pyself; [ biotraj msgpack networkx numpy packaging requests ];
        pythonImportsCheck = [ "biotite.structure" ];
      };

      py3dmol = pyself.buildPythonPackage rec {
        pname = "py3dmol";
        version = "3.0.0";
        format = "wheel";
        src = fetchurl {
          url = "https://files.pythonhosted.org/packages/9f/8f/62ef705093d98efb99694c4118fb1fe54edafddfe872b7c1b562ed8ab8f7/py3dmol-${version}-py3-none-any.whl";
          sha256 = "9788d4386ed331032de348974740c4108aa47464f71b8b2a89441e8a08e8f6fb";
        };
        pythonImportsCheck = [ "py3Dmol" ];
      };

      pydssp = pyself.buildPythonPackage rec {
        pname = "pydssp";
        version = "0.9.2";
        format = "wheel";
        src = fetchurl {
          url = "https://files.pythonhosted.org/packages/58/61/b6fd322af887510c26bb70d353569092a45e7a1b0738344a9d64f3685e04/pydssp-${version}-py3-none-any.whl";
          sha256 = "14fbe43dbb9b3de3108fdc980483122a322c2a909442dec48e6697328a72478f";
        };
        dependencies = with pyself; [ numpy torch einops tqdm ];
      };

      dna-features-viewer = pyself.buildPythonPackage rec {
        pname = "dna-features-viewer";
        version = "3.1.5";
        format = "wheel";
        src = fetchurl {
          url = "https://files.pythonhosted.org/packages/52/f2/9e726e4489f947557290605e3ee33b6ba09fe5dac9213263e0ada75200cf/dna_features_viewer-${version}-py3-none-any.whl";
          sha256 = "a8a383d5340d35979be50224f12f4f426ab004394d9b9a33c99d59404469fb7a";
        };
        dependencies = with pyself; [ matplotlib biopython packaging ];
      };

      esm = pyself.buildPythonPackage rec {
        pname = "esm";
        version = "3.5.0";
        format = "wheel";
        src = fetchurl {
          url = "https://files.pythonhosted.org/packages/1d/93/494b56f0e8f845cef7c9cafdbd0a59de615a7d67cdbc9af5bb18eedf96ce/esm-${version}-py3-none-any.whl";
          sha256 = "86a728b874e3025cb98cb4720c8496f0f5a5241b609d8534a3ca8da724496738";
        };
        dependencies = with pyself; [
          torch transformers ipython einops biotite rdkit msgpack-numpy biopython
          scikit-learn brotli attrs pandas cloudpathlib httpx tenacity zstd
          ipywidgets py3dmol pydssp boto3 huggingface-hub safetensors pygtrie
          dna-features-viewer accelerate
        ] ++ lib.optional (lib.meta.availableOn pkgs.stdenv.hostPlatform pyself.triton) pyself.triton;
        # `triton` on the ROCm torch is triton-no-cuda; pip's metadata names `triton`.
        dontCheckRuntimeDeps = true;
        pythonImportsCheck = [ "esm.models.esmfold2" ];
      };
    };
  };

  env = python.withPackages (ps: [ ps.esm ]);

  predict = pkgs.writeShellApplication {
    name = "esmfold2-predict";
    runtimeInputs = [ env ];
    text = ''
      exec python ${./esmfold2_predict.py} "$@"
    '';
  };
in
{
  inherit python env predict;
}
