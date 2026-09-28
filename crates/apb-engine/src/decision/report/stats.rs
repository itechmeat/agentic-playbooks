//! The figures the decisions report prints: accuracy, Brier, expected
//! calibration error, Wilson intervals. Plain functions over (probability,
//! outcome) pairs, so each can be checked by hand.

/// Rounds to four decimals, so the JSON report reads cleanly and a snapshot
/// does not depend on float noise.
pub fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

/// `hits / n`, `None` when `n` is zero.
pub fn rate(hits: usize, n: usize) -> Option<f64> {
    (n > 0).then(|| round4(hits as f64 / n as f64))
}

/// Mean squared error of the probabilities against the outcomes (1 = the
/// event happened). `None` without items.
pub fn brier(items: &[(f64, bool)]) -> Option<f64> {
    if items.is_empty() {
        return None;
    }
    let sum: f64 = items
        .iter()
        .map(|(p, y)| (p - f64::from(u8::from(*y))).powi(2))
        .sum();
    Some(round4(sum / items.len() as f64))
}

/// Expected calibration error over `bins` equal-width bins of the
/// probability (`[0, 0.1)`, ..., `[0.9, 1.0]` for ten): the item-weighted
/// mean of |mean probability - observed rate| per bin.
pub fn ece(items: &[(f64, bool)], bins: usize) -> Option<f64> {
    if items.is_empty() || bins == 0 {
        return None;
    }
    let mut sums = vec![(0.0_f64, 0.0_f64, 0_usize); bins];
    for (p, y) in items {
        let b = ((p * bins as f64).floor() as usize).min(bins - 1);
        sums[b].0 += p;
        sums[b].1 += f64::from(u8::from(*y));
        sums[b].2 += 1;
    }
    let n = items.len() as f64;
    let total: f64 = sums
        .iter()
        .filter(|(_, _, c)| *c > 0)
        .map(|(ps, ys, c)| {
            let c = *c as f64;
            (c / n) * (ps / c - ys / c).abs()
        })
        .sum();
    Some(round4(total))
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
    fn brier_and_ece_match_hand_computed_values() {
        // (0.9, yes) 0.01, (0.8, no) 0.64, (0.3, no) 0.09, (0.05, no) 0.0025
        // -> Brier 0.7425 / 4 = 0.185625.
        let items = [(0.9, true), (0.8, false), (0.3, false), (0.05, false)];
        assert_eq!(brier(&items), Some(0.1856));
        // Bins: [0.9]: |0.9 - 1| = 0.1; [0.8]: |0.8 - 0| = 0.8;
        // [0.3]: 0.3; [0.0]: 0.05. Each weighs 1/4: 1.25 / 4 = 0.3125.
        assert_eq!(ece(&items, 10), Some(0.3125));
        // Two items in one bin average first: |0.85 - 0.5| = 0.35.
        assert_eq!(ece(&[(0.81, true), (0.89, false)], 10), Some(0.35));
        // p = 1.0 falls in the last bin.
        assert_eq!(ece(&[(1.0, true)], 10), Some(0.0));
        assert_eq!(brier(&[]), None);
    }

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
}
