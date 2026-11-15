use crate::research::stats::types::Window;
use crate::store::time::{MS_PER_DAY, civil_from_days, days_from_civil, days_in_month};
pub(super) fn months(w: Window) -> Vec<Window> {
    let (mut y, mut m, _) = civil_from_days(w.start_ms / MS_PER_DAY);
    let mut result = Vec::new();
    loop {
        let start_ms = days_from_civil(y, m, 1) * MS_PER_DAY;
        let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
        let end_ms = days_from_civil(ny, nm, 1) * MS_PER_DAY;
        if start_ms >= w.end_ms {
            break;
        }
        result.push(Window { start_ms, end_ms });
        y = ny;
        m = nm;
    }
    result
}
pub(super) fn at_least_36_months(w: Window) -> bool {
    let day = w.start_ms / MS_PER_DAY;
    let (y, m, d) = civil_from_days(day);
    let anniversary = days_from_civil(y + 3, m, d.min(days_in_month(y + 3, m))) * MS_PER_DAY
        + w.start_ms % MS_PER_DAY;
    w.end_ms >= anniversary
}
