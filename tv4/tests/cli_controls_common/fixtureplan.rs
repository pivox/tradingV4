#![allow(dead_code)]
use super::helpers::*;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};
pub struct World {
    pub dir: tempfile::TempDir,
    pub sources: Vec<PathBuf>,
    pub plan: PathBuf,
    pub backtest: Vec<String>,
}
impl World {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        git(root, &["config", "user.name", "Test"]);
        git(root, &["config", "user.email", "test@example.invalid"]);
        let csv = crate::common::synthetic_candles(1800, 42, crate::common::T0, 0.)
            .iter()
            .map(|c| {
                format!(
                    "{},{},{},{},{},{},{}\n",
                    c.open_time_ms / 1000,
                    c.open,
                    c.high,
                    c.low,
                    c.close,
                    c.volume * 1000.,
                    c.trades.unwrap_or(1)
                )
            })
            .collect::<String>();
        fs::write(root.join("market.csv"), csv).unwrap();
        let strs = |v: Vec<&str>| v.into_iter().map(str::to_owned).collect::<Vec<_>>();
        ok(
            root,
            &strs(vec![
                "ingest",
                "csv",
                "--csv",
                "market.csv",
                "--exchange",
                "kraken",
                "--symbol",
                "ETH-USD",
                "--tf",
                "1m",
                "--root",
                ".",
            ]),
        );
        ok(
            root,
            &strs(vec![
                "dataset",
                "build",
                "--exchange",
                "kraken",
                "--symbol",
                "ETH-USD",
                "--tf",
                "1m",
                "--from",
                "2024-01-01T00:00:00Z",
                "--to",
                "2024-01-02T06:00:00Z",
                "--name",
                "technical",
                "--root",
                ".",
            ]),
        );
        fs::write(
            root.join("instruments.toml"),
            "[kraken.ETH-USD]\ntick_size=0.01\nlot_size=1e-6\nmin_qty=0.0\nmin_notional=0.0\n",
        )
        .unwrap();
        fs::copy(repo("config/fees.toml"), root.join("fees.toml")).unwrap();
        let mut backtest = strs(vec![
            "backtest",
            "--dataset",
            "technical",
            "--root",
            ".",
            "--symbols",
            "ETH-USD",
            "--strategy",
            "sma_cross",
            "--params",
            "params0.toml",
            "--fees-file",
            "fees.toml",
            "--instruments-file",
            "instruments.toml",
            "--research-root",
            ".",
            "--max-positions",
            "1",
            "--equity-step",
            "1m",
            "--min-trades",
            "1",
        ]);
        let mut sources = Vec::new();
        let mut parents = Vec::new();
        for i in 0..2 {
            let params = format!("params{i}.toml");
            fs::write(
                root.join(&params),
                format!("tf=\"15m\"\nfast={}\nslow=8\nstop_atr_mult=2.0\n", 3 + i),
            )
            .unwrap();
            let p = backtest.iter().position(|s| s == "--params").unwrap() + 1;
            backtest[p] = params.clone();
            let source = format!("source{i}");
            let mut args = backtest.clone();
            args.extend(["--out".into(), source.clone()]);
            ok(root, &args);
            let m = read(root.join(&source).join("metrics.json"));
            parents.push(json!({"config_hash":m["config_hash"],"params":file(root,&params)}));
            sources.push(root.join(source));
        }
        parents.sort_by(|a, b| a["config_hash"].as_str().cmp(&b["config_hash"].as_str()));
        let m = read(sources[0].join("metrics.json"));
        let plan = root.join("plan.json");
        let p = json!({"schema":"tv4-controls-plan-v1","parents":parents,"dataset_manifest":file(root,"datasets/technical.json"),"instruments":file(root,"instruments.toml"),"instruments_format":"toml","fees":file(root,"fees.toml"),"fees_grid":"okx_regular","costs":null,"universe":null,"subset":null,"line":null,"profile":null,"window":{"start_ms":m["data"]["window"]["start_ms"],"end_ms":m["data"]["window"]["end_ms"]},"initial_cash":300.,"symbols":["ETH-USD"],"regime_symbol":"BTC-USD","max_positions":1,"slippage_bps":5.,"entry_delay_bars":0,"equity_step":"1m","min_trades":1,"technical_nonqualifying":true,"options":{"seed":"7","replications":20,"modes":["preserve_btc","remove_btc"],"budgets":tv4::research::controls::Budgets::default()}});
        fs::write(&plan, serde_json::to_vec(&p).unwrap()).unwrap();
        git(
            root,
            &[
                "add",
                "plan.json",
                "params0.toml",
                "params1.toml",
                "instruments.toml",
                "fees.toml",
                "datasets/technical.json",
            ],
        );
        git(root, &["commit", "-qm", "recettes synthétiques"]);
        Self {
            dir,
            sources,
            plan,
            backtest,
        }
    }
    pub fn root(&self) -> &Path {
        self.dir.path()
    }
    pub fn args(&self, out: &str) -> Vec<String> {
        vec![
            "controls".into(),
            self.sources[0].display().to_string(),
            "--peer-run".into(),
            self.sources[1].display().to_string(),
            "--plan".into(),
            self.plan.display().to_string(),
            "--root".into(),
            self.root().display().to_string(),
            "--seed".into(),
            "7".into(),
            "--out".into(),
            self.root().join(out).display().to_string(),
        ]
    }
    pub fn edit_plan(&self, f: impl FnOnce(&mut Value)) {
        let mut p = read(&self.plan);
        f(&mut p);
        fs::write(&self.plan, serde_json::to_vec(&p).unwrap()).unwrap();
        git(self.root(), &["add", "plan.json"]);
        git(self.root(), &["commit", "-qm", "variante du plan"]);
    }
}
impl World {
    pub fn with_profile(profile: &str) -> Self {
        let mut w = Self::new();
        fs::copy(
            repo("tv4/tests/fixtures/costs_c1.toml"),
            w.root().join("costs.toml"),
        )
        .unwrap();
        let mut parents = Vec::new();
        for i in 0..2 {
            let mut args = w.backtest.clone();
            let p = args.iter().position(|s| s == "--params").unwrap() + 1;
            args[p] = format!("params{i}.toml");
            let out = w.root().join(format!("profiles{i}"));
            args.extend([
                "--costs".into(),
                "costs.toml".into(),
                "--out".into(),
                out.display().to_string(),
            ]);
            ok(w.root(), &args);
            w.sources[i] = out.join(profile);
            let m = read(w.sources[i].join("metrics.json"));
            parents.push(json!({"config_hash":m["config_hash"],"params":file(w.root(),&format!("params{i}.toml"))}));
        }
        parents.sort_by(|a, b| a["config_hash"].as_str().cmp(&b["config_hash"].as_str()));
        git(w.root(), &["add", "costs.toml"]);
        git(w.root(), &["commit", "-qm", "coûts synthétiques"]);
        w.edit_plan(|p| {
            p["parents"] = parents.into();
            p["costs"] = json!({"file":file(w.root(),"costs.toml"),"measures":null,"cost_seed":1});
            p["profile"] = profile.into();
        });
        w
    }
}
impl World {
    pub fn dev() -> Self {
        use tv4::{
            model::{Block, ExchangeId, Timeframe},
            store::{GapsPolicy, MultiBuildRequest, ResearchMeta, Window},
        };
        let mut w = Self::new();
        let start = Block::Dev.span().start_ms;
        let csv = crate::common::synthetic_candles(1800, 123, start, 0.)
            .iter()
            .map(|c| {
                format!(
                    "{},{},{},{},{},{},{}\n",
                    c.open_time_ms / 1000,
                    c.open,
                    c.high,
                    c.low,
                    c.close,
                    c.volume * 1000.,
                    c.trades.unwrap_or(1)
                )
            })
            .collect::<String>();
        fs::write(w.root().join("dev.csv"), csv).unwrap();
        ok(
            w.root(),
            &[
                "ingest",
                "csv",
                "--csv",
                "dev.csv",
                "--exchange",
                "kraken",
                "--symbol",
                "ETH-USD",
                "--tf",
                "1m",
                "--root",
                ".",
            ]
            .map(str::to_owned),
        );
        tv4::store::build_dataset_research(
            w.root(),
            &MultiBuildRequest {
                name: "dev",
                exchange: ExchangeId::Kraken,
                symbols: &["ETH-USD".parse().unwrap()],
                tf: Timeframe::M1,
                window: Window::new(start, start + 1800 * 60_000).unwrap(),
                gaps_policy: GapsPolicy::ZeroVolumeMarked,
            },
            &ResearchMeta {
                block: Block::Dev,
                universe: None,
                universe_quality: None,
            },
            future(),
        )
        .unwrap();
        crate::cli_research_common::init_research(w.root());
        let mut parents = Vec::new();
        for i in 0..2 {
            let params = format!("params{i}.toml");
            fs::write(
                w.root().join(&params),
                format!("tf=\"1m\"\nfast={}\nslow=8\nstop_atr_mult=2.0\n", 3 + i),
            )
            .unwrap();
            let mut args = w.backtest.clone();
            let p = args.iter().position(|s| s == "--params").unwrap() + 1;
            args[p] = params.clone();
            let p = args.iter().position(|s| s == "--dataset").unwrap() + 1;
            args[p] = "dev".into();
            let out = w.root().join(format!("devsource{i}"));
            args.extend([
                "--hypothesis".into(),
                "H-001".into(),
                "--out".into(),
                out.display().to_string(),
            ]);
            ok(w.root(), &args);
            w.sources[i] = out;
            let m = read(w.sources[i].join("metrics.json"));
            parents.push(json!({"config_hash":m["config_hash"],"params":file(w.root(),&params)}));
        }
        parents.sort_by(|a, b| a["config_hash"].as_str().cmp(&b["config_hash"].as_str()));
        git(
            w.root(),
            &["add", "params0.toml", "params1.toml", "datasets/dev.json"],
        );
        git(w.root(), &["commit", "-qm", "recettes DEV synthétiques"]);
        let m = read(w.sources[0].join("metrics.json"));
        w.edit_plan(|p| {
            p["parents"] = parents.into();
            p["dataset_manifest"] = file(w.root(), "datasets/dev.json");
            p["line"] = "full".into();
            p["window"]["start_ms"] = m["data"]["window"]["start_ms"].clone();
            p["window"]["end_ms"] = m["data"]["window"]["end_ms"].clone();
            p["technical_nonqualifying"] = false.into();
        });
        w
    }
}
impl World {
    pub fn dev_line(line: &str) -> Self {
        use tv4::{
            model::{
                Block, ExchangeId, Timeframe,
                universe::{PairSubset, SubsetRule, SubsetSource},
            },
            store::{GapsPolicy, MultiBuildRequest, ResearchMeta, Window},
        };
        let mut w = Self::dev();
        let start = Block::Dev.span().start_ms;
        ok(
            w.root(),
            &[
                "ingest",
                "csv",
                "--csv",
                "dev.csv",
                "--exchange",
                "kraken",
                "--symbol",
                "BTC-USD",
                "--tf",
                "1m",
                "--root",
                ".",
            ]
            .map(str::to_owned),
        );
        tv4::store::build_dataset_research(
            w.root(),
            &MultiBuildRequest {
                name: "devtwo",
                exchange: ExchangeId::Kraken,
                symbols: &["BTC-USD".parse().unwrap(), "ETH-USD".parse().unwrap()],
                tf: Timeframe::M1,
                window: Window::new(start, start + 1800 * 60_000).unwrap(),
                gaps_policy: GapsPolicy::ZeroVolumeMarked,
            },
            &ResearchMeta {
                block: Block::Dev,
                universe: None,
                universe_quality: None,
            },
            future(),
        )
        .unwrap();
        let instruments = "[kraken.ETH-USD]\ntick_size=0.01\nlot_size=1e-6\nmin_qty=0.0\nmin_notional=0.0\n[kraken.BTC-USD]\ntick_size=0.01\nlot_size=1e-6\nmin_qty=0.0\nmin_notional=0.0\n";
        fs::write(w.root().join("instruments.toml"), instruments).unwrap();
        let subset = PairSubset::new(
            "okx_usdc_tradable",
            SubsetSource {
                instruments_file: "synthetic.json".into(),
                instruments_hash: format!("blake3:{}", "a".repeat(64)),
                site: "eea".into(),
                fetched_at: "2026-10-30T10:15:00Z".into(),
            },
            SubsetRule {
                quote: "USDC".into(),
                state: "live".into(),
                trade_quote_ccy: true,
                aliases: Default::default(),
            },
            vec!["ETH".into()],
        )
        .unwrap();
        tv4::store::universe::write_pair_subset(&w.root().join("subset.json"), &subset).unwrap();
        let mut parents = Vec::new();
        for i in 0..2 {
            let mut args = w.backtest.clone();
            for (flag, value) in [
                ("--dataset", "devtwo".to_owned()),
                ("--symbols", "BTC-USD,ETH-USD".to_owned()),
                ("--params", format!("params{i}.toml")),
            ] {
                let p = args.iter().position(|a| a == flag).unwrap() + 1;
                args[p] = value;
            }
            let out = w.root().join(format!("lines{i}"));
            args.extend([
                "--hypothesis".into(),
                "H-001".into(),
                "--okx-subset".into(),
                "subset.json".into(),
                "--out".into(),
                out.display().to_string(),
            ]);
            ok(w.root(), &args);
            w.sources[i] = out.join(line);
            let m = read(w.sources[i].join("metrics.json"));
            parents.push(json!({"config_hash":m["config_hash"],"params":file(w.root(),&format!("params{i}.toml"))}));
        }
        parents.sort_by(|a, b| a["config_hash"].as_str().cmp(&b["config_hash"].as_str()));
        git(
            w.root(),
            &[
                "add",
                "instruments.toml",
                "datasets/devtwo.json",
                "subset.json",
            ],
        );
        git(w.root(), &["commit", "-qm", "deux lignes synthétiques"]);
        w.edit_plan(|p| {
            p["parents"] = parents.into();
            p["dataset_manifest"] = file(w.root(), "datasets/devtwo.json");
            p["instruments"] = file(w.root(), "instruments.toml");
            p["subset"] = file(w.root(), "subset.json");
            p["symbols"] = json!(["BTC-USD", "ETH-USD"]);
            p["line"] = line.into();
        });
        w
    }
}
