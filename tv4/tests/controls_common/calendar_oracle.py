#!/usr/bin/env python3
"""Oracle S14 indépendant : sous-ensembles exhaustifs, jamais une récurrence DP.

python3 tv4/tests/controls_common/calendar_oracle.py [--rust]
La sortie Rust est figée dans controls_calendar.rs ; Cargo n'exécute pas Python.
"""
import itertools
import json
import sys
from datetime import datetime, timezone

MINUTE = 60_000
JAN_2020 = int(datetime(2020, 1, 1, tzinfo=timezone.utc).timestamp() * 1000)
JAN_2021 = int(datetime(2021, 1, 1, tzinfo=timezone.utc).timestamp() * 1000)


def regular(durations, purged=()):
    return [dict(id=i, decision=i, signal_ms=JAN_2020 + i * MINUTE,
                 entry_ms=JAN_2020 + i * MINUTE,
                 release_ms=JAN_2020 + (i + h) * MINUTE,
                 next_decision=i + h, fill_year=2020, retained=i not in purged)
            for i, h in enumerate(durations)]


def enumerate_calendars(intervals, quotas):
    assert len(intervals) <= 18
    expected = {year: (filled, kept) for year, filled, kept in quotas}
    calendars = []
    for mask in itertools.product((False, True), repeat=len(intervals)):
        chosen = sorted((x for x, take in zip(intervals, mask) if take),
                        key=lambda x: x['decision'])
        if any(b['decision'] < a['next_decision'] or
               b['signal_ms'] < a['release_ms'] or
               b['fill_year'] < a['fill_year'] or not a['retained']
               for a, b in zip(chosen, chosen[1:])):
            continue
        actual = {year: [0, 0] for year in expected}
        for x in chosen:
            if x['fill_year'] not in actual:
                break
            actual[x['fill_year']][0] += 1
            actual[x['fill_year']][1] += int(x['retained'])
        else:
            if all(tuple(actual[year]) == counts for year, counts in expected.items()):
                calendars.append([x['id'] for x in chosen])
    return sorted(calendars)


def cases():
    output = []
    def add(name, intervals, quotas):
        calendars = enumerate_calendars(intervals, quotas)
        output.append(dict(name=name, intervals=intervals, quotas=quotas,
                           total=len(calendars), calendars=calendars))
    add('fixed_8_2_3', regular([2] * 8), [(2020, 3, 3)])
    for q in range(5):
        add(f'variable_10_q{q}', regular([2, 3, 2, 4, 2, 3, 2, 2, 2, 2]),
            [(2020, q, q)])
    add('variable_2_to_8', regular([2, 3, 4, 5, 6, 7, 8, 2, 3, 4, 5, 6]),
        [(2020, 3, 3)])
    add('terminal_purge', regular([2] * 8, (6, 7)), [(2020, 2, 1)])
    # Décisions 0 et 1 en décembre ; les fills de A/C/D sont réellement en janvier.
    signals = [JAN_2021 - 2 * MINUTE, JAN_2021 - MINUTE,
               JAN_2021, JAN_2021 + 2 * MINUTE]
    nexts = [4, 2, 3, 5]
    releases = [JAN_2021 + 2 * MINUTE, JAN_2021,
                JAN_2021 + MINUTE, JAN_2021 + 3 * MINUTE]
    multi = [dict(id=i, decision=d, signal_ms=signals[i],
                  entry_ms=JAN_2021 if i == 0 else signals[i],
                  release_ms=releases[i], next_decision=nexts[i],
                  fill_year=2020 if i == 1 else 2021, retained=True)
             for i, d in enumerate([0, 1, 2, 4])]
    add('non_monotone_fills', multi, [(2020, 1, 1), (2021, 1, 1)])
    add('zero_before_january_fill', multi, [(2020, 0, 0), (2021, 1, 1)])
    add('missing_year', regular([2] * 4), [(2021, 1, 1)])
    add('all_zero', multi, [(2020, 0, 0), (2021, 0, 0)])
    add('absent_quotas', multi, [])
    add('zero_after_terminal_purge', regular([2] * 8, (6, 7)),
        [(2020, 2, 1), (2021, 0, 0)])
    # Deux années réellement distinctes ; une occupation traverse la frontière.
    yearly = regular([2] * 6)
    for i, x in enumerate(yearly):
        x.update(signal_ms=JAN_2021 + (i - 3) * MINUTE,
                 entry_ms=JAN_2021 + (i - 3) * MINUTE,
                 release_ms=JAN_2021 + (i - 1) * MINUTE,
                 fill_year=2020 if i < 3 else 2021)
    add('two_years', yearly, [(2020, 1, 1), (2021, 2, 2)])
    return output


if __name__ == '__main__':
    payload = json.dumps(cases(), sort_keys=True, separators=(',', ':'))
    if sys.argv[1:] == ['--rust']:
        print('const ORACLE_JSON: &str = r#"' + payload + '"#;')
    else:
        print(payload)
