//! Comptage et désindexation exacts du support individuel, sans aléa ni score.
//!
//! Les buckets portent l'année réelle du fill. Seule l'année active entre dans la clé : les
//! quotas passés sont terminés et les quotas futurs restent fixes. La borne de décision reste
//! globale lors d'un changement d'année, notamment pour un signal de décembre rempli en janvier.

use std::collections::BTreeMap;

use num_bigint::BigUint;

use super::types::{
    AnnualQuota, Budgets, Calendar, PairCatalogue, PlanningFailure, PlanningWork, validate_quotas,
};

/// Budget logique par état, avant les limbs du compte : clé 32 octets et longueur 8 octets.
const STATE_BYTES: u64 = 32 + 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct State {
    year: usize,
    min_decision: usize,
    retained_left: u32,
    purged_left: u32,
}

#[derive(Clone, Copy, Debug)]
struct Candidate {
    id: u32,
    decision: usize,
    next_decision: usize,
    retained: bool,
}

#[derive(Debug)]
struct YearBucket {
    year: i32,
    retained: u32,
    purged: u32,
    candidates: Vec<Candidate>,
}

#[derive(Debug, Default)]
struct Count {
    value: BigUint,
    complete: bool,
}

#[derive(Clone, Copy)]
struct Branches {
    id: u32,
    skip: State,
    take: Option<State>,
}

/// Table immuable possédant les seuls intervalles admissibles à ses quotas.
///
/// `build_with_work` partage le budget d'une géométrie entre toutes ses paires et modes effectifs.
/// La construction n'emploie jamais de récursion, même pour des centaines de milliers de slots.
#[derive(Debug)]
pub struct CalendarTable {
    buckets: Vec<YearBucket>,
    counts: BTreeMap<State, Count>,
    root: State,
    total: BigUint,
}

impl CalendarTable {
    /// Compteur neuf, adapté à une table autonome ; le moteur emploie `build_with_work`.
    pub fn build(
        catalogue: &PairCatalogue,
        quotas: &[AnnualQuota],
        budgets: &Budgets,
    ) -> Result<Self, PlanningFailure> {
        Self::build_with_work(catalogue, quotas, budgets, &mut PlanningWork::default())
    }

    pub fn build_with_work(
        catalogue: &PairCatalogue,
        quotas: &[AnnualQuota],
        budgets: &Budgets,
        work: &mut PlanningWork,
    ) -> Result<Self, PlanningFailure> {
        catalogue.validate()?;
        validate_quotas(quotas)?;
        let mut buckets: Vec<_> = quotas
            .iter()
            .filter(|quota| quota.filled > 0)
            .map(|quota| YearBucket {
                year: quota.year,
                retained: quota.retained,
                purged: quota.filled - quota.retained,
                candidates: Vec::new(),
            })
            .collect();
        for interval in &catalogue.intervals {
            if let Ok(index) = buckets.binary_search_by_key(&interval.fill_year, |b| b.year) {
                buckets[index].candidates.push(Candidate {
                    id: interval.id,
                    decision: interval.decision,
                    next_decision: interval.next_decision,
                    retained: interval.retained,
                });
            }
        }
        let mut table = Self {
            buckets,
            counts: BTreeMap::new(),
            root: State {
                year: 0,
                min_decision: 0,
                retained_left: 0,
                purged_left: 0,
            },
            total: BigUint::default(),
        };
        table.root = table.start_year(0, 0);
        table.reserve(table.root, budgets, work)?;
        let mut stack = vec![table.root];
        while let Some(state) = stack.last().copied() {
            let branches = table.branches(state);
            if let Some(branches) = branches {
                // Un enfant est réservé dès sa découverte, avant son empilement. Le frère take
                // attend le calcul de skip : un petit budget ne crée pas une grande pile future.
                let pending = [Some(branches.skip), branches.take]
                    .into_iter()
                    .flatten()
                    .find(|child| !table.counts.get(child).is_some_and(|count| count.complete));
                if let Some(child) = pending {
                    if !table.counts.contains_key(&child) {
                        table.reserve(child, budgets, work)?;
                    }
                    stack.push(child);
                    continue;
                }
            }
            let value = if let Some(branches) = branches {
                let skip = table.count(branches.skip)?;
                let take = branches.take.map(|child| table.count(child)).transpose()?;
                // La retenue des chiffres u64 donne la taille exacte sans allouer la somme.
                // Le débit précède ainsi aussi l'allocation des limbs du nouveau compte.
                let limb_bytes = sum_limb_bytes(skip, take).ok_or(PlanningFailure::Budget {
                    cells: work.cells,
                    logical_bytes: work.logical_bytes,
                })?;
                work.charge(0, limb_bytes, budgets)?;
                match take {
                    Some(take) => skip + take,
                    None => skip.clone(),
                }
            } else {
                let terminal = state.year == table.buckets.len();
                work.charge(0, if terminal { 8 } else { 0 }, budgets)?;
                BigUint::from(u32::from(terminal))
            };
            let cell = table
                .counts
                .get_mut(&state)
                .ok_or_else(inconsistent_table)?;
            cell.value = value;
            cell.complete = true;
            stack.pop();
        }
        table.total = table.count(table.root)?.clone();
        if table.total == BigUint::default() {
            return Err(PlanningFailure::EmptySupport);
        }
        Ok(table)
    }

    pub fn total(&self) -> &BigUint {
        &self.total
    }

    /// Désindexe un rang dans `[0,total)` ; skip précède take dans l'ordre déterministe.
    pub fn unrank(&self, mut rank: BigUint) -> Result<Calendar, PlanningFailure> {
        if rank >= self.total {
            return Err(PlanningFailure::InvalidInput(
                "le rang doit être strictement inférieur au support".into(),
            ));
        }
        let mut state = self.root;
        let mut calendar = Calendar::default();
        while let Some(branches) = self.branches(state) {
            let skip = self.count(branches.skip)?;
            if &rank < skip {
                state = branches.skip;
            } else {
                rank -= skip;
                state = branches.take.ok_or_else(inconsistent_table)?;
                calendar.interval_ids.push(branches.id);
            }
        }
        if state.year != self.buckets.len() {
            return Err(inconsistent_table());
        }
        Ok(calendar)
    }

    fn reserve(
        &mut self,
        state: State,
        budgets: &Budgets,
        work: &mut PlanningWork,
    ) -> Result<(), PlanningFailure> {
        work.charge(1, STATE_BYTES, budgets)?;
        self.counts.insert(state, Count::default());
        Ok(())
    }

    fn count(&self, state: State) -> Result<&BigUint, PlanningFailure> {
        self.counts
            .get(&state)
            .filter(|count| count.complete)
            .map(|count| &count.value)
            .ok_or_else(inconsistent_table)
    }

    fn start_year(&self, year: usize, min_decision: usize) -> State {
        if let Some(bucket) = self.buckets.get(year) {
            State {
                year,
                min_decision,
                retained_left: bucket.retained,
                purged_left: bucket.purged,
            }
        } else {
            // Un seul état terminal partagé par toutes les bornes de décision.
            State {
                year: self.buckets.len(),
                min_decision: 0,
                retained_left: 0,
                purged_left: 0,
            }
        }
    }

    fn normalize(&self, state: State) -> State {
        if state.retained_left == 0 && state.purged_left == 0 {
            self.start_year(state.year + 1, state.min_decision)
        } else {
            state
        }
    }

    fn branches(&self, state: State) -> Option<Branches> {
        let bucket = self.buckets.get(state.year)?;
        let index = bucket
            .candidates
            .partition_point(|c| c.decision < state.min_decision);
        let candidate = bucket.candidates.get(index)?;
        let mut take = State {
            min_decision: candidate.next_decision,
            ..state
        };
        let remaining = if candidate.retained {
            &mut take.retained_left
        } else {
            &mut take.purged_left
        };
        let take = if let Some(left) = remaining.checked_sub(1) {
            *remaining = left;
            Some(self.normalize(take))
        } else {
            None
        };
        Some(Branches {
            id: candidate.id,
            // validate() garantit decision < next_decision : cette addition ne déborde pas.
            skip: State {
                min_decision: candidate.decision + 1,
                ..state
            },
            take,
        })
    }
}

fn inconsistent_table() -> PlanningFailure {
    PlanningFailure::InvalidInput("la table de calendriers est incohérente".into())
}

/// Nombre exact de limbs de la somme, zéro compris, sans construire un BigUint temporaire.
fn sum_limb_bytes(left: &BigUint, right: Option<&BigUint>) -> Option<u64> {
    let Some(right) = right else {
        return left.bits().div_ceil(64).checked_mul(8);
    };
    let mut left = left.iter_u64_digits();
    let mut right = right.iter_u64_digits();
    let length = left.len().max(right.len());
    let mut carry = false;
    for _ in 0..length {
        let (sum, first_carry) = left
            .next()
            .unwrap_or(0)
            .overflowing_add(right.next().unwrap_or(0));
        let (_, second_carry) = sum.overflowing_add(u64::from(carry));
        carry = first_carry || second_carry;
    }
    u64::try_from(length)
        .ok()?
        .checked_add(u64::from(carry))?
        .checked_mul(8)
}

#[cfg(test)]
mod tests {
    use super::sum_limb_bytes;
    use num_bigint::BigUint;

    #[test]
    fn logical_limbs_cover_zero_word_carries_and_powers_of_two() {
        let zero = BigUint::default();
        let one = BigUint::from(1_u32);
        let p64 = &one << 64_u32;
        let p128 = &one << 128_u32;
        let p192 = &one << 192_u32;
        let cases = [
            (zero.clone(), zero.clone(), 0),
            (zero.clone(), one.clone(), 8),
            (&p64 - &one, one.clone(), 16),
            (&one << 63_u32, &one << 63_u32, 16),
            (p64.clone(), &p64 - &one, 16),
            (p128.clone(), p128.clone(), 24),
            (&p128 - &one, &p128 - &one, 24),
            (&p192 - &one, one.clone(), 32),
            (&p192 - &one, &p128 + &one, 32),
        ];
        for (left, right, expected) in cases {
            assert_eq!(sum_limb_bytes(&left, Some(&right)), Some(expected));
            // L'addition de la crate sert d'oracle au parcours des retenues, sans le reproduire.
            assert_eq!((&left + &right).bits().div_ceil(64) * 8, expected);
        }
        assert_eq!(sum_limb_bytes(&zero, None), Some(0));
        assert_eq!(sum_limb_bytes(&p64, None), Some(16));
    }
}
