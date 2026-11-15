//! Sélection exacte BASE-QUOTE parmi les seuls marchés admis.
use super::*;
use std::collections::BTreeSet;
pub(super) fn resolve(
    args: &StatsArgs,
    pairs: &[String],
    markets: &[MarketSeries],
) -> Result<(SymbolSelection, BenchmarkSymbols), CliError> {
    let quotes: BTreeSet<_> = pairs
        .iter()
        .filter_map(|p| p.split_once('-').map(|(_, q)| q.to_owned()))
        .collect();
    let explicit_quotes = args
        .btc_symbol
        .iter()
        .chain(args.eth_symbol.iter())
        .filter_map(|s| s.split_once('-').map(|(_, q)| q.to_owned()))
        .collect::<BTreeSet<_>>();
    if explicit_quotes.len() > 1 {
        return Err(usage(
            "sélecteurs BTC/ETH de quotes différentes : aucune conversion FX implicite",
        ));
    }
    let quote = explicit_quotes.first().cloned().or_else(|| {
        if quotes.len() == 1 {
            quotes.first().cloned()
        } else {
            None
        }
    });
    let mut reasons = Vec::new();
    let mut select = |base: &str, explicit: &Option<String>| -> Result<Option<String>, CliError> {
        if let Some(symbol) = explicit {
            let Some((b, q)) = symbol.split_once('-') else {
                return Err(usage(format!(
                    "--{}-symbol : BASE-QUOTE requis",
                    base.to_lowercase()
                )));
            };
            if b != base || !quotes.contains(q) {
                return Err(usage(format!(
                    "sélecteur {symbol} : base {base} et quote de l'univers joué requises"
                )));
            }
            if !markets.iter().any(|m| &m.symbol == symbol) {
                return Err(usage(format!(
                    "sélecteur {symbol} absent des marchés admis"
                )));
            }
            return Ok(Some(symbol.clone()));
        }
        if quote.is_none() {
            reasons.push(format!(
                "quote ambiguë dans l'univers joué : préciser --{}-symbol",
                base.to_lowercase()
            ));
            return Ok(None);
        }
        let candidates = markets
            .iter()
            .filter(|m| {
                m.symbol
                    .split_once('-')
                    .is_some_and(|(b, q)| b == base && quote.as_deref() == Some(q))
            })
            .map(|m| m.symbol.clone())
            .collect::<Vec<_>>();
        if candidates.len() == 1 && quote.is_some() {
            Ok(candidates.into_iter().next())
        } else {
            reasons.push(if candidates.is_empty() {
                format!("{base} absent des marchés admis pour la quote jouée")
            } else {
                format!("{base} ambigu : préciser --{}-symbol", base.to_lowercase())
            });
            Ok(None)
        }
    };
    let btc = select("BTC", &args.btc_symbol)?;
    let eth = select("ETH", &args.eth_symbol)?;
    // Identités réservées non présentes : le noyau rend les références indisponibles,
    // sans cours de substitution ni lecture du store. La sélection publique reste null.
    let symbols = BenchmarkSymbols {
        btc: btc.clone().unwrap_or_else(|| "__unavailable_BTC__".into()),
        eth: eth.clone().unwrap_or_else(|| "__unavailable_ETH__".into()),
    };
    Ok((
        SymbolSelection {
            btc,
            eth,
            quote,
            reasons,
        },
        symbols,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    fn args(extra: &[&str]) -> StatsArgs {
        let mut all = vec!["tv4", "stats", "unused", "--seed", "1"];
        all.extend(extra);
        let crate::cli::Command::Stats(a) = crate::cli::Cli::parse_from(all).command else {
            panic!("stats")
        };
        a
    }
    fn markets(symbols: &[&str]) -> Vec<MarketSeries> {
        symbols
            .iter()
            .map(|s| MarketSeries {
                symbol: (*s).into(),
                daily_closes: vec![],
                valuations: vec![],
            })
            .collect()
    }
    #[test]
    fn explicit_quote_selects_one_currency_and_never_invents_fx() {
        let pairs = vec!["ETH-USD".into(), "ETH-USDC".into()];
        let markets = markets(&["BTC-USD", "ETH-USD", "BTC-USDC", "ETH-USDC"]);
        let (ambiguous, _) = resolve(&args(&[]), &pairs, &markets).unwrap();
        assert!(ambiguous.reasons.iter().any(|r| r.contains("ambigu")));
        assert!(
            resolve(
                &args(&["--btc-symbol", "BTC-USD", "--eth-symbol", "ETH-USDC"]),
                &pairs,
                &markets
            )
            .is_err()
        );
        let (s, _) = resolve(&args(&["--btc-symbol", "BTC-USD"]), &pairs, &markets).unwrap();
        assert_eq!(s.quote.as_deref(), Some("USD"));
        assert_eq!(s.eth.as_deref(), Some("ETH-USD"));
    }
    #[test]
    fn exact_base_is_required_and_absence_stays_unavailable() {
        let pairs = vec!["ETH-USD".into()];
        let markets = markets(&["AIXBT-USD", "ETH-USD"]);
        let (s, _) = resolve(&args(&[]), &pairs, &markets).unwrap();
        assert!(s.btc.is_none());
        assert_eq!(s.eth.as_deref(), Some("ETH-USD"));
        for bad in ["AIXBT-USD", "BTC-USDC", "BTC-USD"] {
            assert!(resolve(&args(&["--btc-symbol", bad]), &pairs, &markets).is_err());
        }
    }
}
