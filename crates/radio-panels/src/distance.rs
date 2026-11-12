use crate::{Block, Kind, ProtocolError, match_key, sum::py_sum};
use std::collections::{BTreeMap, BTreeSet};
/// Symmetric distances indexed by member name.
pub type Matrix = BTreeMap<String, BTreeMap<String, f64>>;
/// Measure item overlap or reciprocal text ratings.
pub fn pair_distance(
    a: Option<&Block>,
    b: Option<&Block>,
    kind: Kind,
    n: Option<i64>,
    name_a: &str,
    name_b: &str,
) -> Result<f64, ProtocolError> {
    if kind == Kind::List {
        let (Some(a), Some(b)) = (a, b) else {
            return Ok(1.0);
        };
        let n = n.filter(|n| *n > 0).ok_or(ProtocolError::ListNeedsN)?;
        let a: BTreeSet<_> = a.items.iter().map(|s| match_key(s)).collect();
        let b: BTreeSet<_> = b.items.iter().map(|s| match_key(s)).collect();
        return Ok((n as f64 - a.intersection(&b).count() as f64) / n as f64);
    }
    let forward = a.and_then(|b| b.ratings.get(name_b)).copied().unwrap_or(0) as f64;
    let reverse = b.and_then(|b| b.ratings.get(name_a)).copied().unwrap_or(0) as f64;
    Ok((20.0 - forward - reverse) / 20.0)
}
/// Compute weighted own and group means, falling back to plain means at zero weight.
pub fn distances(
    names: &[String],
    matrix: &Matrix,
    weights: &BTreeMap<String, f64>,
) -> Result<(BTreeMap<String, f64>, f64), ProtocolError> {
    if names.len() < 2 {
        return Err(ProtocolError::InvalidState(
            "distance needs at least two members",
        ));
    }
    let weight = |name: &str| {
        weights
            .get(name)
            .copied()
            .ok_or(ProtocolError::InvalidState("missing member weight"))
    };
    let distance = |a: &str, b: &str| {
        matrix
            .get(a)
            .and_then(|row| row.get(b))
            .copied()
            .ok_or(ProtocolError::InvalidState("missing pair distance"))
    };
    let mut own = BTreeMap::new();
    let mut pairs = Vec::new();
    // All sums follow the supplied member order, independent of map ordering.
    for (i, name) in names.iter().enumerate() {
        let mut others = Vec::new();
        for other in names.iter().filter(|other| *other != name) {
            let d = distance(name, other)?;
            let w = weight(other)?;
            others.push((w, d));
        }
        let denominator = py_sum(others.iter().map(|(w, _)| *w));
        own.insert(
            name.clone(),
            if denominator != 0.0 {
                py_sum(others.iter().map(|(w, d)| w * d)) / denominator
            } else {
                py_sum(others.iter().map(|(_, d)| *d)) / (names.len() - 1) as f64
            },
        );
        for other in names.iter().skip(i + 1) {
            let w = weight(name)? * weight(other)?;
            let d = distance(name, other)?;
            pairs.push((w, d));
        }
    }
    let pair_weight = py_sum(pairs.iter().map(|(w, _)| *w));
    Ok((
        own,
        if pair_weight != 0.0 {
            py_sum(pairs.iter().map(|(w, d)| w * d)) / pair_weight
        } else {
            py_sum(pairs.iter().map(|(_, d)| *d)) / pairs.len() as f64
        },
    ))
}
/// Build a symmetric matrix in the caller's member order.
pub fn distance_matrix(
    kind: Kind,
    n: Option<i64>,
    crew: &[String],
    blocks: &BTreeMap<String, Option<Block>>,
) -> Result<Matrix, ProtocolError> {
    let mut matrix: Matrix = crew.iter().map(|s| (s.clone(), BTreeMap::new())).collect();
    for (i, a) in crew.iter().enumerate() {
        for b in crew.iter().skip(i + 1) {
            let d = pair_distance(
                blocks.get(a).and_then(Option::as_ref),
                blocks.get(b).and_then(Option::as_ref),
                kind,
                n,
                a,
                b,
            )?;
            matrix.entry(a.clone()).or_default().insert(b.clone(), d);
            matrix.entry(b.clone()).or_default().insert(a.clone(), d);
        }
    }
    Ok(matrix)
}
