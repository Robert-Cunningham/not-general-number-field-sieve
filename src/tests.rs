use super::*;

#[test]
fn known_small_lucky_numbers() {
    for n in [1_u64, 3, 7, 9, 13, 15, 21, 25, 31, 33, 37, 43, 49, 51, 63, 67, 69, 73, 75, 79] {
        assert!(prove_small(n), "expected {n} to be lucky");
    }

    for n in [5_u64, 11, 17, 19, 23, 27, 29, 35, 39, 41, 45, 47, 53, 55, 57, 59] {
        assert!(!prove_small(n), "expected {n} not to be lucky");
    }
}

#[test]
fn scan_delete_matches_point_deletes() {
    let mut reference = LuckySieve::new(1_000_000).unwrap();
    let mut scanned = LuckySieve::new(1_000_000).unwrap();
    let mut factor_rank = 2_u64;

    for _ in 0..14 {
        let factor = reference.select(factor_rank).unwrap() * 2 - 1;
        let expected_deleted = reference.delete_every(factor);
        let got_deleted = scanned.delete_every_by_u16_scan(factor, 2);

        assert_eq!(got_deleted, expected_deleted, "deletion count mismatch for factor {factor}");

        scanned.alive -= got_deleted;
        scanned.rebuild_tree();

        assert_eq!(scanned.alive, reference.alive, "alive count mismatch after factor {factor}");
        assert_eq!(scanned.bits, reference.bits, "bitset mismatch after factor {factor}");

        for rank in [1, factor_rank, scanned.alive / 2, scanned.alive] {
            if rank > 0 {
                assert_eq!(scanned.select(rank), reference.select(rank), "select mismatch after factor {factor}");
            }
        }

        factor_rank += 1;
    }
}

#[test]
fn recovers_oeis_sequences_to_1m() {
    const LIMIT: u64 = 1_000_000;

    for (name, generate, expected) in [
        (
            "A031882",
            candidates::repdigits as Generator,
            &[1, 3, 7, 9, 33, 99, 111, 777, 9999, 33333, 55555, 111111, 777777][..],
        ),
        ("A057613", candidates::mersennes, &[1, 3, 7, 15, 31, 63, 127, 511, 1023, 4095, 8191, 131071, 524287]),
        ("A057612", candidates::mersenne_prime_exponents, &[3, 7, 31, 127, 8191, 131071, 524287]),
        ("A057589", candidates::fibonacci, &[1, 3, 13, 21, 1597, 6765, 75025]),
        ("A306632", candidates::lucas, &[1, 3, 7, 3571, 9349, 710647]),
        ("A140285", candidates::tetranacci, &[1, 15, 10671]),
        ("A118569", candidates::consecutive_digits, &[21, 43, 67, 87, 321, 4321, 4567, 6789, 78901, 432109]),
    ] {
        assert_eq!(lucky_terms(generate, LIMIT), expected, "{name}");
    }
}

fn prove_small(n: u64) -> bool {
    if n == 1 {
        return true;
    }
    if n % 2 == 0 {
        return false;
    }

    let mut candidate = Candidate::new_unlabeled(n);
    let mut sieve = LuckySieve::new(512).unwrap();
    let mut factor_rank = 2_u64;

    loop {
        let factor = sieve.select(factor_rank).unwrap() * 2 - 1;
        apply_factor_to_candidates(factor, std::slice::from_mut(&mut candidate), false);
        match candidate.state {
            CandidateState::Lucky { .. } => return true,
            CandidateState::Composite { .. } => return false,
            CandidateState::Pending { .. } => {}
            CandidateState::Inconclusive { .. } => unreachable!(),
        }
        if factor <= sieve.alive {
            sieve.delete_every(factor);
        }
        factor_rank += 1;
    }
}

fn lucky_terms(generate: Generator, limit: u64) -> Vec<u64> {
    let mut candidates = generate_candidates(0, limit, &[generate]);
    let mut sieve = LuckySieve::new(limit / 2).unwrap();
    search_with_sieve(&mut sieve, &mut candidates, threads(), SCAN_THRESHOLD, false);

    candidates
        .into_iter()
        .filter(|candidate| matches!(candidate.state, CandidateState::Lucky { .. }))
        .map(|candidate| candidate.n)
        .collect()
}
