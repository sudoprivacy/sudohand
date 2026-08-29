//! Text matching for `click_by_text`. Port of `core/text_match.py`.

/// Result of a text match.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchResult {
    /// The matched candidate.
    pub text: String,
    /// Score in `0.0..=1.0`.
    pub score: f64,
    /// `exact` / `contains` / `fuzzy`.
    pub strategy: &'static str,
    /// Index into the candidate list.
    pub index: usize,
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    if a.len() < b.len() {
        return levenshtein(b, a);
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut curr = Vec::with_capacity(b.len() + 1);
        curr.push(i + 1);
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr.push((curr[j] + 1).min(prev[j + 1] + 1).min(prev[j] + cost));
        }
        prev = curr;
    }
    prev[b.len()]
}

/// Score how well `query` matches `target`; returns `(score, strategy)`.
#[must_use]
pub fn match_score(query: &str, target: &str, case_sensitive: bool) -> (f64, &'static str) {
    if query.is_empty() || target.is_empty() {
        return (0.0, "none");
    }
    let (q, t) = if case_sensitive {
        (query.to_string(), target.to_string())
    } else {
        (query.to_lowercase(), target.to_lowercase())
    };
    if q == t {
        return (1.0, "exact");
    }
    let qc: Vec<char> = q.chars().collect();
    let tc: Vec<char> = t.chars().collect();
    let mut best = 0.0_f64;
    let mut strategy = "none";

    if t.contains(&q) {
        let coverage = qc.len() as f64 / tc.len() as f64;
        let base = if t.starts_with(&q) { 0.85 } else { 0.75 };
        let score = base + 0.1 * coverage;
        if score > best {
            best = score;
            strategy = "contains";
        }
    }
    if q.contains(&t) {
        let coverage = tc.len() as f64 / qc.len() as f64;
        let score = 0.7 + 0.1 * coverage;
        if score > best {
            best = score;
            strategy = "contains";
        }
    }
    let dist = levenshtein(&qc, &tc);
    let max_len = qc.len().max(tc.len());
    let edit_score = 1.0 - dist as f64 / max_len as f64;
    if edit_score > best {
        best = edit_score;
        strategy = "fuzzy";
    }
    (best, strategy)
}

/// Best candidate above `threshold`, ties broken by first occurrence.
#[must_use]
pub fn best_match<S: AsRef<str>>(
    query: &str,
    candidates: &[S],
    threshold: f64,
    case_sensitive: bool,
) -> Option<MatchResult> {
    if query.is_empty() || candidates.is_empty() {
        return None;
    }
    let mut best: Option<MatchResult> = None;
    for (i, c) in candidates.iter().enumerate() {
        let c = c.as_ref();
        if c.is_empty() {
            continue;
        }
        let (score, strategy) = match_score(query, c, case_sensitive);
        if score >= threshold && best.as_ref().is_none_or(|b| score > b.score) {
            best = Some(MatchResult {
                text: c.to_string(),
                score,
                strategy,
                index: i,
            });
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_beats_contains() {
        let names = ["Search products…", "Search"];
        let m = best_match("Search", &names, 0.4, false).unwrap();
        assert_eq!(m.index, 1);
        assert_eq!(m.strategy, "exact");
    }

    #[test]
    fn contains_scores_like_python() {
        let (s, st) = match_score("Upload", "Upload files", false);
        assert_eq!(st, "contains");
        assert!((s - (0.85 + 0.1 * 6.0 / 12.0)).abs() < 1e-9);
    }

    #[test]
    fn fuzzy_fallback() {
        let (s, st) = match_score("Submt", "Submit", false);
        assert_eq!(st, "fuzzy");
        assert!((s - (1.0 - 1.0 / 6.0)).abs() < 1e-9);
    }
}
