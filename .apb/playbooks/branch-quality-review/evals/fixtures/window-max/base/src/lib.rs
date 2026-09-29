//! Small helpers over integer slices.

/// The sum of the first `n` elements of `xs`, or of all of them when `n`
/// exceeds the length.
pub fn sum_first(xs: &[i64], n: usize) -> i64 {
    xs.iter().take(n).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_a_prefix() {
        assert_eq!(sum_first(&[1, 2, 3], 2), 3);
        assert_eq!(sum_first(&[1, 2, 3], 9), 6);
    }
}
