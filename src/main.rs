use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fs;
use std::num::ParseIntError;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_MIN: u64 = 0;
const DEFAULT_RESERVE_MIB: u64 = 1024;
const DEFAULT_PROGRESS_SECS: u64 = 5;
const DEFAULT_SCAN_THRESHOLD: u64 = 511;
const U16_SCAN_MAX_STEP: u64 = 2_048;
const SCAN_MIN_WORDS: u64 = 1_000_000;
const SCAN_LARGE_WORDS: u64 = 100_000_000;
const SCAN_SMALL_DELETION_WORD_DIVISOR: u64 = 4;
const SCAN_LARGE_DELETION_WORD_DIVISOR: u64 = 16;
const SCAN_PROGRESS_BLOCK_WORDS: usize = 65_536;
const REBUILD_PROGRESS_BLOCK_WORDS: usize = 1_000_000;

#[derive(Debug, Clone)]
struct Config {
    min: u64,
    max: u64,
    families: Vec<CandidateFamily>,
    sieve_limit: Option<u64>,
    memory_mib: Option<u64>,
    reserve_mib: u64,
    progress_every: Duration,
    threads: usize,
    scan_threshold: u64,
    self_test: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            min: DEFAULT_MIN,
            max: u64::MAX,
            families: vec![CandidateFamily::Repdigit],
            sieve_limit: None,
            memory_mib: None,
            reserve_mib: DEFAULT_RESERVE_MIB,
            progress_every: Duration::from_secs(DEFAULT_PROGRESS_SECS),
            threads: thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1),
            scan_threshold: DEFAULT_SCAN_THRESHOLD,
            self_test: false,
        }
    }
}

#[derive(Debug)]
enum AppError {
    Args(String),
    Parse(ParseIntError),
    Allocation(String),
    EmptySieve,
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::Args(msg) => write!(f, "{msg}"),
            AppError::Parse(err) => write!(f, "{err}"),
            AppError::Allocation(msg) => write!(f, "{msg}"),
            AppError::EmptySieve => write!(f, "sieve memory budget is too small to hold any words"),
        }
    }
}

impl From<ParseIntError> for AppError {
    fn from(value: ParseIntError) -> Self {
        AppError::Parse(value)
    }
}

#[derive(Debug, Clone)]
struct Candidate {
    n: u64,
    labels: Vec<String>,
    state: CandidateState,
}

#[derive(Debug, Clone)]
enum CandidateState {
    Pending { rank: u128 },
    Lucky { rank: u128, proof_factor: u64 },
    Composite { rank: u128, deletion_factor: u64 },
    Inconclusive { rank: u128 },
}

impl Candidate {
    fn new(n: u64, labels: Vec<String>) -> Self {
        let state = if n == 1 {
            CandidateState::Lucky {
                rank: 1,
                proof_factor: 0,
            }
        } else {
            CandidateState::Pending {
                rank: ((n as u128) + 1) / 2,
            }
        };

        Self { n, labels, state }
    }

    fn is_pending(&self) -> bool {
        matches!(self.state, CandidateState::Pending { .. })
    }

    fn current_rank(&self) -> u128 {
        match self.state {
            CandidateState::Pending { rank }
            | CandidateState::Lucky { rank, .. }
            | CandidateState::Composite { rank, .. }
            | CandidateState::Inconclusive { rank } => rank,
        }
    }

    fn labels_text(&self) -> String {
        self.labels.join(";")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateFamily {
    Repdigit,
    Mersenne,
    MersennePrimeExponent,
    Fibonacci,
    Lucas,
    Tetranacci,
    ConsecutiveDigits,
}

impl CandidateFamily {
    const ALL: [Self; 7] = [
        Self::Repdigit,
        Self::Mersenne,
        Self::MersennePrimeExponent,
        Self::Fibonacci,
        Self::Lucas,
        Self::Tetranacci,
        Self::ConsecutiveDigits,
    ];

    fn parse(name: &str) -> Option<Self> {
        match name {
            "repdigit" | "repdigits" => Some(Self::Repdigit),
            "mersenne" | "mersennes" => Some(Self::Mersenne),
            "mersenne-prime-exp" | "mersenne-prime-exponent" | "prime-mersenne" => {
                Some(Self::MersennePrimeExponent)
            }
            "fibonacci" | "fib" => Some(Self::Fibonacci),
            "lucas" => Some(Self::Lucas),
            "tetranacci" => Some(Self::Tetranacci),
            "consecutive-digits" | "digit-run" | "digit-runs" => Some(Self::ConsecutiveDigits),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Repdigit => "repdigit",
            Self::Mersenne => "mersenne",
            Self::MersennePrimeExponent => "mersenne-prime-exp",
            Self::Fibonacci => "fibonacci",
            Self::Lucas => "lucas",
            Self::Tetranacci => "tetranacci",
            Self::ConsecutiveDigits => "consecutive-digits",
        }
    }
}

#[derive(Default)]
struct CandidateBuilder {
    labels_by_n: BTreeMap<u64, Vec<String>>,
}

impl CandidateBuilder {
    fn add(&mut self, n: u128, min: u64, max: u64, label: String) {
        if n > u64::MAX as u128 || n < min as u128 || n > max as u128 {
            return;
        }

        let n = n as u64;
        if n != 1 && n % 2 == 0 {
            return;
        }

        let labels = self.labels_by_n.entry(n).or_default();
        if !labels.iter().any(|existing| existing == &label) {
            labels.push(label);
        }
    }

    fn into_candidates(self) -> Vec<Candidate> {
        self.labels_by_n
            .into_iter()
            .map(|(n, labels)| Candidate::new(n, labels))
            .collect()
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
            AppError::Allocation(format!(
                "sieve needs {words_u64} words, which does not fit usize on this target"
            ))
        })?;

        let mut bits = Vec::new();
        bits.try_reserve_exact(words).map_err(|err| {
            AppError::Allocation(format!("failed to reserve bitset for {words} words: {err}"))
        })?;
        bits.resize(words, !0_u64);

        let used_bits_in_last = (n_odds - 1) % 64 + 1;
        if used_bits_in_last < 64 {
            let mask = (1_u64 << used_bits_in_last) - 1;
            let last = bits.len() - 1;
            bits[last] = mask;
        }

        let mut tree = Vec::new();
        tree.try_reserve_exact(words + 1).map_err(|err| {
            AppError::Allocation(format!(
                "failed to reserve Fenwick tree for {} words: {err}",
                words + 1
            ))
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

        Ok(Self {
            bits,
            tree,
            n_odds,
            alive: n_odds,
        })
    }

    fn value_limit(&self) -> u64 {
        self.n_odds.saturating_mul(2).saturating_sub(1)
    }

    fn memory_bytes(&self) -> u128 {
        ((self.bits.len() as u128) + (self.tree.len() as u128)) * 8
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
        self.delete_every_impl(step, None)
    }

    fn delete_every_with_progress(
        &mut self,
        step: u64,
        started: Instant,
        next_progress: &mut Instant,
        progress_every: Duration,
        threads: usize,
        scan_threshold: u64,
    ) -> u64 {
        let deletions = self.alive / step;
        if self.should_scan_delete(step, deletions, scan_threshold) {
            return self.delete_every_by_scan(
                step,
                deletions,
                started,
                next_progress,
                progress_every,
                threads,
            );
        }

        self.delete_every_impl(
            step,
            Some(DeleteProgress {
                started,
                next_progress,
                progress_every,
            }),
        )
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

    fn delete_every_by_scan(
        &mut self,
        step: u64,
        expected_deletions: u64,
        started: Instant,
        next_progress: &mut Instant,
        progress_every: Duration,
        threads: usize,
    ) -> u64 {
        let threads = threads.max(1).min(self.bits.len().max(1));
        let old_alive = self.alive;
        let deleted = if step == 3 && self.alive == self.n_odds {
            println!(
                "scan-delete factor=3 method=initial-mask threads={} words={}",
                threads,
                self.bits.len()
            );
            self.delete_initial_every_three(started, next_progress, progress_every, threads)
        } else {
            println!(
                "scan-delete factor={} method=u16-table threads={} words={} table={}",
                step,
                threads,
                self.bits.len(),
                human_bytes((step as u128) * 65_536 * 4)
            );
            self.delete_every_by_u16_scan(step, started, next_progress, progress_every, threads)
        };

        assert_eq!(
            deleted, expected_deletions,
            "scan deletion count mismatch for factor {step}"
        );
        self.alive = old_alive - deleted;
        self.rebuild_tree_with_progress(started, next_progress, progress_every);
        deleted
    }

    fn delete_initial_every_three(
        &mut self,
        started: Instant,
        next_progress: &mut Instant,
        progress_every: Duration,
        threads: usize,
    ) -> u64 {
        let keep_masks = initial_keep_masks_for_three();
        let deleted = self.n_odds / 3;
        let total_words = self.bits.len();
        let done_words = AtomicU64::new(0);
        let finished = AtomicBool::new(false);
        let chunk_words = total_words.div_ceil(threads);

        thread::scope(|scope| {
            let monitor = scope.spawn(|| {
                monitor_scan_progress(
                    "scan-delete",
                    3,
                    total_words as u64,
                    &done_words,
                    &finished,
                    started,
                    next_progress,
                    progress_every,
                );
            });

            let mut handles = Vec::with_capacity(threads);
            for (chunk_idx, chunk) in self.bits.chunks_mut(chunk_words).enumerate() {
                let start_word = chunk_idx * chunk_words;
                let done_words = &done_words;
                let keep_masks = keep_masks;
                handles.push(scope.spawn(move || {
                    for (block_idx, block) in
                        chunk.chunks_mut(SCAN_PROGRESS_BLOCK_WORDS).enumerate()
                    {
                        let block_start = start_word + block_idx * SCAN_PROGRESS_BLOCK_WORDS;
                        for (offset, word) in block.iter_mut().enumerate() {
                            *word &= keep_masks[(block_start + offset) % 3];
                        }
                        done_words.fetch_add(block.len() as u64, Ordering::Relaxed);
                    }
                }));
            }

            for handle in handles {
                handle.join().expect("initial scan worker panicked");
            }
            finished.store(true, Ordering::Release);
            monitor.join().expect("scan progress monitor panicked");
        });

        deleted
    }

    fn delete_every_by_u16_scan(
        &mut self,
        step: u64,
        started: Instant,
        next_progress: &mut Instant,
        progress_every: Duration,
        threads: usize,
    ) -> u64 {
        let actions = build_u16_actions(step as u16);
        let ranges = self.scan_ranges(threads, step);
        let total_words = self.bits.len();
        let done_words = AtomicU64::new(0);
        let finished = AtomicBool::new(false);
        let chunk_words = total_words.div_ceil(ranges.len().max(1));
        let mut deleted = 0_u64;

        thread::scope(|scope| {
            let monitor = scope.spawn(|| {
                monitor_scan_progress(
                    "scan-delete",
                    step,
                    total_words as u64,
                    &done_words,
                    &finished,
                    started,
                    next_progress,
                    progress_every,
                );
            });

            let mut handles = Vec::with_capacity(ranges.len());
            for (range, chunk) in ranges.into_iter().zip(self.bits.chunks_mut(chunk_words)) {
                let actions = &actions;
                let done_words = &done_words;
                handles.push(scope.spawn(move || {
                    scan_delete_range_u16(
                        chunk,
                        range.start_rank_mod,
                        step as u16,
                        actions,
                        done_words,
                    )
                }));
            }

            for handle in handles {
                deleted += handle.join().expect("scan worker panicked");
            }

            finished.store(true, Ordering::Release);
            monitor.join().expect("scan progress monitor panicked");
        });

        deleted
    }

    fn scan_ranges(&self, threads: usize, step: u64) -> Vec<ScanRange> {
        let total_words = self.bits.len();
        let threads = threads.max(1).min(total_words.max(1));
        let chunk_words = total_words.div_ceil(threads);
        let mut ranges = Vec::with_capacity(threads);

        for start_word in (0..total_words).step_by(chunk_words) {
            ranges.push(ScanRange {
                start_rank_mod: self.prefix_word_count(start_word) % step,
            });
        }

        ranges
    }

    fn rebuild_tree_with_progress(
        &mut self,
        started: Instant,
        next_progress: &mut Instant,
        progress_every: Duration,
    ) {
        let words = self.bits.len();
        self.tree[0] = 0;

        for (i, word) in self.bits.iter().enumerate() {
            self.tree[i + 1] = word.count_ones() as u64;
            if (i + 1) % REBUILD_PROGRESS_BLOCK_WORDS == 0 {
                maybe_print_rebuild_progress(
                    "rebuild-counts",
                    i + 1,
                    words,
                    started,
                    next_progress,
                    progress_every,
                );
            }
        }

        for i in 1..=words {
            let parent = i + lowbit(i);
            if parent <= words {
                self.tree[parent] += self.tree[i];
            }
            if i % REBUILD_PROGRESS_BLOCK_WORDS == 0 {
                maybe_print_rebuild_progress(
                    "rebuild-fenwick",
                    i,
                    words,
                    started,
                    next_progress,
                    progress_every,
                );
            }
        }

        maybe_print_rebuild_progress(
            "rebuild-fenwick",
            words,
            words,
            started,
            next_progress,
            progress_every,
        );
    }

    fn delete_every_impl(&mut self, step: u64, mut progress: Option<DeleteProgress<'_>>) -> u64 {
        let deletions = self.alive / step;
        let mut deleted_in_pass = 0_u64;

        for multiple in (1..=deletions).rev() {
            let rank = multiple * step;
            let odd_index = self
                .select(rank)
                .expect("rank chosen from alive count must be selectable");
            self.clear(odd_index);

            deleted_in_pass += 1;
            if deleted_in_pass & 0xFFFFF == 0 {
                if let Some(progress) = progress.as_mut() {
                    progress.maybe_print(step, deleted_in_pass, deletions, self.alive);
                }
            }
        }

        if let Some(progress) = progress.as_mut() {
            progress.maybe_print(step, deleted_in_pass, deletions, self.alive);
        }

        deletions
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

fn initial_keep_masks_for_three() -> [u64; 3] {
    let mut masks = [0_u64; 3];
    for word_mod in 0..3 {
        let mut mask = 0_u64;
        for bit in 0..64 {
            let rank_mod = ((word_mod * 64 + bit + 1) % 3) as u64;
            if rank_mod != 0 {
                mask |= 1_u64 << bit;
            }
        }
        masks[word_mod] = mask;
    }
    masks
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

fn scan_delete_range_u16(
    bits: &mut [u64],
    start_rank_mod: u64,
    step: u16,
    actions: &[u32],
    done_words: &AtomicU64,
) -> u64 {
    let mut rank_mod = start_rank_mod as usize;
    let mut deleted = 0_u64;

    for block in bits.chunks_mut(SCAN_PROGRESS_BLOCK_WORDS) {
        for word in block.iter_mut() {
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
        done_words.fetch_add(block.len() as u64, Ordering::Relaxed);
    }

    debug_assert!((rank_mod as u64) < step as u64);
    deleted
}

fn monitor_scan_progress(
    label: &str,
    factor: u64,
    total_words: u64,
    done_words: &AtomicU64,
    finished: &AtomicBool,
    started: Instant,
    next_progress: &mut Instant,
    progress_every: Duration,
) {
    while !finished.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(50));
        let now = Instant::now();
        if now < *next_progress {
            continue;
        }

        let done = done_words.load(Ordering::Relaxed).min(total_words);
        println!(
            "{} elapsed={} factor={} words={}/{}",
            label,
            human_duration(started.elapsed()),
            factor,
            done,
            total_words
        );
        *next_progress = now + progress_every;
    }
}

fn maybe_print_rebuild_progress(
    label: &str,
    done: usize,
    total: usize,
    started: Instant,
    next_progress: &mut Instant,
    progress_every: Duration,
) {
    let now = Instant::now();
    if now < *next_progress {
        return;
    }

    println!(
        "{} elapsed={} words={}/{}",
        label,
        human_duration(started.elapsed()),
        done,
        total
    );
    *next_progress = now + progress_every;
}

struct DeleteProgress<'a> {
    started: Instant,
    next_progress: &'a mut Instant,
    progress_every: Duration,
}

impl DeleteProgress<'_> {
    fn maybe_print(&mut self, factor: u64, done: u64, total: u64, alive: u64) {
        let now = Instant::now();
        if now < *self.next_progress {
            return;
        }

        println!(
            "delete-progress elapsed={} factor={} deleted_in_pass={}/{} alive={}",
            human_duration(self.started.elapsed()),
            factor,
            done,
            total,
            alive
        );
        *self.next_progress = now + self.progress_every;
    }
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
    let config = parse_args(env::args().skip(1))?;

    if config.self_test {
        self_test()?;
        return Ok(());
    }

    let mut candidates = generate_candidates(&config);
    if candidates.is_empty() {
        println!(
            "no odd candidates for families [{}] in [{}, {}]",
            family_names(&config.families),
            config.min,
            config.max
        );
        return Ok(());
    }

    println!(
        "searching {} odd candidates for families [{}] in [{}, {}]",
        candidates.len(),
        family_names(&config.families),
        config.min,
        config.max
    );
    println!(
        "runtime: threads={} scan_threshold={} dense_scan_min_words={} dense_scan_min_deletions=words/{} small, words/{} large",
        config.threads,
        config.scan_threshold,
        SCAN_MIN_WORDS,
        SCAN_SMALL_DELETION_WORD_DIVISOR,
        SCAN_LARGE_DELETION_WORD_DIVISOR
    );
    for candidate in &candidates {
        println!(
            "candidate {:>20}  labels={} initial_rank={}",
            candidate.n,
            candidate.labels_text(),
            candidate.current_rank()
        );
    }

    let target_n_odds = choose_sieve_size(&config)?;
    let target_limit = target_n_odds
        .checked_mul(2)
        .and_then(|x| x.checked_sub(1))
        .unwrap_or(u64::MAX);
    let target_words = target_n_odds.div_ceil(64);

    println!(
        "building sieve: odd_values={} words={} value_limit={} planned_memory={}",
        target_n_odds,
        target_words,
        target_limit,
        human_bytes((target_words as u128) * 16 + 8)
    );

    let mut sieve = allocate_sieve_with_backoff(target_n_odds)?;
    println!(
        "sieve ready: value_limit={} alive={} memory={}",
        sieve.value_limit(),
        sieve.alive,
        human_bytes(sieve.memory_bytes())
    );

    search_with_sieve(
        &mut sieve,
        &mut candidates,
        config.progress_every,
        config.threads,
        config.scan_threshold,
    );
    print_summary(&candidates, sieve.value_limit());

    Ok(())
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Config, AppError> {
    let mut config = Config::default();
    let mut saw_family_arg = false;
    let mut args = args.peekable();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--min" => config.min = parse_next_u64(&mut args, "--min")?,
            "--max" => config.max = parse_next_u64(&mut args, "--max")?,
            "--family" | "--families" | "--kind" => {
                let value = args
                    .next()
                    .ok_or_else(|| AppError::Args(format!("{arg} needs a value")))?;
                let parsed = parse_family_list(&value)?;
                if !saw_family_arg {
                    config.families.clear();
                    saw_family_arg = true;
                }
                config.families.extend(parsed);
            }
            "--limit" => config.sieve_limit = Some(parse_next_u64(&mut args, "--limit")?),
            "--memory-mib" => config.memory_mib = Some(parse_next_u64(&mut args, "--memory-mib")?),
            "--reserve-mib" => config.reserve_mib = parse_next_u64(&mut args, "--reserve-mib")?,
            "--threads" => {
                let threads = parse_next_u64(&mut args, "--threads")?;
                if threads == 0 {
                    return Err(AppError::Args("--threads must be at least 1".to_string()));
                }
                config.threads = usize::try_from(threads).map_err(|_| {
                    AppError::Args(format!("--threads value {threads} does not fit usize"))
                })?;
            }
            "--scan-threshold" => {
                config.scan_threshold = parse_next_u64(&mut args, "--scan-threshold")?
            }
            "--progress-seconds" => {
                config.progress_every =
                    Duration::from_secs(parse_next_u64(&mut args, "--progress-seconds")?)
            }
            "--self-test" => config.self_test = true,
            _ => {
                return Err(AppError::Args(format!(
                    "unknown argument {arg}; run with --help"
                )))
            }
        }
    }

    if config.min > config.max {
        return Err(AppError::Args(format!(
            "--min {} is greater than --max {}",
            config.min, config.max
        )));
    }

    dedup_families(&mut config.families);
    if config.families.is_empty() {
        return Err(AppError::Args(
            "at least one --family value is required".to_string(),
        ));
    }

    Ok(config)
}

fn parse_family_list(value: &str) -> Result<Vec<CandidateFamily>, AppError> {
    let mut families = Vec::new();
    for item in value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if item == "all" {
            families.extend(CandidateFamily::ALL);
            continue;
        }
        let Some(family) = CandidateFamily::parse(item) else {
            return Err(AppError::Args(format!(
                "unknown family {item}; expected one of {} or all",
                available_family_names()
            )));
        };
        families.push(family);
    }
    Ok(families)
}

fn dedup_families(families: &mut Vec<CandidateFamily>) {
    let mut deduped = Vec::new();
    for family in families.iter().copied() {
        if !deduped.contains(&family) {
            deduped.push(family);
        }
    }
    *families = deduped;
}

fn family_names(families: &[CandidateFamily]) -> String {
    families
        .iter()
        .map(|family| family.name())
        .collect::<Vec<_>>()
        .join(",")
}

fn available_family_names() -> String {
    CandidateFamily::ALL
        .iter()
        .map(|family| family.name())
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_next_u64(
    args: &mut std::iter::Peekable<impl Iterator<Item = String>>,
    flag: &str,
) -> Result<u64, AppError> {
    let value = args
        .next()
        .ok_or_else(|| AppError::Args(format!("{flag} needs a value")))?;
    Ok(value.replace('_', "").parse()?)
}

fn print_help() {
    println!(
        "\
lucky-number sparse-family search

USAGE:
    cargo run --release -- [OPTIONS]

OPTIONS:
    --family LIST          Candidate families to test [default: repdigit]
                           LIST is comma-separated; use all for every sparse family
                           values: repdigit, mersenne, mersenne-prime-exp,
                           fibonacci, lucas, tetranacci, consecutive-digits
    --min N                Smallest candidate value to test [default: 0]
    --max N                Largest candidate value to test [default: u64::MAX]
    --limit N              Maximum lucky deletion factor value to generate
    --memory-mib M         Fixed sieve memory budget in MiB
    --reserve-mib M        Memory to leave free when auto-sizing [default: 1024]
    --threads N            Worker threads for dense scan passes [default: CPU parallelism]
    --scan-threshold N     Highest factor eligible for scan/rebuild [default: 511, cap: 2048]
    --progress-seconds S   Seconds between progress lines [default: 5]
    --self-test            Run a small correctness check

The default sieve size uses MemAvailable from /proc/meminfo minus --reserve-mib.
The sieve stores odd values in a u64 bitset plus a Fenwick tree over word popcounts.
Dense early deletion passes use parallel bitset scans and Fenwick rebuilds.
"
    );
}

fn choose_sieve_size(config: &Config) -> Result<u64, AppError> {
    let budget = match config.memory_mib {
        Some(mib) => (mib as u128) * 1024 * 1024,
        None => {
            let available = mem_available_bytes().ok_or_else(|| {
                AppError::Args(
                    "could not read MemAvailable; pass --memory-mib or --limit".to_string(),
                )
            })?;
            let reserve = (config.reserve_mib as u128) * 1024 * 1024;
            available.saturating_sub(reserve)
        }
    };

    let words = budget.saturating_sub(8) / 16;
    if words == 0 {
        return Err(AppError::EmptySieve);
    }

    let max_words_from_u64 = (u64::MAX as u128).div_ceil(64);
    let budget_words = words.min(max_words_from_u64);
    let budget_n_odds = (budget_words * 64).min(((u64::MAX as u128) + 1) / 2);
    let n_odds = if let Some(limit) = config.sieve_limit {
        let limit_n_odds = ((limit as u128) + 1) / 2;
        budget_n_odds.min(limit_n_odds)
    } else {
        budget_n_odds
    };

    u64::try_from(n_odds).map_err(|_| {
        AppError::Allocation(format!(
            "chosen odd count {n_odds} does not fit in u64; reduce --memory-mib"
        ))
    })
}

fn allocate_sieve_with_backoff(target_n_odds: u64) -> Result<LuckySieve, AppError> {
    let mut n_odds = target_n_odds;

    loop {
        match LuckySieve::new(n_odds) {
            Ok(sieve) => {
                if n_odds != target_n_odds {
                    println!(
                        "allocation succeeded after reducing sieve to odd_values={} value_limit={}",
                        n_odds,
                        sieve.value_limit()
                    );
                }
                return Ok(sieve);
            }
            Err(err) => {
                let reduced = n_odds / 10 * 9;
                if reduced == n_odds || reduced == 0 {
                    return Err(err);
                }
                println!(
                    "allocation failed at odd_values={}; retrying with {}",
                    n_odds, reduced
                );
                n_odds = reduced;
            }
        }
    }
}

fn mem_available_bytes() -> Option<u128> {
    let meminfo = fs::read_to_string("/proc/meminfo").ok()?;
    for line in meminfo.lines() {
        let Some(rest) = line.strip_prefix("MemAvailable:") else {
            continue;
        };
        let kb = rest.split_whitespace().next()?.parse::<u128>().ok()?;
        return Some(kb * 1024);
    }
    None
}

fn generate_candidates(config: &Config) -> Vec<Candidate> {
    let mut builder = CandidateBuilder::default();
    for family in &config.families {
        match family {
            CandidateFamily::Repdigit => repdigit_candidates(config.min, config.max, &mut builder),
            CandidateFamily::Mersenne => mersenne_candidates(config.min, config.max, &mut builder),
            CandidateFamily::MersennePrimeExponent => {
                mersenne_prime_exponent_candidates(config.min, config.max, &mut builder)
            }
            CandidateFamily::Fibonacci => {
                fibonacci_candidates(config.min, config.max, &mut builder)
            }
            CandidateFamily::Lucas => lucas_candidates(config.min, config.max, &mut builder),
            CandidateFamily::Tetranacci => {
                tetranacci_candidates(config.min, config.max, &mut builder)
            }
            CandidateFamily::ConsecutiveDigits => {
                consecutive_digit_candidates(config.min, config.max, &mut builder)
            }
        }
    }
    builder.into_candidates()
}

fn repdigit_candidates(min: u64, max: u64, builder: &mut CandidateBuilder) {
    let mut repunit = 0_u128;

    for len in 1..=20_u8 {
        repunit = repunit * 10 + 1;
        for digit in [1_u8, 3, 5, 7, 9] {
            let n = repunit * (digit as u128);
            builder.add(n, min, max, format!("repdigit(d={digit},len={len})"));
        }
    }
}

fn mersenne_candidates(min: u64, max: u64, builder: &mut CandidateBuilder) {
    for exponent in 1..=64_u32 {
        let Some(n) = (1_u128).checked_shl(exponent) else {
            break;
        };
        builder.add(n - 1, min, max, format!("mersenne(k={exponent})"));
    }
}

fn mersenne_prime_exponent_candidates(min: u64, max: u64, builder: &mut CandidateBuilder) {
    for exponent in 2..=64_u32 {
        if !is_prime_u32(exponent) {
            continue;
        }
        let Some(n) = (1_u128).checked_shl(exponent) else {
            break;
        };
        builder.add(n - 1, min, max, format!("mersenne-prime-exp(p={exponent})"));
    }
}

fn fibonacci_candidates(min: u64, max: u64, builder: &mut CandidateBuilder) {
    let mut prev = 0_u128;
    let mut curr = 1_u128;
    builder.add(prev, min, max, "fibonacci(k=0)".to_string());
    builder.add(curr, min, max, "fibonacci(k=1)".to_string());

    for index in 2_u32.. {
        let Some(next) = prev.checked_add(curr) else {
            break;
        };
        if next > u64::MAX as u128 {
            break;
        }
        builder.add(next, min, max, format!("fibonacci(k={index})"));
        prev = curr;
        curr = next;
    }
}

fn lucas_candidates(min: u64, max: u64, builder: &mut CandidateBuilder) {
    let mut prev = 2_u128;
    let mut curr = 1_u128;
    builder.add(prev, min, max, "lucas(k=0)".to_string());
    builder.add(curr, min, max, "lucas(k=1)".to_string());

    for index in 2_u32.. {
        let Some(next) = prev.checked_add(curr) else {
            break;
        };
        if next > u64::MAX as u128 {
            break;
        }
        builder.add(next, min, max, format!("lucas(k={index})"));
        prev = curr;
        curr = next;
    }
}

fn tetranacci_candidates(min: u64, max: u64, builder: &mut CandidateBuilder) {
    let mut window = [0_u128, 0, 0, 1];
    for (index, n) in window.iter().copied().enumerate() {
        builder.add(n, min, max, format!("tetranacci(k={index})"));
    }

    for index in 4_u32.. {
        let Some(next) = window.into_iter().try_fold(0_u128, u128::checked_add) else {
            break;
        };
        if next > u64::MAX as u128 {
            break;
        }
        builder.add(next, min, max, format!("tetranacci(k={index})"));
        window = [window[1], window[2], window[3], next];
    }
}

fn consecutive_digit_candidates(min: u64, max: u64, builder: &mut CandidateBuilder) {
    for start in 1_u8..=9 {
        for step in [-1_i8, 1] {
            let mut n = start as u128;
            let mut digit = start;
            for len in 2_u8..=20 {
                digit = next_digit_mod_10(digit, step);
                n = n * 10 + digit as u128;
                let direction = if step > 0 { "asc" } else { "desc" };
                builder.add(
                    n,
                    min,
                    max,
                    format!("consecutive-digits({direction},start={start},len={len})"),
                );
            }
        }
    }
}

fn next_digit_mod_10(digit: u8, step: i8) -> u8 {
    ((digit as i8 + step).rem_euclid(10)) as u8
}

fn is_prime_u32(n: u32) -> bool {
    if n < 2 {
        return false;
    }
    if n == 2 {
        return true;
    }
    if n % 2 == 0 {
        return false;
    }
    let mut divisor = 3_u32;
    while divisor * divisor <= n {
        if n % divisor == 0 {
            return false;
        }
        divisor += 2;
    }
    true
}

fn search_with_sieve(
    sieve: &mut LuckySieve,
    candidates: &mut [Candidate],
    progress_every: Duration,
    threads: usize,
    scan_threshold: u64,
) {
    let started = Instant::now();
    let mut next_progress = Instant::now() + progress_every;
    let mut factor_rank = 2_u64;
    let mut processed_factors = 0_u64;
    let mut total_deleted = 0_u64;
    let mut pending = candidates.iter().filter(|c| c.is_pending()).count();

    while pending > 0 {
        let Some(odd_index) = sieve.select(factor_rank) else {
            break;
        };
        let factor = odd_index.saturating_mul(2).saturating_sub(1);
        if factor > sieve.value_limit() {
            break;
        }

        processed_factors += 1;
        pending -= apply_factor_to_candidates(factor, candidates, true);

        if pending == 0 {
            break;
        }

        let deleted = if factor <= sieve.alive {
            sieve.delete_every_with_progress(
                factor,
                started,
                &mut next_progress,
                progress_every,
                threads,
                scan_threshold,
            )
        } else {
            0
        };
        total_deleted += deleted;
        factor_rank += 1;

        if Instant::now() >= next_progress {
            print_progress(
                started.elapsed(),
                factor,
                factor_rank,
                processed_factors,
                sieve.alive,
                total_deleted,
                candidates,
            );
            next_progress = Instant::now() + progress_every;
        }
    }

    let final_factor_bound = sieve.value_limit() as u128;
    for candidate in candidates.iter_mut() {
        if let CandidateState::Pending { rank } = candidate.state {
            if rank <= final_factor_bound {
                candidate.state = CandidateState::Lucky {
                    rank,
                    proof_factor: sieve.value_limit(),
                };
                println!(
                    "LUCKY {:>20}  labels={} rank={} after exhausting factors <= {}",
                    candidate.n,
                    candidate.labels_text(),
                    rank,
                    sieve.value_limit()
                );
            } else {
                candidate.state = CandidateState::Inconclusive { rank };
            }
        }
    }

    print_progress(
        started.elapsed(),
        0,
        factor_rank,
        processed_factors,
        sieve.alive,
        total_deleted,
        candidates,
    );
}

fn apply_factor_to_candidates(factor: u64, candidates: &mut [Candidate], announce: bool) -> usize {
    let factor128 = factor as u128;
    let mut completed = 0_usize;

    for candidate in candidates {
        let CandidateState::Pending { rank } = candidate.state else {
            continue;
        };

        if rank < factor128 {
            candidate.state = CandidateState::Lucky {
                rank,
                proof_factor: factor,
            };
            completed += 1;
            if announce {
                println!(
                    "LUCKY {:>20}  labels={} rank={} before factor {}",
                    candidate.n,
                    candidate.labels_text(),
                    rank,
                    factor
                );
            }
            continue;
        }

        if rank % factor128 == 0 {
            candidate.state = CandidateState::Composite {
                rank,
                deletion_factor: factor,
            };
            completed += 1;
            if announce {
                println!(
                    "reject {:>19}  labels={} rank={} deleted by factor {}",
                    candidate.n,
                    candidate.labels_text(),
                    rank,
                    factor
                );
            }
            continue;
        }

        let new_rank = rank - rank / factor128;
        if new_rank < factor128 {
            candidate.state = CandidateState::Lucky {
                rank: new_rank,
                proof_factor: factor,
            };
            completed += 1;
            if announce {
                println!(
                    "LUCKY {:>20}  labels={} rank={} after factor {}",
                    candidate.n,
                    candidate.labels_text(),
                    new_rank,
                    factor
                );
            }
        } else {
            candidate.state = CandidateState::Pending { rank: new_rank };
        }
    }

    completed
}

fn print_progress(
    elapsed: Duration,
    factor: u64,
    factor_rank: u64,
    processed_factors: u64,
    alive: u64,
    total_deleted: u64,
    candidates: &[Candidate],
) {
    let pending = candidates.iter().filter(|c| c.is_pending()).count();
    let lucky = candidates
        .iter()
        .filter(|c| matches!(c.state, CandidateState::Lucky { .. }))
        .count();
    let composite = candidates
        .iter()
        .filter(|c| matches!(c.state, CandidateState::Composite { .. }))
        .count();
    let inconclusive = candidates
        .iter()
        .filter(|c| matches!(c.state, CandidateState::Inconclusive { .. }))
        .count();
    let max_rank = candidates
        .iter()
        .filter_map(|c| match c.state {
            CandidateState::Pending { rank } | CandidateState::Inconclusive { rank } => Some(rank),
            _ => None,
        })
        .max()
        .unwrap_or(0);

    println!(
        "progress elapsed={} factor={} factor_rank={} processed={} alive={} deleted={} pending={} lucky={} rejected={} inconclusive={} max_pending_rank={}",
        human_duration(elapsed),
        factor,
        factor_rank,
        processed_factors,
        alive,
        total_deleted,
        pending,
        lucky,
        composite,
        inconclusive,
        max_rank
    );
}

fn print_summary(candidates: &[Candidate], sieve_limit: u64) {
    println!();
    println!("summary (sieve_limit={sieve_limit})");

    for candidate in candidates {
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
            CandidateState::Composite {
                rank,
                deletion_factor,
            } => {
                println!(
                    "reject {:>19}  labels={} deletion_rank={} deletion_factor={}",
                    candidate.n,
                    candidate.labels_text(),
                    rank,
                    deletion_factor
                );
            }
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
        }
    }
}

fn human_bytes(bytes: u128) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.2} {}", UNITS[unit])
}

fn human_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    let secs = secs % 60;
    if hours > 0 {
        format!("{hours}h{mins:02}m{secs:02}s")
    } else if mins > 0 {
        format!("{mins}m{secs:02}s")
    } else {
        format!("{secs}s")
    }
}

fn self_test() -> Result<(), AppError> {
    let known_lucky = [
        1_u64, 3, 7, 9, 13, 15, 21, 25, 31, 33, 37, 43, 49, 51, 63, 67, 69, 73, 75, 79,
    ];
    let known_not_lucky = [
        5_u64, 11, 17, 19, 23, 27, 29, 35, 39, 41, 45, 47, 53, 55, 57, 59,
    ];

    for n in known_lucky {
        if !prove_small(n)? {
            return Err(AppError::Args(format!(
                "self-test failed: expected {n} lucky"
            )));
        }
    }

    for n in known_not_lucky {
        if prove_small(n)? {
            return Err(AppError::Args(format!(
                "self-test failed: expected {n} not lucky"
            )));
        }
    }

    scan_delete_self_test()?;
    candidate_generation_self_test()?;

    println!("self-test passed");
    Ok(())
}

fn candidate_generation_self_test() -> Result<(), AppError> {
    let repdigit_config = Config {
        max: 1_000,
        ..Config::default()
    };
    let repdigits = generate_candidates(&repdigit_config);
    if repdigits.len() != 15 {
        return Err(AppError::Args(format!(
            "candidate self-test failed: expected 15 repdigits <= 1000, got {}",
            repdigits.len()
        )));
    }
    if !candidate_has_label(&repdigits, 333, "repdigit(d=3,len=3)") {
        return Err(AppError::Args(
            "candidate self-test failed: missing 333 repdigit label".to_string(),
        ));
    }

    let all_config = Config {
        max: 1_000,
        families: CandidateFamily::ALL.to_vec(),
        ..Config::default()
    };
    let all = generate_candidates(&all_config);
    for (n, label) in [
        (13, "fibonacci(k=7)"),
        (15, "tetranacci(k=8)"),
        (31, "mersenne-prime-exp(p=5)"),
        (321, "consecutive-digits(desc,start=3,len=3)"),
    ] {
        if !candidate_has_label(&all, n, label) {
            return Err(AppError::Args(format!(
                "candidate self-test failed: missing {n} label {label}"
            )));
        }
    }

    Ok(())
}

fn candidate_has_label(candidates: &[Candidate], n: u64, label: &str) -> bool {
    candidates
        .iter()
        .any(|candidate| candidate.n == n && candidate.labels.iter().any(|item| item == label))
}

fn scan_delete_self_test() -> Result<(), AppError> {
    let mut reference = LuckySieve::new(1_000_000)?;
    let mut scanned = LuckySieve::new(1_000_000)?;
    let mut factor_rank = 2_u64;

    for _ in 0..14 {
        let factor = reference
            .select(factor_rank)
            .ok_or_else(|| AppError::Args("scan self-test could not select factor".to_string()))?
            * 2
            - 1;
        let expected_deleted = reference.delete_every(factor);
        let mut next_progress = Instant::now() + Duration::from_secs(86_400);
        let started = Instant::now();
        let got_deleted = if factor == 3 && scanned.alive == scanned.n_odds {
            scanned.delete_initial_every_three(
                started,
                &mut next_progress,
                Duration::from_secs(86_400),
                2,
            )
        } else {
            scanned.delete_every_by_u16_scan(
                factor,
                started,
                &mut next_progress,
                Duration::from_secs(86_400),
                2,
            )
        };

        if got_deleted != expected_deleted {
            return Err(AppError::Args(format!(
                "scan self-test failed for factor {factor}: deleted {got_deleted}, expected {expected_deleted}"
            )));
        }

        scanned.alive -= got_deleted;
        scanned.rebuild_tree_with_progress(
            started,
            &mut next_progress,
            Duration::from_secs(86_400),
        );

        if scanned.alive != reference.alive || scanned.bits != reference.bits {
            return Err(AppError::Args(format!(
                "scan self-test state mismatch after factor {factor}"
            )));
        }

        for rank in [1, factor_rank, scanned.alive / 2, scanned.alive] {
            if rank > 0 && scanned.select(rank) != reference.select(rank) {
                return Err(AppError::Args(format!(
                    "scan self-test select mismatch after factor {factor} at rank {rank}"
                )));
            }
        }

        factor_rank += 1;
    }

    Ok(())
}

fn prove_small(n: u64) -> Result<bool, AppError> {
    if n == 1 {
        return Ok(true);
    }
    if n % 2 == 0 {
        return Ok(false);
    }

    let mut candidate = Candidate::new(n, vec!["self-test".to_string()]);
    let mut sieve = LuckySieve::new(512)?;
    let mut factor_rank = 2_u64;

    loop {
        let Some(odd_index) = sieve.select(factor_rank) else {
            return Err(AppError::Args(format!(
                "self-test sieve too small to decide {n}"
            )));
        };
        let factor = odd_index * 2 - 1;
        apply_factor_to_candidates(factor, std::slice::from_mut(&mut candidate), false);
        match candidate.state {
            CandidateState::Lucky { .. } => return Ok(true),
            CandidateState::Composite { .. } => return Ok(false),
            CandidateState::Pending { .. } => {}
            CandidateState::Inconclusive { .. } => unreachable!(),
        }
        if factor <= sieve.alive {
            sieve.delete_every(factor);
        }
        factor_rank += 1;
    }
}
