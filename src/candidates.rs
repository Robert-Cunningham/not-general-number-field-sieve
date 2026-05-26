use std::{collections::BTreeMap, fmt};

pub type Generator = fn(u64, u64) -> Vec<Candidate>;

#[derive(Debug, Clone)]
pub struct Candidate {
    pub n: u64,
    labels: Vec<CandidateLabel>,
    pub state: CandidateState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateLabel {
    Repdigit { digit: u8, len: u8 },
    Mersenne { exponent: u32 },
    MersennePrimeExponent { exponent: u32 },
    Fibonacci { index: u32 },
    Lucas { index: u32 },
    Tetranacci { index: u32 },
    ConsecutiveDigits { direction: Direction, start: u8, len: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Asc,
    Desc,
}

impl CandidateLabel {
    pub fn shape(&self) -> &'static str {
        match self {
            Self::Repdigit { .. } => "repdigit",
            Self::Mersenne { .. } => "mersenne",
            Self::MersennePrimeExponent { .. } => "mersenne-prime-exp",
            Self::Fibonacci { .. } => "fibonacci",
            Self::Lucas { .. } => "lucas",
            Self::Tetranacci { .. } => "tetranacci",
            Self::ConsecutiveDigits { .. } => "consecutive-digits",
        }
    }
}

impl fmt::Display for CandidateLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Repdigit { digit, len } => write!(f, "repdigit(d={digit},len={len})"),
            Self::Mersenne { exponent } => write!(f, "mersenne(k={exponent})"),
            Self::MersennePrimeExponent { exponent } => write!(f, "mersenne-prime-exp(p={exponent})"),
            Self::Fibonacci { index } => write!(f, "fibonacci(k={index})"),
            Self::Lucas { index } => write!(f, "lucas(k={index})"),
            Self::Tetranacci { index } => write!(f, "tetranacci(k={index})"),
            Self::ConsecutiveDigits { direction, start, len } => {
                write!(f, "consecutive-digits({},start={start},len={len})", direction.text())
            }
        }
    }
}

impl Direction {
    fn text(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}

#[derive(Debug, Clone)]
pub enum CandidateState {
    Pending { rank: u128 },
    Lucky { rank: u128, proof_factor: u64 },
    Composite { rank: u128, deletion_factor: u64 },
    Inconclusive,
}

impl Candidate {
    fn new(n: u64, labels: Vec<CandidateLabel>) -> Self {
        let state = if n == 1 {
            CandidateState::Lucky { rank: 1, proof_factor: 0 }
        } else {
            CandidateState::Pending { rank: ((n as u128) + 1) / 2 }
        };

        Self { n, labels, state }
    }

    #[cfg(test)]
    pub fn new_unlabeled(n: u64) -> Self {
        Self::new(n, Vec::new())
    }

    pub fn is_pending(&self) -> bool {
        matches!(self.state, CandidateState::Pending { .. })
    }

    pub fn labels_text(&self) -> String {
        if self.labels.is_empty() {
            "unlabeled".to_string()
        } else {
            self.labels.iter().map(ToString::to_string).collect::<Vec<_>>().join(";")
        }
    }

    pub fn labels(&self) -> &[CandidateLabel] {
        &self.labels
    }
}

#[derive(Default)]
struct CandidateBuilder {
    labels_by_n: BTreeMap<u64, Vec<CandidateLabel>>,
}

impl CandidateBuilder {
    fn add(&mut self, label: CandidateLabel, n: u128, min: u64, max: u64) {
        if n > u64::MAX as u128 || n < min as u128 || n > max as u128 {
            return;
        }

        let n = n as u64;
        if n != 1 && n % 2 == 0 {
            return;
        }

        let labels = self.labels_by_n.entry(n).or_default();
        if !labels.contains(&label) {
            labels.push(label);
        }
    }

    fn into_candidates(self) -> Vec<Candidate> {
        self.labels_by_n.into_iter().map(|(n, labels)| Candidate::new(n, labels)).collect()
    }

    fn extend(&mut self, candidates: Vec<Candidate>) {
        for candidate in candidates {
            let labels = self.labels_by_n.entry(candidate.n).or_default();
            for label in candidate.labels {
                if !labels.contains(&label) {
                    labels.push(label);
                }
            }
        }
    }
}

pub fn generate_candidates(min: u64, max: u64, generators: &[Generator]) -> Vec<Candidate> {
    let mut out = CandidateBuilder::default();
    for generate in generators {
        out.extend(generate(min, max));
    }
    out.into_candidates()
}

pub fn repdigits(min: u64, max: u64) -> Vec<Candidate> {
    let mut out = CandidateBuilder::default();
    let mut repunit = 0_u128;
    for len in 1..=20_u8 {
        repunit = repunit * 10 + 1;
        for digit in [1_u8, 3, 5, 7, 9] {
            out.add(CandidateLabel::Repdigit { digit, len }, repunit * digit as u128, min, max);
        }
    }
    out.into_candidates()
}

pub fn mersennes(min: u64, max: u64) -> Vec<Candidate> {
    let mut out = CandidateBuilder::default();
    for exponent in 1..=64_u32 {
        out.add(CandidateLabel::Mersenne { exponent }, (1_u128 << exponent) - 1, min, max);
    }
    out.into_candidates()
}

pub fn mersenne_prime_exponents(min: u64, max: u64) -> Vec<Candidate> {
    let mut out = CandidateBuilder::default();
    for exponent in 2..=64_u32 {
        if is_prime_u32(exponent) {
            out.add(CandidateLabel::MersennePrimeExponent { exponent }, (1_u128 << exponent) - 1, min, max);
        }
    }
    out.into_candidates()
}

pub fn fibonacci(min: u64, max: u64) -> Vec<Candidate> {
    recurrence(|index| CandidateLabel::Fibonacci { index }, &[0, 1], min, max)
}

pub fn lucas(min: u64, max: u64) -> Vec<Candidate> {
    recurrence(|index| CandidateLabel::Lucas { index }, &[2, 1], min, max)
}

pub fn tetranacci(min: u64, max: u64) -> Vec<Candidate> {
    recurrence(|index| CandidateLabel::Tetranacci { index }, &[0, 0, 0, 1], min, max)
}

fn recurrence(label: fn(u32) -> CandidateLabel, seeds: &[u128], min: u64, max: u64) -> Vec<Candidate> {
    let mut out = CandidateBuilder::default();
    let mut window = seeds.to_vec();
    for (index, n) in window.iter().copied().enumerate() {
        out.add(label(index as u32), n, min, max);
    }

    for index in seeds.len() as u32.. {
        let Some(n) = window.iter().copied().try_fold(0_u128, u128::checked_add) else {
            break;
        };
        if n > u64::MAX as u128 {
            break;
        }
        out.add(label(index), n, min, max);
        window.remove(0);
        window.push(n);
    }
    out.into_candidates()
}

pub fn consecutive_digits(min: u64, max: u64) -> Vec<Candidate> {
    let mut out = CandidateBuilder::default();
    for start in 1_u8..=9 {
        for (direction, step) in [(Direction::Desc, -1_i8), (Direction::Asc, 1)] {
            let mut n = start as u128;
            let mut digit = start;
            for len in 2_u8..=20 {
                digit = ((digit as i8 + step).rem_euclid(10)) as u8;
                n = n * 10 + digit as u128;
                out.add(CandidateLabel::ConsecutiveDigits { direction, start, len }, n, min, max);
            }
        }
    }
    out.into_candidates()
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

    let mut divisor = 3;
    while divisor * divisor <= n {
        if n % divisor == 0 {
            return false;
        }
        divisor += 2;
    }
    true
}
