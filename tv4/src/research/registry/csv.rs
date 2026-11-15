//! Encodage CSV déterministe et lecture stricte, sans réparation des journaux incomplets.
use crate::model::blocks::Block;

use super::{
    ClaimId, Digest, Event, EventId, HypothesisRef, Metrics, ReadId, RegistryError, Status,
    TrialId, UtcDate, invalid,
};

const HEADER: [&str; 18] = [
    "trial_id",
    "famille",
    "fiche_sha256",
    "config_hash",
    "dataset_hash",
    "bloc",
    "date",
    "mu_r",
    "n",
    "pf",
    "statut",
    "claim_holdout",
    "event_id",
    "read_id",
    "hypothesis",
    "generation",
    "decision_start_ms",
    "decision_end_ms",
];

/// En-tête S12 avec sa colonne `generation` explicite, terminé par LF.
pub fn header_csv() -> Vec<u8> {
    format!("{}\n", HEADER.join(",")).into_bytes()
}

fn number<T: std::str::FromStr>(text: &str, field: &str) -> Result<T, RegistryError> {
    if text.is_empty() || text.trim() != text {
        return Err(invalid(format!("champ {field} invalide")));
    }
    text.parse()
        .map_err(|_| invalid(format!("champ {field} invalide : {text}")))
}
fn optional<T>(
    text: &str,
    parse: impl FnOnce(&str) -> Result<T, RegistryError>,
) -> Result<Option<T>, RegistryError> {
    if text.is_empty() {
        Ok(None)
    } else {
        parse(text).map(Some)
    }
}

/// Valide aussi les guillemets : le lecteur CSV usuel tolère des guillemets non fermés à EOF.
fn validate_syntax(bytes: &[u8]) -> Result<(), RegistryError> {
    #[derive(Clone, Copy)]
    enum State {
        Start,
        Bare,
        Quoted,
        Closed,
    }
    let mut state = State::Start;
    let mut row_has_data = false;
    let mut index = 0;
    while index < bytes.len() {
        let b = bytes[index];
        if matches!(state, State::Quoted) {
            if b == b'"' {
                state = State::Closed;
            }
            index += 1;
            continue;
        }
        let newline = b == b'\n' || (b == b'\r' && bytes.get(index + 1) == Some(&b'\n'));
        if newline {
            if !row_has_data {
                return Err(invalid("ligne CSV vide"));
            }
            state = State::Start;
            row_has_data = false;
            index += if b == b'\r' { 2 } else { 1 };
            continue;
        }
        row_has_data = true;
        state = match (state, b) {
            (State::Start, b'"') => State::Quoted,
            (State::Start | State::Bare | State::Closed, b',') => State::Start,
            (State::Closed, b'"') => State::Quoted,
            (State::Start | State::Bare, b'"' | b'\r') | (State::Closed, _) => {
                return Err(invalid("guillemets ou séparateurs CSV invalides"));
            }
            _ => State::Bare,
        };
        index += 1;
    }
    if matches!(state, State::Quoted) {
        return Err(invalid("champ CSV entre guillemets non fermé"));
    }
    Ok(())
}

/// Lit les octets sans les modifier. Un journal réellement vide est valide ; tout autre journal
/// doit posséder l'en-tête exact et un dernier LF, faute de quoi la dernière écriture est partielle.
pub fn decode_csv(bytes: &[u8]) -> Result<Vec<Event>, RegistryError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if bytes.last() != Some(&b'\n') {
        return Err(invalid("journal tronqué : dernier saut de ligne absent"));
    }
    validate_syntax(bytes)?;
    let mut reader = ::csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(false)
        .from_reader(bytes);
    let headers = reader
        .headers()
        .map_err(|e| invalid(format!("en-tête CSV invalide : {e}")))?;
    if headers.iter().ne(HEADER) {
        return Err(invalid("schéma CSV du registre incompatible"));
    }
    let mut events = Vec::new();
    for record in reader.records() {
        let row = record.map_err(|e| invalid(format!("ligne CSV invalide : {e}")))?;
        let status = Status::parse(&row[10])?;
        let hypothesis = match (&row[14], &row[2]) {
            ("unregistered_hypothesis", "") if status == Status::Registered => None,
            ("", "") if status != Status::Registered => None,
            (id, sha) if !id.is_empty() && !sha.is_empty() => {
                Some(HypothesisRef::new(id.to_owned(), Digest::new(sha)?)?)
            }
            _ => return Err(invalid("identité de fiche absente ou incomplète")),
        };
        let block = optional(&row[5], |value| {
            Block::ALL
                .into_iter()
                .find(|b| b.as_str() == value)
                .ok_or_else(|| invalid("bloc inconnu ou non canonique"))
        })?;
        let event = Event {
            trial_id: TrialId(number(&row[0], "trial_id")?),
            family: row[1].to_owned(),
            hypothesis,
            config_hash: Digest::new(&row[3])?,
            dataset_hash: optional(&row[4], |v| Digest::new(v))?,
            block,
            date: UtcDate::new(&row[6])?,
            metrics: Metrics {
                mu_r: optional(&row[7], |v| number(v, "mu_r"))?,
                n: optional(&row[8], |v| number(v, "n"))?,
                pf: optional(&row[9], |v| number(v, "pf"))?,
            },
            status,
            claim_holdout: optional(&row[11], |v| number(v, "claim_holdout").map(ClaimId))?,
            event_id: EventId(number(&row[12], "event_id")?),
            read_id: optional(&row[13], |v| number(v, "read_id").map(ReadId))?,
            generation: number(&row[15], "generation")?,
            decision_start_ms: optional(&row[16], |v| number(v, "decision_start_ms"))?,
            decision_end_ms: optional(&row[17], |v| number(v, "decision_end_ms"))?,
        };
        event.metrics.validate()?;
        super::validate_window(event.decision_start_ms, event.decision_end_ms)?;
        events.push(event);
    }
    Ok(events)
}

/// Encode uniquement des lignes d'événements, sans en-tête, afin de préserver le préfixe existant.
/// La cohérence historique est validée par `Registry::with_events` avant persistance.
pub fn encode_events(events: &[Event]) -> Result<Vec<u8>, RegistryError> {
    let mut writer = ::csv::WriterBuilder::new()
        .has_headers(false)
        .terminator(::csv::Terminator::Any(b'\n'))
        .from_writer(Vec::new());
    for event in events {
        event.metrics.validate()?;
        super::validate_window(event.decision_start_ms, event.decision_end_ms)?;
        let hypothesis = event.hypothesis.as_ref().map_or_else(
            || {
                if event.status == Status::Registered {
                    "unregistered_hypothesis".to_owned()
                } else {
                    String::new()
                }
            },
            |h| h.id().to_owned(),
        );
        let fields = [
            event.trial_id.0.to_string(),
            event.family.clone(),
            event
                .hypothesis
                .as_ref()
                .map_or_else(String::new, |h| h.sha256().to_string()),
            event.config_hash.to_string(),
            event
                .dataset_hash
                .as_ref()
                .map_or_else(String::new, ToString::to_string),
            event
                .block
                .map_or_else(String::new, |b| b.as_str().to_owned()),
            event.date.as_str().to_owned(),
            event
                .metrics
                .mu_r
                .map_or_else(String::new, |v| v.to_string()),
            event.metrics.n.map_or_else(String::new, |v| v.to_string()),
            event.metrics.pf.map_or_else(String::new, |v| v.to_string()),
            event.status.as_str().to_owned(),
            event
                .claim_holdout
                .map_or_else(String::new, |id| id.0.to_string()),
            event.event_id.0.to_string(),
            event
                .read_id
                .map_or_else(String::new, |id| id.0.to_string()),
            hypothesis,
            event.generation.to_string(),
            event
                .decision_start_ms
                .map_or_else(String::new, |v| v.to_string()),
            event
                .decision_end_ms
                .map_or_else(String::new, |v| v.to_string()),
        ];
        writer
            .write_record(fields)
            .map_err(|e| invalid(format!("encodage CSV impossible : {e}")))?;
    }
    writer
        .into_inner()
        .map_err(|e| invalid(format!("encodage CSV impossible : {e}")))
}
