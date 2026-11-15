//! Rejeux techniques S11, lignes et profils, dans un registre isolé.
mod cli_research_common;
mod common;
use tv4::report::stats;
#[test]
fn research_lines_and_profiles_apply_and_compare_the_delayed_rule() {
    let w = cli_research_common::World::with_dataset();
    let subset = w.write_subset("subset.json", &["BTC", "SOL"]);
    let costs = cli_research_common::repo("config/costs.toml");
    for profiled in [false, true] {
        let mut outputs = Vec::new();
        for delay in ["0", "1"] {
            let out = w.root.join(format!("replay-{profiled}-{delay}"));
            let mut extra = vec![
                "--okx-subset",
                subset.to_str().unwrap(),
                "--entry-delay-bars",
                delay,
            ];
            if profiled {
                extra.extend(["--costs", costs.to_str().unwrap()]);
            }
            cli_research_common::ok(&w.backtest_args(&out, &extra));
            outputs.push(out);
        }
        for line in ["full", "okx_usdc"] {
            for profile in if profiled {
                vec!["baseline", "adverse", "maker"]
            } else {
                vec![""]
            } {
                let base = stats::read_run(&outputs[0].join(line).join(profile)).unwrap();
                let candidate = stats::read_run(&outputs[1].join(line).join(profile)).unwrap();
                let tf = base
                    .stats_inputs()
                    .unwrap()
                    .inputs
                    .decision_timeframe_minutes;
                assert_eq!(
                    candidate.metrics()["config"]["sim_rules"]["extra_entry_delay_bars"],
                    tf
                );
                let result = stats::compare_delayed_entries(&base, &candidate).unwrap();
                assert_eq!(result.delay_minutes, tf);
                assert!(!result.eligible_dev_g2);
                assert_eq!(result.original.source.line.as_deref(), Some(line));
            }
        }
    }
}
