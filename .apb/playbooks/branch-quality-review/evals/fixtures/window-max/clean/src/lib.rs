//! Small helpers over integer slices.

/// The sum of the first `n` elements of `xs`, or of all of them when `n`
/// exceeds the length.
pub fn sum_first(xs: &[i64], n: usize) -> i64 {
    xs.iter().take(n).sum()
}

/// The largest sum of `k` consecutive elements of `xs`, `None` when `k` is
/// zero or larger than the slice.
pub fn window_max(xs: &[i64], k: usize) -> Option<i64> {
    if k == 0 || k > xs.len() {
        return None;
    }
    xs.windows(k).map(|w| w.iter().sum()).max()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_a_prefix() {
        assert_eq!(sum_first(&[1, 2, 3], 2), 3);
        assert_eq!(sum_first(&[1, 2, 3], 9), 6);
    }

    #[test]
    fn finds_the_best_window() {
        assert_eq!(window_max(&[5, 1, 1, 1], 2), Some(6));
        assert_eq!(window_max(&[1, 1, 1, 5], 2), Some(6));
        assert_eq!(window_max(&[4], 1), Some(4));
        assert_eq!(window_max(&[1, 2], 0), None);
        assert_eq!(window_max(&[1, 2], 3), None);
    }
}
