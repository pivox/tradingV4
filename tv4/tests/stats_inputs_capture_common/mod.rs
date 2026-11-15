use std::{fs, path::Path};
use tv4::report::{
    stats,
    stats_inputs::{STATS_INPUTS_FILE, StatsInputs},
};
pub fn check(dir: &Path) -> StatsInputs {
    let bytes = fs::read(dir.join(STATS_INPUTS_FILE)).expect("nouveau backtest : paquet complet");
    let packet: StatsInputs = serde_json::from_slice(&bytes).unwrap();
    let run = stats::read_run(dir).unwrap();
    packet.validate_for(&run).unwrap();
    assert_eq!(
        run.manifest()["stats_inputs"]["hash"],
        tv4::model::hash::blake3_prefixed(&bytes)
    );
    assert_eq!(
        packet.cash.first().unwrap().at_ms,
        packet.provenance.window.start_ms
    );
    assert_eq!(
        packet.cash.last().unwrap().at_ms,
        packet.provenance.window.end_ms
    );
    assert_eq!(
        packet.marks.first().unwrap().equity,
        packet.provenance.initial_cash
    );
    assert_eq!(
        packet.marks.last().unwrap().at_ms,
        packet.provenance.window.end_ms
    );
    packet
}
