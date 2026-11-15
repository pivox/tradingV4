"""Fumée technique S13 : deux processus, sources et registre immuables."""

import argparse
import hashlib
import json
import subprocess
from pathlib import Path


def digest(path: Path) -> str | None:
    return hashlib.sha256(path.read_bytes()).hexdigest() if path.exists() else None


def source_image(leaves: list[Path]) -> dict[Path, str | None]:
    names = ("manifest.json", "metrics.json", "trades.csv", "equity.csv", "stats-inputs.json")
    image = {}
    for leaf in leaves:
        for name in names:
            path = leaf / name
            if not path.is_file():
                raise AssertionError(f"source S13 absente : {path}")
            image[path] = digest(path)
    return image


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--research-root", type=Path, required=True)
    parser.add_argument("runs", type=Path, nargs="+")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    study = args.research_root.resolve(strict=True)
    registry_paths = [study / "research" / name for name in ("trials.csv", "README.md")]
    registry = {path: digest(path) for path in registry_paths}
    seed = "18446744073709551615"
    for root in args.runs:
        root = root.resolve(strict=True)
        leaves = sorted(path.parent for path in root.rglob("manifest.json"))
        if not leaves:
            raise AssertionError(f"aucun run dans {root}")
        before = source_image(leaves)
        command = [str(binary), "stats", str(root), "--seed", seed, "--research-root", str(study)]
        subprocess.run(command, check=True)
        first = {leaf: (leaf / "stats.json").read_bytes() for leaf in leaves}
        for leaf, raw in first.items():
            value = json.loads(raw)
            if value.get("schema") != "tv4-stats-v1" or value.get("options", {}).get("seed") != seed:
                raise AssertionError(f"schéma ou graine u64 non canonique : {leaf}")
        subprocess.run(command, check=True)
        for leaf, raw in first.items():
            if raw != (leaf / "stats.json").read_bytes():
                raise AssertionError(f"stats.json non déterministe : {leaf}")
        if before != source_image(leaves):
            raise AssertionError(f"un post-traitement a modifié ses sources : {root}")
        if registry != {path: digest(path) for path in registry_paths}:
            raise AssertionError("stats a modifié le registre ou ses compteurs")
        subprocess.run([str(binary), "verify", str(root), str(root)], check=True)
        print(f"S13 : {len(leaves)} feuille(s), sources et registre inchangés, deux processus identiques")


if __name__ == "__main__":
    main()
