//! The figures the decisions report prints: accuracy, Brier, expected
//! calibration error, Wilson intervals. Plain functions over (probability,
//! outcome) pairs, so each can be checked by hand. The shared rates, the
//! median and the Wilson interval live in [`apb_core::stats`].

pub use apb_core::stats::{median, rate, round4, wilson95};

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
        // Rounded before the floor: 1 - 0.9 is 0.0999..., which belongs
        // to [0.1, 0.2) like the 0.1 it stands for.
        let b = (((p * bins as f64) * 1e9).round() / 1e9).floor().max(0.0) as usize;
        let b = b.min(bins - 1);
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

/// The `q` quantile (0 to 1) of already sorted values, linearly
/// interpolated between the two nearest ranks; `None` for no values.
pub fn quantile(sorted: &[f64], q: f64) -> Option<f64> {
    let last = sorted.len().checked_sub(1)?;
    let pos = q.clamp(0.0, 1.0) * last as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    let (a, b) = (sorted[lo], sorted[hi]);
    Some(a + (b - a) * (pos - lo as f64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probability_on_a_bin_edge_lands_in_the_bin_it_opens() {
        // 1 - 0.9 is 0.0999...: bin [0.1, 0.2), whose observed rate is 1
        // here, so the error is |0.1 - 1| = 0.9 either way; with a second
        // item at 0.15 (rate 0) sharing the bin the mean differs by bin.
        let items = [(1.0 - 0.9, true), (0.15, false)];
        assert_eq!(ece(&items, 10), Some(0.375));
    }

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
}
