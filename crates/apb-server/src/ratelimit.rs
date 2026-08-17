//! The one piece of rate-limiter math both listeners share.
//!
//! The dashboard's [`crate::auth::RateLimiter`] and the ingest listener's
//! rolling windows keep the same `(window_start_ms, count)` shape and evict
//! under the same cap, so the eviction ordering lives here rather than being
//! written twice and drifting apart.

/// The sort key that decides which rolling-window row is evicted first when a
/// map is over its cap. Lowest key is evicted.
///
/// Three terms, in order:
///
/// 1. **Not-yet-blocked before blocked.** A row past `blocked_over` is only
///    evicted when nothing else is left, so overflowing the map cannot be used
///    to clear an established block. This is what makes eviction safe to do at
///    all, rather than clearing the whole map (which handed an
///    address-rotating attacker their own budget back).
/// 2. **Lowest count first.** The fresh single-hit rows a flood adds are the
///    least established and are shed first.
/// 3. **An age term that flips inside the blocked class.** Below the threshold
///    the oldest row goes first, the ordinary LRU intuition. At or above it the
///    FRESHEST block goes first instead.
///
/// That flip is the subtle part and it is load-bearing. The dashboard limiter
/// never counts past the threshold: both `auth_middleware` and the login
/// handler answer 429 on `is_blocked` and return without recording, so every
/// blocked row is pinned at exactly `blocked_over + 1`. All blocked rows then
/// tie on the first two terms, and an oldest-first tie-break would evict the
/// established block, which is always the oldest row in the map. An attacker
/// could clear their own block by flooding rotated addresses up to the same
/// count. Evicting the freshest block first inverts that: the flood sheds its
/// own newly blocked rows and the established block survives.
pub(crate) fn eviction_key(start_ms: u128, count: u32, blocked_over: u32) -> (bool, u32, u128) {
    let blocked = count > blocked_over;
    let age = if blocked {
        // Reverse the ordering so a LARGER start (a fresher block) sorts lower
        // and is evicted first.
        u128::MAX - start_ms
    } else {
        start_ms
    };
    (blocked, count, age)
}

#[cfg(test)]
mod tests {
    use super::eviction_key;

    const OVER: u32 = 10;

    #[test]
    fn an_unblocked_row_is_evicted_before_a_blocked_one() {
        // Even a high-count unblocked row goes before the lowest blocked row.
        assert!(eviction_key(1_000, OVER, OVER) < eviction_key(1_000, OVER + 1, OVER));
    }

    #[test]
    fn below_the_threshold_the_lowest_count_then_the_oldest_goes_first() {
        assert!(eviction_key(5_000, 1, OVER) < eviction_key(1_000, 2, OVER));
        // Equal count: oldest first, the ordinary LRU intuition.
        assert!(eviction_key(1_000, 3, OVER) < eviction_key(5_000, 3, OVER));
    }

    #[test]
    fn inside_the_blocked_class_the_freshest_block_goes_first() {
        let established = eviction_key(1_000, OVER + 1, OVER);
        let fresh = eviction_key(5_000, OVER + 1, OVER);
        assert!(
            fresh < established,
            "a newly blocked row must be evicted before an established block"
        );
    }
}
