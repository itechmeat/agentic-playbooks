//! Plain statistics every report and decision shares: rates, the median and
//! the Wilson score interval. Pure functions over counts, so each can be
//! checked by hand and the core decisions (promotion by quality) and the
//! engine's reports read the same figures.

/// Rounds to four decimals, so a JSON report reads cleanly and a snapshot
/// does not depend on float noise.
pub fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

/// `hits / n`, `None` when `n` is zero.
pub fn rate(hits: usize, n: usize) -> Option<f64> {
    (n > 0).then(|| round4(hits as f64 / n as f64))
}

/// The Wilson score interval at 95 % for `hits` of `n`, as (low, high).
pub fn wilson95(hits: usize, n: usize) -> Option<(f64, f64)> {
    if n == 0 {
        return None;
    }
    let z = 1.96_f64;
    let n_f = n as f64;
    let p = hits as f64 / n_f;
    let z2 = z * z;
    let denom = 1.0 + z2 / n_f;
    let center = (p + z2 / (2.0 * n_f)) / denom;
    let half = z * (p * (1.0 - p) / n_f + z2 / (4.0 * n_f * n_f)).sqrt() / denom;
    Some((
        round4((center - half).max(0.0)),
        round4((center + half).min(1.0)),
    ))
}

/// The median of `values` (the lower middle for an even count).
pub fn median(values: &mut [u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[(values.len() - 1) / 2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wilson_matches_the_published_interval() {
        // 1 of 20: 0.0089 to 0.2361 (the standard Wilson score interval).
        assert_eq!(wilson95(1, 20), Some((0.0089, 0.2361)));
        assert_eq!(wilson95(0, 10).map(|w| w.0), Some(0.0));
        assert_eq!(wilson95(0, 0), None);
    }

    #[test]
    fn median_takes_the_lower_middle() {
        assert_eq!(median(&mut [30, 10, 20]), Some(20));
        assert_eq!(median(&mut [40, 10, 30, 20]), Some(20));
        assert_eq!(median(&mut []), None);
    }

    #[test]
    fn a_rate_of_nothing_is_absent() {
        assert_eq!(rate(1, 3), Some(0.3333));
        assert_eq!(rate(0, 0), None);
    }
}
