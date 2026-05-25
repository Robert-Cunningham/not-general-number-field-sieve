mod candidates;
#[cfg(test)]
mod tests;

use candidates::{generate_candidates, Candidate, CandidateState, Generator};
use clap::Parser;
use std::collections::BTreeMap;
use std::fmt;
use std::thread;

const MIN: u64 = 0;
const MAX: u64 = u64::MAX;
const MEMORY_MIB: u64 = 512;
const SIEVE_LIMIT: Option<u64> = None;
const SCAN_THRESHOLD: u64 = 511;
const U16_SCAN_MAX_STEP: u64 = 2_048;
const SCAN_MIN_WORDS: u64 = 1_000_000;
const SCAN_LARGE_WORDS: u64 = 100_000_000;
const SCAN_SMALL_DELETION_WORD_DIVISOR: u64 = 4;
const SCAN_LARGE_DELETION_WORD_DIVISOR: u64 = 16;
const CANDIDATE_GENERATORS: &[Generator] = &[
    candidates::repdigits,
    candidates::mersennes,
    candidates::mersenne_prime_exponents,
    candidates::fibonacci,
    candidates::lucas,
    candidates::tetranacci,
    candidates::consecutive_digits,
];

#[derive(Debug, Parser)]
struct Cli {
    #[arg(long, default_value_t = MIN)]
    min: u64,

    #[arg(long, default_value_t = MAX)]
    max: u64,

    #[arg(long, default_value_t = MEMORY_MIB)]
    memory_mib: u64,

    #[arg(long)]
    sieve_limit: Option<u64>,

    #[arg(long, default_value_t = SCAN_THRESHOLD)]
    scan_threshold: u64,

    #[arg(long)]
    threads: Option<usize>,
}

#[derive(Debug)]
enum AppError {
    Allocation(String),
    EmptySieve,
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::Allocation(msg) => write!(f, "{msg}"),
            AppError::EmptySieve => write!(f, "sieve memory budget is too small to hold any words"),
        }
    }
}

struct LuckySieve {
    bits: Vec<u64>,
    tree: Vec<u64>,
    n_odds: u64,
    alive: u64,
}

impl LuckySieve {
    fn new(n_odds: u64) -> Result<Self, AppError> {
        if n_odds == 0 {
            return Err(AppError::EmptySieve);
        }

        let words_u64 = n_odds.div_ceil(64);
        let words = usize::try_from(words_u64).map_err(|_| {
            AppError::Allocation(format!("sieve needs {words_u64} words, which does not fit usize on this target"))
        })?;

        let mut bits = Vec::new();
        bits.try_reserve_exact(words)
            .map_err(|err| AppError::Allocation(format!("failed to reserve bitset for {words} words: {err}")))?;
        bits.resize(words, !0_u64);

        let used_bits_in_last = (n_odds - 1) % 64 + 1;
        if used_bits_in_last < 64 {
            let mask = (1_u64 << used_bits_in_last) - 1;
            let last = bits.len() - 1;
            bits[last] = mask;
        }

        let mut tree = Vec::new();
        tree.try_reserve_exact(words + 1).map_err(|err| {
            AppError::Allocation(format!("failed to reserve Fenwick tree for {} words: {err}", words + 1))
        })?;
        tree.resize(words + 1, 0);

        for (i, word) in bits.iter().enumerate() {
            tree[i + 1] = word.count_ones() as u64;
        }
        for i in 1..=words {
            let parent = i + lowbit(i);
            if parent <= words {
                tree[parent] += tree[i];
            }
        }

        Ok(Self { bits, tree, n_odds, alive: n_odds })
    }

    fn value_limit(&self) -> u64 {
        self.n_odds.saturating_mul(2).saturating_sub(1)
    }

    fn select(&self, rank: u64) -> Option<u64> {
        if rank == 0 || rank > self.alive {
            return None;
        }

        let words = self.bits.len();
        let mut idx = 0_usize;
        let mut bit = highest_power_of_two_at_most(words);
        let mut remaining = rank;

        while bit != 0 {
            let next = idx + bit;
            if next <= words && self.tree[next] < remaining {
                idx = next;
                remaining -= self.tree[next];
            }
            bit >>= 1;
        }

        let word_idx = idx;
        let bit_idx = select_in_word(self.bits[word_idx], remaining as u32);
        Some((word_idx as u64) * 64 + (bit_idx as u64) + 1)
    }

    fn prefix_word_count(&self, words: usize) -> u64 {
        debug_assert!(words <= self.bits.len());

        let mut i = words;
        let mut sum = 0_u64;
        while i > 0 {
            sum += self.tree[i];
            i -= lowbit(i);
        }
        sum
    }

    fn delete_every(&mut self, step: u64) -> u64 {
        let deletions = self.alive / step;
        for multiple in (1..=deletions).rev() {
            let rank = multiple * step;
            let odd_index = self.select(rank).expect("rank chosen from alive count must be selectable");
            self.clear(odd_index);
        }
        deletions
    }

    fn should_scan_delete(&self, step: u64, deletions: u64, scan_threshold: u64) -> bool {
        if step > scan_threshold || step > U16_SCAN_MAX_STEP {
            return false;
        }

        let words = self.bits.len() as u64;
        if words < SCAN_MIN_WORDS {
            return false;
        }

        let min_deletions = (words / self.scan_deletion_word_divisor()).max(1);
        deletions >= min_deletions
    }

    fn scan_deletion_word_divisor(&self) -> u64 {
        if self.bits.len() as u64 >= SCAN_LARGE_WORDS {
            SCAN_LARGE_DELETION_WORD_DIVISOR
        } else {
            SCAN_SMALL_DELETION_WORD_DIVISOR
        }
    }

    fn delete_every_by_scan(&mut self, step: u64, expected_deletions: u64, threads: usize) -> u64 {
        let threads = threads.max(1).min(self.bits.len().max(1));
        let old_alive = self.alive;
        let deleted = self.delete_every_by_u16_scan(step, threads);
        assert_eq!(deleted, expected_deletions, "scan deletion count mismatch for factor {step}");
        self.alive = old_alive - deleted;
        self.rebuild_tree();
        deleted
    }

    fn delete_every_by_u16_scan(&mut self, step: u64, threads: usize) -> u64 {
        let actions = build_u16_actions(step as u16);
        let ranges = self.scan_ranges(threads, step);
        let total_words = self.bits.len();
        let chunk_words = total_words.div_ceil(ranges.len().max(1));
        let mut deleted = 0_u64;

        thread::scope(|scope| {
            let mut handles = Vec::with_capacity(ranges.len());
            for (range, chunk) in ranges.into_iter().zip(self.bits.chunks_mut(chunk_words)) {
                let actions = &actions;
                handles.push(
                    scope.spawn(move || scan_delete_range_u16(chunk, range.start_rank_mod, step as u16, actions)),
                );
            }

            for handle in handles {
                deleted += handle.join().expect("scan worker panicked");
            }
        });

        deleted
    }

    fn scan_ranges(&self, threads: usize, step: u64) -> Vec<ScanRange> {
        let total_words = self.bits.len();
        let threads = threads.max(1).min(total_words.max(1));
        let chunk_words = total_words.div_ceil(threads);
        let mut ranges = Vec::with_capacity(threads);

        for start_word in (0..total_words).step_by(chunk_words) {
            ranges.push(ScanRange { start_rank_mod: self.prefix_word_count(start_word) % step });
        }

        ranges
    }

    fn rebuild_tree(&mut self) {
        let words = self.bits.len();
        self.tree[0] = 0;

        for (i, word) in self.bits.iter().enumerate() {
            self.tree[i + 1] = word.count_ones() as u64;
        }

        for i in 1..=words {
            let parent = i + lowbit(i);
            if parent <= words {
                self.tree[parent] += self.tree[i];
            }
        }
    }

    fn clear(&mut self, odd_index: u64) {
        debug_assert!((1..=self.n_odds).contains(&odd_index));
        let zero_based = odd_index - 1;
        let word_idx = (zero_based / 64) as usize;
        let mask = 1_u64 << (zero_based % 64);

        if self.bits[word_idx] & mask == 0 {
            return;
        }

        self.bits[word_idx] &= !mask;
        self.alive -= 1;

        let mut i = word_idx + 1;
        while i < self.tree.len() {
            self.tree[i] -= 1;
            i += lowbit(i);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ScanRange {
    start_rank_mod: u64,
}

fn build_u16_actions(step: u16) -> Vec<u32> {
    debug_assert!((step as u64) <= U16_SCAN_MAX_STEP);
    let step_usize = step as usize;
    let mut actions = Vec::with_capacity(step_usize * 65_536);

    for phase in 0..step {
        for chunk in 0..=u16::MAX {
            actions.push(pack_u16_action(step, phase, chunk));
        }
    }

    actions
}

fn pack_u16_action(step: u16, phase: u16, chunk: u16) -> u32 {
    let mut rank_mod = phase;
    let mut out = chunk;
    let mut deleted = 0_u32;

    for bit in 0..16 {
        let mask = 1_u16 << bit;
        if chunk & mask == 0 {
            continue;
        }

        rank_mod += 1;
        if rank_mod == step {
            out &= !mask;
            rank_mod = 0;
            deleted += 1;
        }
    }

    (out as u32) | (deleted << 16) | ((rank_mod as u32) << 21)
}

fn scan_delete_range_u16(bits: &mut [u64], start_rank_mod: u64, step: u16, actions: &[u32]) -> u64 {
    let mut rank_mod = start_rank_mod as usize;
    let mut deleted = 0_u64;

    for word in bits {
        let old = *word;
        let mut new_word = 0_u64;

        for chunk_idx in 0..4 {
            let shift = chunk_idx * 16;
            let chunk = ((old >> shift) & 0xFFFF) as usize;
            let action = actions[rank_mod * 65_536 + chunk];
            let new_chunk = action & 0xFFFF;
            deleted += ((action >> 16) & 0x1F) as u64;
            rank_mod = (action >> 21) as usize;
            new_word |= (new_chunk as u64) << shift;
        }

        *word = new_word;
    }

    debug_assert!((rank_mod as u64) < step as u64);
    deleted
}

fn lowbit(x: usize) -> usize {
    x & x.wrapping_neg()
}

fn highest_power_of_two_at_most(n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    1_usize << (usize::BITS - 1 - n.leading_zeros())
}

fn select_in_word(mut word: u64, rank: u32) -> u32 {
    debug_assert!(rank >= 1);
    debug_assert!(rank <= word.count_ones());

    for _ in 1..rank {
        word &= word - 1;
    }
    word.trailing_zeros()
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), AppError> {
    let cli = Cli::parse();
    let mut candidates = generate_candidates(cli.min, cli.max, CANDIDATE_GENERATORS);
    if candidates.is_empty() {
        return Ok(());
    }

    for candidate in &candidates {
        if matches!(candidate.state, CandidateState::Lucky { .. }) {
            print_candidate(candidate);
        }
    }

    let mut sieve = LuckySieve::new(sieve_size(cli.memory_mib, cli.sieve_limit.or(SIEVE_LIMIT))?)?;
    search_with_sieve(
        &mut sieve,
        &mut candidates,
        cli.threads.unwrap_or_else(threads).max(1),
        cli.scan_threshold,
        true,
    );
    print_summary(&candidates, sieve.value_limit());

    Ok(())
}

fn sieve_size(memory_mib: u64, sieve_limit: Option<u64>) -> Result<u64, AppError> {
    let budget = (memory_mib as u128) * 1024 * 1024;
    let words = budget.saturating_sub(8) / 16;
    if words == 0 {
        return Err(AppError::EmptySieve);
    }

    let max_words_from_u64 = (u64::MAX as u128).div_ceil(64);
    let budget_words = words.min(max_words_from_u64);
    let budget_n_odds = (budget_words * 64).min(((u64::MAX as u128) + 1) / 2);
    let n_odds = if let Some(limit) = sieve_limit {
        let limit_n_odds = ((limit as u128) + 1) / 2;
        budget_n_odds.min(limit_n_odds)
    } else {
        budget_n_odds
    };

    u64::try_from(n_odds)
        .map_err(|_| AppError::Allocation(format!("chosen odd count {n_odds} does not fit in u64; reduce MEMORY_MIB")))
}

fn threads() -> usize {
    thread::available_parallelism().map(usize::from).unwrap_or(1)
}

fn search_with_sieve(
    sieve: &mut LuckySieve,
    candidates: &mut [Candidate],
    threads: usize,
    scan_threshold: u64,
    announce: bool,
) {
    let mut factor_rank = 2_u64;
    let mut pending = candidates.iter().filter(|c| c.is_pending()).count();

    while pending > 0 {
        let Some(odd_index) = sieve.select(factor_rank) else {
            break;
        };
        let factor = odd_index.saturating_mul(2).saturating_sub(1);
        if factor > sieve.value_limit() {
            break;
        }

        pending -= apply_factor_to_candidates(factor, candidates, announce);

        if pending == 0 {
            break;
        }

        if factor <= sieve.alive {
            let deletions = sieve.alive / factor;
            if sieve.should_scan_delete(factor, deletions, scan_threshold) {
                sieve.delete_every_by_scan(factor, deletions, threads);
            } else {
                sieve.delete_every(factor);
            }
        }
        factor_rank += 1;
    }

    let final_factor_bound = sieve.value_limit() as u128;
    for candidate in candidates.iter_mut() {
        if let CandidateState::Pending { rank } = candidate.state {
            if rank <= final_factor_bound {
                candidate.state = CandidateState::Lucky { rank, proof_factor: sieve.value_limit() };
                if announce {
                    print_candidate(candidate);
                }
            } else {
                candidate.state = CandidateState::Inconclusive { rank };
                if announce {
                    print_candidate(candidate);
                }
            }
        }
    }
}

fn apply_factor_to_candidates(factor: u64, candidates: &mut [Candidate], announce: bool) -> usize {
    let factor128 = factor as u128;
    let mut completed = 0_usize;

    for candidate in candidates {
        let CandidateState::Pending { rank } = candidate.state else {
            continue;
        };

        if rank < factor128 {
            candidate.state = CandidateState::Lucky { rank, proof_factor: factor };
            completed += 1;
            if announce {
                print_candidate(candidate);
            }
            continue;
        }

        if rank % factor128 == 0 {
            candidate.state = CandidateState::Composite { rank, deletion_factor: factor };
            completed += 1;
            if announce {
                print_candidate(candidate);
            }
            continue;
        }

        let new_rank = rank - rank / factor128;
        if new_rank < factor128 {
            candidate.state = CandidateState::Lucky { rank: new_rank, proof_factor: factor };
            completed += 1;
            if announce {
                print_candidate(candidate);
            }
        } else {
            candidate.state = CandidateState::Pending { rank: new_rank };
        }
    }

    completed
}

fn print_summary(candidates: &[Candidate], sieve_limit: u64) {
    println!();
    println!("lucky sequences");

    for (shape, values) in lucky_sequences(candidates) {
        let text = values.iter().map(u64::to_string).collect::<Vec<_>>().join(", ");
        println!("{shape}: {text}");
    }

    let mut printed_inconclusive_header = false;
    for candidate in candidates {
        if matches!(candidate.state, CandidateState::Inconclusive { .. }) {
            if !printed_inconclusive_header {
                println!();
                println!("inconclusive (sieve_limit={sieve_limit})");
                printed_inconclusive_header = true;
            }
            print_inconclusive(candidate, sieve_limit);
        }
    }
}

fn lucky_sequences(candidates: &[Candidate]) -> BTreeMap<&'static str, Vec<u64>> {
    let mut sequences = BTreeMap::new();

    for candidate in candidates {
        if !matches!(candidate.state, CandidateState::Lucky { .. }) {
            continue;
        }

        for label in candidate.labels() {
            sequences.entry(label.shape).or_insert_with(Vec::new).push(candidate.n);
        }
    }

    for values in sequences.values_mut() {
        values.sort_unstable();
        values.dedup();
    }

    sequences
}

fn print_candidate(candidate: &Candidate) {
    match candidate.state {
        CandidateState::Lucky { rank, proof_factor } => {
            println!(
                "LUCKY {:>20}  labels={} final_rank={} proof_factor={}",
                candidate.n,
                candidate.labels_text(),
                rank,
                proof_factor
            );
        }
        CandidateState::Composite { rank, deletion_factor } => {
            println!(
                "reject {:>19}  labels={} deletion_rank={} deletion_factor={}",
                candidate.n,
                candidate.labels_text(),
                rank,
                deletion_factor
            );
        }
        CandidateState::Inconclusive { .. } => {}
        CandidateState::Pending { .. } => {
            unreachable!("pending candidates are not ready to print")
        }
    }
}

fn print_inconclusive(candidate: &Candidate, sieve_limit: u64) {
    match candidate.state {
        CandidateState::Inconclusive { rank } => {
            println!(
                "INCONCLUSIVE {:>13}  labels={} current_rank={} need factors beyond {} or more memory",
                candidate.n,
                candidate.labels_text(),
                rank,
                sieve_limit
            );
        }
        CandidateState::Pending { .. } => {
            unreachable!("pending states are finalized as inconclusive")
        }
        _ => {}
    }
}
