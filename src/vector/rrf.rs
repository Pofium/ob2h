//! Слияние результатов ранжирования Reciprocal Rank Fusion (RRF k=60).

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct RankedItem {
    pub id: i64,
    pub fts_rank: Option<usize>,
    pub vector_rank: Option<usize>,
    pub rrf_score: f64,
}

/// Слияние ранжированных списков FTS и Vector поиска через RRF (k=60).
pub fn rrf_merge(
    fts_ids: &[i64],
    vector_ids: &[i64],
    k: f64,
) -> Vec<RankedItem> {
    let mut scores: HashMap<i64, (Option<usize>, Option<usize>, f64)> = HashMap::new();

    for (rank, &id) in fts_ids.iter().enumerate() {
        let entry = scores.entry(id).or_insert((None, None, 0.0));
        entry.0 = Some(rank + 1);
        entry.2 += 1.0 / (k + (rank + 1) as f64);
    }

    for (rank, &id) in vector_ids.iter().enumerate() {
        let entry = scores.entry(id).or_insert((None, None, 0.0));
        entry.1 = Some(rank + 1);
        entry.2 += 1.0 / (k + (rank + 1) as f64);
    }

    let mut result: Vec<RankedItem> = scores
        .into_iter()
        .map(|(id, (fts_rank, vector_rank, rrf_score))| RankedItem {
            id,
            fts_rank,
            vector_rank,
            rrf_score,
        })
        .collect();

    result.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rrf_merge() {
        let fts = vec![1, 2, 3];
        let vec = vec![2, 1, 4];
        let merged = rrf_merge(&fts, &vec, 60.0);

        assert_eq!(merged.len(), 4);
        // ID 1 и 2 должны быть в топе, так как присутствуют в обоих списках
        assert!(merged[0].id == 1 || merged[0].id == 2);
        assert!(merged[1].id == 1 || merged[1].id == 2);
    }
}
