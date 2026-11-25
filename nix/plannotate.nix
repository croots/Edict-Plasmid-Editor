# pLannotate (https://github.com/mmcguffi/pLannotate, GPL-3.0-only) with the search
# tools it shells out to and its annotation databases, for the pLannotate plugin.
#
# pLannotate looks for its databases inside its own package folder
# (`plannotate/data/{BLAST_dbs,diamond_dbs,infernal_dbs}`), which `plannotate setupdb`
# fills by downloading a bundle from the project's GitHub release. In the Nix store
# that folder is read-only, so the bundle is fetched here instead (pinned by hash)
# and linked into place: `plannotate batch` works with no setup step.
#
# Nothing here is distributed with Edict: each machine builds it from the upstream
# sources when the plugin's environment is first used.
{ pkgs }:
let
  inherit (pkgs) lib;
  version = "2.0.0";

  # Rfam covariance-model search. Not in nixpkgs.
  infernal = pkgs.stdenv.mkDerivation rec {
    pname = "infernal";
    version = "1.1.5";
    src = pkgs.fetchurl {
      url = "http://eddylab.org/infernal/infernal-${version}.tar.gz";
      sha256 = "0zs9abkwp1d95xq2ddxfjvgzhnl7kyfagi68bg4afk4j5zhdlkdd";
    };
    enableParallelBuilding = true;
    meta = {
      description = "Search DNA sequence databases for RNA structure and sequence similarities";
      homepage = "http://eddylab.org/infernal/";
      license = lib.licenses.bsd3;
      platforms = lib.platforms.linux;
    };
  };

  # The database bundle `plannotate setupdb` would download for this release
  # (SnapGene features for BLAST, FPbase and Swiss-Prot for DIAMOND, Rfam for
  # Infernal). About 256 MB.
  databases = pkgs.stdenvNoCC.mkDerivation {
    pname = "plannotate-databases";
    version = "2";
    src = pkgs.fetchurl {
      url = "https://github.com/mmcguffi/pLannotate/releases/download/v${version}/plannotate-databases-v2.tar.gz";
      sha256 = "50b958f1e55119659a5df0eaafbbdfb0f34d29e3e48c8cdb8a50f9a344095589";
    };
    sourceRoot = ".";
    dontConfigure = true;
    dontBuild = true;
    dontFixup = true;
    installPhase = ''
      mkdir -p $out
      cp -r BLAST_dbs diamond_dbs infernal_dbs database-manifest.json $out/
    '';
  };

  tools = [ pkgs.blast pkgs.diamond infernal ];

  python = pkgs.python3;

  plannotate = python.pkgs.buildPythonApplication {
    pname = "plannotate";
    inherit version;
    pyproject = true;
    src = pkgs.fetchFromGitHub {
      owner = "mmcguffi";
      repo = "pLannotate";
      rev = "v${version}";
      hash = "sha256-1aUvfB0HHj6nl5C3EU8vV6kvvnU2PVR19czRPGV6TrI=";
    };
    build-system = [ python.pkgs.setuptools ];
    dependencies = with python.pkgs; [ biopython numpy pandas pyyaml rich typer ];
    # It asks for pandas < 3; nixpkgs has 3.x, which it annotates pUC19 fine with.
    pythonRelaxDeps = [ "pandas" ];
    makeWrapperArgs = [ "--prefix" "PATH" ":" (lib.makeBinPath tools) ];
    postInstall = ''
      data=$out/${python.sitePackages}/plannotate/data
      for d in BLAST_dbs diamond_dbs infernal_dbs database-manifest.json; do
        ln -s ${databases}/$d $data/$d
      done
    '';
    doCheck = false;
    pythonImportsCheck = [ "plannotate" ];

    passthru = { inherit databases infernal; };

    meta = {
      description = "Annotate engineered DNA sequences and plasmids";
      homepage = "https://github.com/mmcguffi/pLannotate";
      license = lib.licenses.gpl3Only;
      mainProgram = "plannotate";
    };
  };
in
{
  inherit plannotate databases infernal;

  # The Python the plugin runs in: `plannotate` on PATH (wrapped so BLAST, DIAMOND
  # and Infernal are on its PATH), and Biopython to read the GenBank it writes.
  env = pkgs.buildEnv {
    name = "plannotate-env";
    paths = [ (python.withPackages (ps: [ ps.biopython ])) plannotate ] ++ tools;
  };
}
