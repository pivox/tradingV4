"""Checkout Git éphémère pour les tests de plomberie de CI, sans recherche économique."""
import pathlib
import subprocess
import sys

root = pathlib.Path(sys.argv[1])
(root / "research").mkdir(parents=True, exist_ok=False)
model = pathlib.Path("research/H-000-modele.md").read_text(encoding="utf-8")
fields = [line.split(":", 1)[0].strip() for line in model.split("```text\n", 1)[1].split("```", 1)[0].splitlines() if ":" in line]
assert len(fields) == 18
fiche = "# Fumée de CI, sans hypothèse économique\n\n" + "".join(
    f"{field} : {'H-001' if field == 'H-NNN' else 'Plomberie CI uniquement ; aucun résultat de recherche'}\n"
    for field in fields
)
(root / "research/H-001.md").write_text(fiche, encoding="utf-8")
subprocess.run(["git", "init", "-q", str(root)], check=True)
subprocess.run(["git", "-C", str(root), "add", "research/H-001.md"], check=True)
subprocess.run(["git", "-C", str(root), "-c", "user.name=Fumée CI", "-c", "user.email=ci@example.invalid", "commit", "-qm", "Fiche du test de plomberie"], check=True)
