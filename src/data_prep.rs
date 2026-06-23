pub const BENCH_CAPACITY_MIN: i64 = 1;
pub const BENCH_CAPACITY_MAX: i64 = 20;

const BENCH_CAPACITY_SRC_MULTIPLIER: u128 = 31;
const BENCH_CAPACITY_DST_MULTIPLIER: u128 = 17;

pub fn bench_capacity(
    src_id: u64,
    dst_id: u64,
) -> i64 {
    let raw = ((src_id as u128)
        .saturating_mul(BENCH_CAPACITY_SRC_MULTIPLIER)
        .saturating_add((dst_id as u128).saturating_mul(BENCH_CAPACITY_DST_MULTIPLIER)))
        % (BENCH_CAPACITY_MAX as u128);

    BENCH_CAPACITY_MIN + raw as i64
}

#[cfg(test)]
mod tests {
    use super::{bench_capacity, BENCH_CAPACITY_MAX, BENCH_CAPACITY_MIN};

    #[test]
    fn bench_capacity_is_deterministic() {
        assert_eq!(bench_capacity(1, 1), bench_capacity(1, 1));
        assert_eq!(bench_capacity(42, 99), bench_capacity(42, 99));
        assert_eq!(bench_capacity(9998, 7777), bench_capacity(9998, 7777));
    }

    #[test]
    fn bench_capacity_stays_within_expected_range() {
        for src in [1, 2, 17, 100, 9998] {
            for dst in [1, 3, 9, 200, 9998] {
                let value = bench_capacity(src, dst);
                assert!(value >= BENCH_CAPACITY_MIN);
                assert!(value <= BENCH_CAPACITY_MAX);
            }
        }
    }

    #[test]
    fn bench_capacity_matches_formula_examples() {
        assert_eq!(bench_capacity(1, 1), 9);
        assert_eq!(bench_capacity(5, 10), 6);
    }
}
