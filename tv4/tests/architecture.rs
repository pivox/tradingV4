//! Test de la règle de dépendance entre modules (docs/03-architecture.md §2).
//!
//! clippy ne sait pas vérifier qui `use` qui : on lit donc les sources de `tv4/src` et on cherche les
//! références `crate::<module>` et `tv4::<module>`. Le test est tolérant : un module qui n'existe pas
//! encore n'a aucun fichier à lire, donc aucune violation possible.
//!
//! Limite assumée : c'est une analyse de texte, pas du compilateur. Elle voit les `use`, les chemins
//! complets dans le code et les groupes `use crate::{a, b}`. Elle ne suit pas les alias de chemins
//! (`use crate as racine;`), qui n'ont pas de raison d'exister ici. Les `super::` sont résolus selon
//! la profondeur du fichier dans `src` : `super::x` dans `sim.rs`, `super::super::x` dans
//! `sim/orders.rs`. Un `super::` écrit dans un module en ligne (`mod tests { ... }`) n'est pas suivi :
//! la convention du projet est `crate::` dans les sous-fichiers.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Modules de `tv4/src` connus (docs/03 §4). Un nom qui n'est pas dans cette liste est ignoré.
const MODULES: [&str; 12] = [
    "model",
    "marketdata",
    "store",
    "mtf",
    "indicators",
    "strategy",
    "sim",
    "engine",
    "report",
    "cli",
    "research",
    "execution",
];

/// Règle d'un module : ce qu'il a le droit de référencer.
enum Rule {
    /// Seuls ces modules sont autorisés (le module peut toujours se référencer lui-même).
    OnlyAllowed(&'static [&'static str]),
    /// Tout est autorisé sauf ces modules.
    Forbidden(&'static [&'static str]),
}

/// Règles de dépendance de docs/03 §2.
fn rules() -> Vec<(&'static str, Rule)> {
    vec![
        // `model` ne dépend de rien.
        ("model", Rule::OnlyAllowed(&[])),
        // `strategy` ne dépend que de `model`.
        ("strategy", Rule::OnlyAllowed(&["model"])),
        // `indicators` ne dépend que de `model`.
        ("indicators", Rule::OnlyAllowed(&["model"])),
        // S2 : le lecteur Kraken ne dépend que de `model` ; à revoir explicitement en S5/S7 si besoin.
        ("marketdata", Rule::OnlyAllowed(&["model"])),
        // S5 : `mtf` (agrégation 1m vers TF, `closed_count`) ne dépend que de `model`.
        ("mtf", Rule::OnlyAllowed(&["model"])),
        // S4 : `store` ne dépend que de `model` ; `tv4 ingest csv` fait le lien avec `marketdata` dans la
        // CLI. Il ne connaît ni `engine`, ni `sim`, ni `strategy`, ni `report`, ni `cli`.
        ("store", Rule::OnlyAllowed(&["model"])),
        // `sim` ne dépend pas de `strategy` ; il ne remonte pas non plus vers l'orchestration.
        ("sim", Rule::Forbidden(&["strategy", "engine", "cli"])),
        // `engine` produit un résultat pur (run_cell, docs/03 §6-§7) : il n'écrit rien (c'est `report`)
        // et ne remonte pas vers la CLI.
        ("engine", Rule::Forbidden(&["cli", "report"])),
        // `report` lit les résultats de `engine`/`sim` ; il ne connaît pas les stratégies (id, version
        // et params lui arrivent dans `RunConfig`) et ne remonte pas vers la CLI.
        ("report", Rule::Forbidden(&["cli", "strategy"])),
        // S10 : `research` (calibration du stress, contrôles) lit le modèle, les données, `mtf` et les
        // indicateurs ; il ne produit aucun rapport, n'appelle pas la CLI et ne connaît pas les
        // stratégies (docs/specs/s10.md §15.1).
        ("research", Rule::Forbidden(&["cli", "report", "strategy"])),
    ]
}

/// Modules où la crate `chrono` est interdite : le temps y est un `i64` en millisecondes UTC.
const NO_CHRONO: [&str; 5] = ["model", "engine", "sim", "strategy", "mtf"];

/// Retire les commentaires de ligne (`// ...`, `/// ...`, `//! ...`) pour ne pas compter les mentions
/// dans la documentation. Un `//` situé dans une chaîne (`"https://..."`) n'est pas un commentaire.
/// Les commentaires de bloc ne sont pas utilisés dans le projet.
fn strip_line_comments(source: &str) -> String {
    let mut result = String::new();
    for line in source.lines() {
        let mut in_string = false;
        let mut previous = ' ';
        let mut cut = line.len();
        for (index, c) in line.char_indices() {
            if c == '"' && previous != '\\' {
                in_string = !in_string;
            }
            if !in_string && c == '/' && previous == '/' {
                cut = index - 1;
                break;
            }
            previous = c;
        }
        result.push_str(&line[..cut]);
        result.push('\n');
    }
    result
}

/// Identifiant qui commence `text` (lettres, chiffres, `_`) ; chaîne vide s'il n'y en a pas.
fn leading_ident(text: &str) -> &str {
    let end = text
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
        .map_or(text.len(), |(index, _)| index);
    &text[..end]
}

/// Noms de modules qui suivent le préfixe `prefix` (par exemple `crate::`) dans `text`.
///
/// Gère `crate::model::X` et le groupe `crate::{model::X, sim}`.
fn modules_after_prefix(text: &str, prefix: &str, found: &mut BTreeSet<String>) {
    let mut rest = text;
    while let Some(position) = rest.find(prefix) {
        // Le préfixe doit commencer un mot : on ignore `autre_crate::` pour `crate::`.
        let before = rest[..position].chars().last();
        let after_prefix = &rest[position + prefix.len()..];
        rest = after_prefix;
        if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        if let Some(group) = after_prefix.strip_prefix('{') {
            // Groupe : on prend le premier identifiant de chaque élément de premier niveau.
            let mut depth = 0;
            let mut element_start = true;
            let mut index = 0;
            let chars: Vec<char> = group.chars().collect();
            while index < chars.len() {
                let c = chars[index];
                match c {
                    '{' => {
                        depth += 1;
                        element_start = false;
                    }
                    '}' => {
                        if depth == 0 {
                            break;
                        }
                        depth -= 1;
                    }
                    ',' if depth == 0 => element_start = true,
                    c if c.is_whitespace() => {}
                    _ => {
                        if element_start && depth == 0 {
                            let tail: String = chars[index..].iter().collect();
                            let name = leading_ident(&tail).to_string();
                            if !name.is_empty() {
                                index += name.len() - 1;
                                found.insert(name);
                            }
                        }
                        element_start = false;
                    }
                }
                index += 1;
            }
        } else {
            let name = leading_ident(after_prefix);
            if !name.is_empty() {
                found.insert(name.to_string());
            }
        }
    }
}

/// Modules de `tv4` référencés par un fichier source.
///
/// `super_depth` est le nombre de `super::` qui mènent à la racine de la crate : 1 pour `module.rs`
/// et `module/mod.rs` (`super::x` désigne alors le module voisin `x`), 2 pour `module/fichier.rs`
/// (`super::super::x`), et ainsi de suite.
fn referenced_modules(source: &str, super_depth: usize) -> BTreeSet<String> {
    let code = strip_line_comments(source);
    let mut names = BTreeSet::new();
    modules_after_prefix(&code, "crate::", &mut names);
    modules_after_prefix(&code, "tv4::", &mut names);
    if super_depth > 0 {
        modules_after_prefix(&code, &"super::".repeat(super_depth), &mut names);
    }
    names
        .into_iter()
        .filter(|name| MODULES.contains(&name.as_str()))
        .collect()
}

/// `true` si la crate `chrono` est utilisée (`chrono::...` ou `use chrono`) : le mot `chrono` isolé,
/// hors commentaires.
fn uses_chrono(source: &str) -> bool {
    let code = strip_line_comments(source);
    let is_word_char = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut rest = code.as_str();
    while let Some(position) = rest.find("chrono") {
        let before = rest[..position].chars().last();
        let after = &rest[position + "chrono".len()..];
        rest = after;
        let word_start = !before.is_some_and(is_word_char);
        let word_end = !after.chars().next().is_some_and(is_word_char);
        if word_start && word_end {
            return true;
        }
    }
    false
}

/// Fichiers `.rs` du module : `src/<module>.rs` et tout `src/<module>/**/*.rs`, avec leur profondeur
/// de `super::` (voir [`referenced_modules`]). Liste vide si le module n'existe pas encore.
fn module_files(src: &Path, module: &str) -> Vec<(PathBuf, usize)> {
    let mut paths = Vec::new();
    let single = src.join(format!("{module}.rs"));
    if single.is_file() {
        paths.push(single);
    }
    let directory = src.join(module);
    if directory.is_dir() {
        collect_rs_files(&directory, &mut paths);
    }
    paths
        .into_iter()
        .map(|path| {
            let depth = super_depth(src, &path);
            (path, depth)
        })
        .collect()
}

/// Nombre de `super::` pour remonter de ce fichier à la racine de la crate.
///
/// `src/sim.rs` et `src/sim/mod.rs` valent 1 ; `src/sim/orders.rs` et `src/sim/a/mod.rs` valent 2.
fn super_depth(src: &Path, path: &Path) -> usize {
    let relative = path.strip_prefix(src).unwrap_or(path);
    let components = relative.components().count();
    if relative.file_name().is_some_and(|name| name == "mod.rs") {
        components - 1
    } else {
        components
    }
}

/// Parcourt `directory` en profondeur et ajoute les fichiers `.rs` à `files`.
fn collect_rs_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|e| e.path())
        .collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_rs_files(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
}

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|erreur| panic!("lecture de {} : {erreur}", path.display()))
}

#[test]
fn la_regle_de_dependance_est_respectee() {
    let src = src_dir();
    let mut violations = Vec::new();
    for (module, rule) in rules() {
        for (path, depth) in module_files(&src, module) {
            for name in referenced_modules(&read(&path), depth) {
                if name == module {
                    continue;
                }
                let allowed = match &rule {
                    Rule::OnlyAllowed(list) => list.contains(&name.as_str()),
                    Rule::Forbidden(list) => !list.contains(&name.as_str()),
                };
                if !allowed {
                    violations.push(format!(
                        "{} : le module « {module} » référence « {name} »",
                        path.display()
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "dépendances interdites (docs/03 §2) :\n{}",
        violations.join("\n")
    );
}

#[test]
fn personne_ne_depend_de_cli() {
    // `cli` n'a aucune logique métier : il appelle les autres modules, jamais l'inverse.
    let src = src_dir();
    let mut violations = Vec::new();
    for module in MODULES.iter().filter(|m| **m != "cli") {
        for (path, depth) in module_files(&src, module) {
            if referenced_modules(&read(&path), depth).contains("cli") {
                violations.push(path.display().to_string());
            }
        }
    }
    assert!(
        violations.is_empty(),
        "des modules dépendent de cli :\n{}",
        violations.join("\n")
    );
}

#[test]
fn chrono_est_absent_des_modules_de_calcul() {
    let src = src_dir();
    let mut violations = Vec::new();
    for module in NO_CHRONO {
        for (path, _) in module_files(&src, module) {
            if uses_chrono(&read(&path)) {
                violations.push(format!("{} utilise chrono", path.display()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "chrono est interdit dans model, engine, sim, strategy et mtf (temps = i64 ms UTC) :\n{}",
        violations.join("\n")
    );
}

#[test]
fn les_modules_du_jalon_existent() {
    // Garde-fou du test lui-même : si le dossier src bouge, les règles ci-dessus ne liraient plus rien.
    for module in ["model", "marketdata", "indicators", "strategy"] {
        assert!(
            !module_files(&src_dir(), module).is_empty(),
            "aucun fichier pour le module « {module} » dans src"
        );
    }
}

#[test]
fn les_regles_ne_sont_plus_vides() {
    // Sans fichier à lire, la règle « sim ne dépend pas de strategy » serait vacuement verte.
    for module in ["sim", "engine", "report", "store", "mtf"] {
        assert!(
            !module_files(&src_dir(), module).is_empty(),
            "aucun fichier pour le module « {module} » dans src"
        );
    }
}

#[test]
fn strategy_interdit_unsafe() {
    // Le bornage anti look-ahead de `BarContext` repose sur l'absence d'`unsafe` (docs/03 §1.2).
    let path = src_dir().join("strategy").join("mod.rs");
    let code = strip_line_comments(&read(&path));
    assert!(
        code.contains("#![forbid(unsafe_code)]"),
        "{} doit contenir #![forbid(unsafe_code)]",
        path.display()
    );
}

/// Champs privés de `TfView` et `BarContext` (`strategy/context.rs`).
const CONTEXT_FIELDS: [&str; 9] = [
    "views",
    "columns",
    "candles",
    "names",
    "closed",
    "state",
    "pending",
    "just_closed",
    "symbol",
];

/// Accès par champ (`.champ` non suivi de `(`) à l'un des champs de [`CONTEXT_FIELDS`] dans `code`.
fn context_field_accesses(code: &str) -> Vec<&'static str> {
    let mut found = Vec::new();
    for field in CONTEXT_FIELDS {
        let pattern = format!(".{field}");
        for (index, _) in code.match_indices(&pattern) {
            let rest = &code[index + pattern.len()..];
            let next = rest.chars().next().unwrap_or(' ');
            let is_identifier = next.is_alphanumeric() || next == '_';
            if !is_identifier && next != '(' && !found.contains(&field) {
                found.push(field);
            }
        }
    }
    found
}

#[test]
fn les_strategies_ne_lisent_pas_les_champs_du_contexte() {
    // Un champ privé reste visible des modules descendants du module qui définit le type : `TfView` et
    // `BarContext` vivent donc dans `strategy/context.rs`, et aucun autre fichier de `strategy/` ne
    // doit accéder à leurs champs (`ctx.views`, `.columns`...), sous peine de lire au-delà de `closed`
    // sans `unsafe`. Seuls les accesseurs bornés (`ctx.candles(tf)`, `ctx.ind(tf, name)`) sont permis.
    let directory = src_dir().join("strategy");
    assert!(directory.join("context.rs").is_file(), "context.rs absent");
    let mut violations = Vec::new();
    for (path, _) in module_files(&src_dir(), "strategy") {
        if path.file_name().is_some_and(|name| name == "context.rs") {
            continue;
        }
        let code = strip_line_comments(&read(&path));
        for field in context_field_accesses(&code) {
            violations.push(format!("{} accède au champ `.{field}`", path.display()));
        }
    }
    assert!(
        violations.is_empty(),
        "accès aux champs du contexte hors de strategy/context.rs :\n{}",
        violations.join("\n")
    );
}

#[test]
fn l_analyseur_de_champs_distingue_champ_et_methode() {
    assert!(context_field_accesses("ctx.candles(tf)").is_empty());
    assert!(context_field_accesses("ctx.just_closed(tf)").is_empty());
    assert_eq!(context_field_accesses("ctx.views[0]"), vec!["views"]);
    assert_eq!(
        context_field_accesses("let n = v.columns.len();"),
        vec!["columns"]
    );
    assert!(context_field_accesses("self.candles_count").is_empty());
}

/// `true` si la crate `name` est utilisée dans `source`, hors commentaires : soit en tête de chemin
/// (`name::...`), soit importée sous un alias ou en bloc (`use name as a;`, `use name;`,
/// `extern crate name as a;`), cas où le chemin ne contient plus `name::`.
///
/// Un `name::` précédé d'un caractère d'identifiant (`my_arrow::f()`) est un autre nom. Un `name::`
/// précédé de `::` est un chemin de module (`store::parquet::`, `self::parquet::`) quand le caractère
/// avant ce `::` est un identifiant ou `>` ; sinon (`::parquet::` en tête de chemin) c'est la crate.
fn uses_crate(source: &str, name: &str) -> bool {
    let code = strip_line_comments(source);
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let pattern = format!("{name}::");
    for (position, _) in code.match_indices(&pattern) {
        let before = &code[..position];
        if before.chars().next_back().is_some_and(is_ident) {
            continue;
        }
        if let Some(path) = before.strip_suffix("::")
            && path
                .chars()
                .next_back()
                .is_some_and(|c| is_ident(c) || c == '>')
        {
            continue;
        }
        return true;
    }
    imports_crate_without_path(&code, name)
}

/// `use name;`, `use name as a;`, `use ::name as a;`, `use name::{...}` et `extern crate name ...;`
/// (`code` sans commentaires) : formes qui introduisent la crate sans la suivre de `::`.
fn imports_crate_without_path(code: &str, name: &str) -> bool {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    for keyword in ["use", "extern crate"] {
        for (position, _) in code.match_indices(keyword) {
            if code[..position].chars().next_back().is_some_and(is_ident) {
                continue;
            }
            let rest = &code[position + keyword.len()..];
            // Le mot-clé doit être suivi d'un espace (`user`, `used` sont d'autres mots).
            if !rest.starts_with(char::is_whitespace) {
                continue;
            }
            let rest = rest.trim_start();
            let rest = rest.strip_prefix('{').unwrap_or(rest).trim_start();
            let rest = rest.strip_prefix("::").unwrap_or(rest);
            let Some(after) = rest.strip_prefix(name) else {
                continue;
            };
            if after.chars().next().is_some_and(is_ident) {
                continue;
            }
            let after = after.trim_start();
            if after.starts_with(';')
                || after.starts_with(',')
                || after.starts_with('}')
                || after.starts_with("as ")
                || after.starts_with("as\t")
                || after.starts_with("as\n")
            {
                return true;
            }
        }
    }
    false
}

/// Fichiers `.rs` de `src`, `tests` et `benches` du paquet.
fn all_rs_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for directory in ["src", "tests", "benches"] {
        collect_rs_files(&root.join(directory), &mut files);
    }
    files
}

#[test]
fn arrow_et_parquet_confines_a_store_parquet() {
    // docs/03 §10 ligne S4 : l'API arrow est la plus verbeuse du jalon, elle reste dans un seul
    // fichier. Les tests d'intégration ni les benchs n'en voient aucun type : ils passent par
    // `Columns`, `read_parquet` et les autres types du store.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let autorise = root.join("src").join("store").join("parquet.rs");
    let ce_fichier = root.join("tests").join("architecture.rs");
    assert!(autorise.is_file(), "{} absent", autorise.display());
    assert!(
        uses_crate(&read(&autorise), "parquet"),
        "garde-fou : store/parquet.rs devrait utiliser la crate parquet"
    );
    let violations: Vec<String> = all_rs_files()
        .into_iter()
        .filter(|path| *path != autorise && *path != ce_fichier)
        .filter(|path| {
            let source = read(path);
            uses_crate(&source, "arrow") || uses_crate(&source, "parquet")
        })
        .map(|path| path.display().to_string())
        .collect();
    assert!(
        violations.is_empty(),
        "arrow ou parquet utilisés hors de store/parquet.rs :\n{}",
        violations.join("\n")
    );
}

#[test]
fn l_analyseur_de_crate_voit_arrow_et_parquet() {
    assert!(uses_crate("use parquet::arrow::ArrowWriter;", "parquet"));
    assert!(uses_crate("use arrow::array::Int64Array;", "arrow"));
    assert!(uses_crate(
        "let t = ::arrow::datatypes::DataType::Int64;",
        "arrow"
    ));
    assert!(uses_crate("Vec<arrow::array::ArrayRef>", "arrow"));
}

#[test]
fn l_analyseur_de_crate_voit_les_alias() {
    // Après un alias, plus aucun `arrow::` dans le code : le confinement doit quand même le voir.
    assert!(uses_crate("use arrow as a;", "arrow"));
    assert!(uses_crate("use ::arrow as a;", "arrow"));
    assert!(uses_crate("pub use arrow as a;", "arrow"));
    assert!(uses_crate("extern crate parquet as p;", "parquet"));
    assert!(uses_crate("extern crate arrow;", "arrow"));
    assert!(uses_crate("use arrow;", "arrow"));
    assert!(uses_crate("use\n    arrow\n    as a;", "arrow"));
    assert!(uses_crate("use arrow::{array::Int64Array};", "arrow"));
    assert!(uses_crate("use {arrow, serde};", "arrow"));
}

#[test]
fn l_analyseur_de_crate_ignore_les_chemins_de_module() {
    assert!(!uses_crate(
        "use crate::store::parquet::read_parquet;",
        "parquet"
    ));
    assert!(!uses_crate(
        "pub use self::parquet::{read_parquet, ReadOptions};",
        "parquet"
    ));
    assert!(!uses_crate("use super::parquet::write_parquet;", "parquet"));
    assert!(!uses_crate("mod parquet;", "parquet"));
    assert!(!uses_crate("// use parquet::x;", "parquet"));
    assert!(!uses_crate("my_arrow::f();", "arrow"));
    assert!(!uses_crate("let arrow_count = 1;", "arrow"));
    // Un alias vers un module local ou un autre nom n'est pas la crate.
    assert!(!uses_crate("use self::parquet as p;", "parquet"));
    assert!(!uses_crate("use crate::store::parquet as p;", "parquet"));
    assert!(!uses_crate("use arrow_schema as a;", "arrow"));
    assert!(!uses_crate("let user = arrow;", "arrow"));
    assert!(!uses_crate("// use arrow as a;", "arrow"));
}

// --- Tests de l'analyseur lui-même (il doit voir les violations qu'il est censé voir) ---

fn names(source: &str, super_depth: usize) -> Vec<String> {
    referenced_modules(source, super_depth)
        .into_iter()
        .collect()
}

#[test]
fn l_analyseur_voit_un_use_simple() {
    assert_eq!(names("use crate::sim::Portfolio;", 0), ["sim"]);
    assert_eq!(
        names("let x = crate::strategy::Signal::Hold;", 0),
        ["strategy"]
    );
}

#[test]
fn l_analyseur_voit_les_groupes() {
    let source = "use crate::{model::Candle, sim::{Order, Fill}, engine};";
    assert_eq!(names(source, 0), ["engine", "model", "sim"]);
}

#[test]
fn l_analyseur_voit_un_groupe_sur_plusieurs_lignes() {
    let source = "use crate::{\n    model::Candle,\n    strategy::Signal,\n};";
    assert_eq!(names(source, 0), ["model", "strategy"]);
}

#[test]
fn l_analyseur_voit_le_chemin_par_le_nom_de_la_crate() {
    assert_eq!(names("use tv4::engine::run_cell;", 0), ["engine"]);
}

#[test]
fn l_analyseur_ignore_les_commentaires_et_les_autres_crates() {
    assert!(names("// use crate::strategy::Signal;", 0).is_empty());
    assert!(names("/// Voir `crate::sim`.", 0).is_empty());
    assert!(names("use autre_crate::sim::X;", 0).is_empty());
    assert!(names("use crate::inconnu::X;", 0).is_empty());
}

#[test]
fn l_analyseur_resout_super_selon_la_profondeur_du_fichier() {
    // `sim.rs` ou `sim/mod.rs` (profondeur 1) : `super::sim` est le module voisin.
    assert_eq!(names("use super::sim::Portfolio;", 1), ["sim"]);
    assert!(names("use super::sim::Portfolio;", 0).is_empty());
    // `sim/orders.rs` (profondeur 2) : il faut deux `super::` pour atteindre la racine.
    assert_eq!(
        names("use super::super::strategy::Signal;", 2),
        ["strategy"]
    );
    // Un seul `super::` y désigne le module `sim` lui-même, pas la racine.
    assert!(names("use super::fills::Fill;", 2).is_empty());
}

#[test]
fn la_profondeur_de_super_vient_du_chemin_du_fichier() {
    let src = Path::new("/x/src");
    assert_eq!(super_depth(src, Path::new("/x/src/sim.rs")), 1);
    assert_eq!(super_depth(src, Path::new("/x/src/sim/mod.rs")), 1);
    assert_eq!(super_depth(src, Path::new("/x/src/sim/orders.rs")), 2);
    assert_eq!(super_depth(src, Path::new("/x/src/sim/a/mod.rs")), 2);
}

#[test]
fn l_analyseur_ne_coupe_pas_une_chaine_contenant_deux_barres() {
    // Le `//` de l'URL est dans une chaîne : le `use` qui suit sur la même ligne reste visible.
    let source = "let u = \"https://exemple.org\"; let s = crate::strategy::Signal::Hold; // note";
    assert_eq!(names(source, 0), ["strategy"]);
    assert!(names("let x = 1; // crate::strategy::Signal", 0).is_empty());
}

#[test]
fn l_analyseur_detecte_chrono() {
    assert!(uses_chrono("use chrono::Utc;"));
    assert!(uses_chrono("let t = ::chrono::Utc::now();"));
    assert!(!uses_chrono("// chrono est interdit ici"));
    assert!(!uses_chrono("let chronometre = 1;"));
}

#[test]
fn les_fichiers_d_un_module_absent_sont_une_liste_vide() {
    assert!(module_files(&src_dir(), "execution").is_empty());
}

/// Appels de la libm interdits hors de `report/approx.rs` (docs/03 §7.4) : leurs résultats ne sont
/// pas exacts au bit d'une machine à l'autre. Forme méthode (`x.ln()`) et forme chemin (`f64::ln(x)`).
const LIBM_CALLS: [&str; 17] = [
    ".ln(", ".ln_1p(", ".log(", ".log2(", ".log10(", ".exp(", ".exp2(", ".exp_m1(", ".powf(",
    ".powi(", ".sin(", ".cos(", ".tan(", ".cbrt(", ".hypot(", ".atan(", ".atan2(",
];

/// Formes chemin des mêmes appels, pour `f64` et `f32`.
const LIBM_PATH_CALLS: [&str; 8] = [
    "f64::ln(",
    "f64::exp(",
    "f64::powf(",
    "f64::powi(",
    "f64::log10(",
    "f32::ln(",
    "f32::exp(",
    "f32::powf(",
];

/// Appels de la libm trouvés dans `source` (commentaires retirés).
fn libm_calls(source: &str) -> Vec<&'static str> {
    let code = strip_line_comments(source);
    LIBM_CALLS
        .iter()
        .chain(LIBM_PATH_CALLS.iter())
        .filter(|call| code.contains(**call))
        .copied()
        .collect()
}

#[test]
fn la_libm_n_est_appelee_que_dans_report_approx() {
    let src = src_dir();
    let mut files = Vec::new();
    collect_rs_files(&src, &mut files);
    assert!(files.len() > 40, "{} fichiers lus", files.len());
    let approx = src.join("report").join("approx.rs");
    assert!(approx.is_file(), "report/approx.rs absent");
    let mut violations = Vec::new();
    for path in files {
        if path == approx {
            continue;
        }
        for call in libm_calls(&read(&path)) {
            violations.push(format!("{} appelle `{call}`", path.display()));
        }
    }
    assert!(
        violations.is_empty(),
        "la libm n'est permise que dans report/approx.rs (docs/03 §7.4) :\n{}",
        violations.join("\n")
    );
    // Le seul fichier autorisé s'en sert bien : la règle n'est pas vacuement verte.
    let calls = libm_calls(&read(&approx));
    assert!(
        calls.contains(&".ln(") && calls.contains(&".powf("),
        "{calls:?}"
    );
}

#[test]
fn l_analyseur_de_libm_distingue_appel_commentaire_et_expect() {
    assert_eq!(libm_calls("let y = x.ln();"), [".ln("]);
    assert_eq!(libm_calls("let y = f64::powf(x, 2.0);"), ["f64::powf("]);
    assert!(libm_calls("// x.ln() est interdit ici").is_empty());
    assert!(libm_calls("let y = r.expect(\"ok\");").is_empty());
    assert!(libm_calls("let y = x.exp_like();").is_empty());
}

// --- S10 : un seul générateur pour la simulation (docs/specs/s10.md §15.1) ---

/// Crates d'aléa interdites dans `src/` : le générateur de la simulation est `model::rng`, écrit dans
/// le dépôt (SplitMix64 à compteur). `rand` n'est dans `Cargo.lock` qu'à travers `proptest`.
const FORBIDDEN_RNG_CRATES: [&str; 3] = ["rand", "rand_chacha", "rand_core"];

#[test]
fn aucun_fichier_de_src_n_utilise_la_crate_rand() {
    let src = src_dir();
    let mut files = Vec::new();
    collect_rs_files(&src, &mut files);
    assert!(files.len() > 40, "{} fichiers lus", files.len());
    let mut violations = Vec::new();
    for path in files {
        let source = read(&path);
        for name in FORBIDDEN_RNG_CRATES {
            if uses_crate(&source, name) {
                violations.push(format!("{} utilise la crate `{name}`", path.display()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "l'aléa de la simulation est `model::rng`, sans crate (s10 §8) :\n{}",
        violations.join("\n")
    );
}

#[test]
fn l_analyseur_voit_la_crate_rand_sans_confondre_les_noms_voisins() {
    assert!(uses_crate("use rand::Rng;", "rand"));
    assert!(uses_crate("let x = rand::random::<u64>();", "rand"));
    assert!(uses_crate("use rand_chacha::ChaCha8Rng;", "rand_chacha"));
    assert!(uses_crate("use rand as r;", "rand"));
    assert!(!uses_crate("let operand = 1; operand::f();", "rand"));
    assert!(!uses_crate("let grand_total = 1;", "rand"));
    assert!(!uses_crate("use crate::model::rng::draw;", "rand"));
    assert!(!uses_crate("// use rand::Rng;", "rand"));
}

/// La constante d'or de SplitMix64, sans tiret bas et en minuscules.
const SPLITMIX_GAMMA: &str = "0x9e3779b97f4a7c15";

/// `true` si `text` contient la constante d'or de SplitMix64, quelle que soit l'écriture (tirets bas,
/// majuscules). Les commentaires comptent : la mention vaut la constante.
fn mentions_splitmix_gamma(text: &str) -> bool {
    let normalized: String = text
        .chars()
        .filter(|c| *c != '_')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    normalized.contains(SPLITMIX_GAMMA)
}

#[test]
fn la_constante_splitmix_n_a_que_deux_maisons() {
    // `model/rng.rs` est le générateur de la simulation ; `marketdata/okx/bucket.rs` n'a que le jitter
    // des retries réseau. Une troisième copie serait un deuxième générateur à garder identique.
    let src = src_dir();
    let autorises = [
        src.join("model").join("rng.rs"),
        src.join("marketdata").join("okx").join("bucket.rs"),
    ];
    let mut files = Vec::new();
    collect_rs_files(&src, &mut files);
    let mut violations = Vec::new();
    for path in &files {
        if mentions_splitmix_gamma(&read(path)) && !autorises.contains(path) {
            violations.push(path.display().to_string());
        }
    }
    assert!(
        violations.is_empty(),
        "la constante 0x9E37_79B9_7F4A_7C15 n'a le droit d'apparaître que dans model/rng.rs et \
         marketdata/okx/bucket.rs :\n{}",
        violations.join("\n")
    );
    // La règle n'est pas vacuement verte : les deux fichiers autorisés la portent bien.
    for path in &autorises {
        assert!(path.is_file(), "{} absent", path.display());
        assert!(
            mentions_splitmix_gamma(&read(path)),
            "{} devrait porter la constante",
            path.display()
        );
    }
}

#[test]
fn l_analyseur_de_constante_voit_toutes_les_ecritures() {
    assert!(mentions_splitmix_gamma(
        "const G: u64 = 0x9E37_79B9_7F4A_7C15;"
    ));
    assert!(mentions_splitmix_gamma("0x9e3779b97f4a7c15"));
    assert!(mentions_splitmix_gamma(
        "// constante d'or 0x9E37_79B9_7F4A_7C15"
    ));
    assert!(mentions_splitmix_gamma(
        "0X9E3779B9_7F4A7C15".replace('X', "x").as_str()
    ));
    assert!(!mentions_splitmix_gamma("0x9E37_79B9_7F4A_7C16"));
    assert!(!mentions_splitmix_gamma("let x = 0xBF58_476D_1CE4_E5B9;"));
}

#[test]
fn la_regle_research_est_declaree() {
    // Le module `research` arrive avec le lot de calibration : sa règle doit déjà être là.
    let rule = rules()
        .into_iter()
        .find(|(module, _)| *module == "research")
        .map(|(_, rule)| rule);
    match rule {
        Some(Rule::Forbidden(list)) => {
            for interdit in ["cli", "report", "strategy"] {
                assert!(
                    list.contains(&interdit),
                    "research doit interdire {interdit}"
                );
            }
        }
        _ => panic!("règle `research` absente ou de mauvaise forme"),
    }
}
