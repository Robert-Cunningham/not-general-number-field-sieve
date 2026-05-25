use std::collections::BTreeMap;

pub type Generator = fn(u64, u64) -> Vec<Candidate>;

#[derive(Debug, Clone)]
pub struct Candidate {
    pub n: u64,
    labels: Vec<CandidateLabel>,
    pub state: CandidateState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateLabel {
    pub shape: &'static str,
    text: String,
}

impl CandidateLabel {
    fn new(shape: &'static str, text: String) -> Self {
        Self { shape, text }
    }
}

#[derive(Debug, Clone)]
pub enum CandidateState {
    Pending { rank: u128 },
    Lucky { rank: u128, proof_factor: u64 },
    Composite { rank: u128, deletion_factor: u64 },
    Inconclusive { rank: u128 },
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
            self.labels.iter().map(|label| label.text.as_str()).collect::<Vec<_>>().join(";")
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
    fn add(&mut self, shape: &'static str, n: u128, min: u64, max: u64, text: String) {
        if n > u64::MAX as u128 || n < min as u128 || n > max as u128 {
            return;
        }

        let n = n as u64;
        if n != 1 && n % 2 == 0 {
            return;
        }

        let labels = self.labels_by_n.entry(n).or_default();
        let label = CandidateLabel::new(shape, text);
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
            out.add("repdigit", repunit * digit as u128, min, max, format!("repdigit(d={digit},len={len})"));
        }
    }
    out.into_candidates()
}

pub fn mersennes(min: u64, max: u64) -> Vec<Candidate> {
    let mut out = CandidateBuilder::default();
    for exponent in 1..=64_u32 {
        out.add("mersenne", (1_u128 << exponent) - 1, min, max, format!("mersenne(k={exponent})"));
    }
    out.into_candidates()
}

pub fn mersenne_prime_exponents(min: u64, max: u64) -> Vec<Candidate> {
    let mut out = CandidateBuilder::default();
    for exponent in 2..=64_u32 {
        if is_prime_u32(exponent) {
            out.add(
                "mersenne-prime-exp",
                (1_u128 << exponent) - 1,
                min,
                max,
                format!("mersenne-prime-exp(p={exponent})"),
            );
        }
    }
    out.into_candidates()
}

pub fn fibonacci(min: u64, max: u64) -> Vec<Candidate> {
    recurrence("fibonacci", &[0, 1], min, max)
}

pub fn lucas(min: u64, max: u64) -> Vec<Candidate> {
    recurrence("lucas", &[2, 1], min, max)
}

pub fn tetranacci(min: u64, max: u64) -> Vec<Candidate> {
    recurrence("tetranacci", &[0, 0, 0, 1], min, max)
}

fn recurrence(name: &'static str, seeds: &[u128], min: u64, max: u64) -> Vec<Candidate> {
    let mut out = CandidateBuilder::default();
    let mut window = seeds.to_vec();
    for (index, n) in window.iter().copied().enumerate() {
        out.add(name, n, min, max, format!("{name}(k={index})"));
    }

    for index in seeds.len() as u32.. {
        let Some(n) = window.iter().copied().try_fold(0_u128, u128::checked_add) else {
            break;
        };
        if n > u64::MAX as u128 {
            break;
        }
        out.add(name, n, min, max, format!("{name}(k={index})"));
        window.remove(0);
        window.push(n);
    }
    out.into_candidates()
}

pub fn consecutive_digits(min: u64, max: u64) -> Vec<Candidate> {
    let mut out = CandidateBuilder::default();
    for start in 1_u8..=9 {
        for (direction, step) in [("desc", -1_i8), ("asc", 1)] {
            let mut n = start as u128;
            let mut digit = start;
            for len in 2_u8..=20 {
                digit = ((digit as i8 + step).rem_euclid(10)) as u8;
                n = n * 10 + digit as u128;
                out.add(
                    "consecutive-digits",
                    n,
                    min,
                    max,
                    format!("consecutive-digits({direction},start={start},len={len})"),
                );
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
