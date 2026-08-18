use ob2h::vector::{cosine, deserialize, normalize, rrf_merge, serialize, top_k};

#[test]
fn test_vector_serialization_and_similarity() {
    let original = vec![1.0f32, -2.5, 0.0, 4.2];
    let blob = serialize(&original);
    assert_eq!(blob.len(), 16);

    let restored = deserialize(&blob).expect("deserialization failed");
    assert_eq!(original, restored);

    let normalized = normalize(&original);
    let norm: f32 = normalized.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-5);

    let v1 = normalize(&[1.0, 0.0, 0.0]);
    let v2 = normalize(&[1.0, 0.0, 0.0]);
    let v3 = normalize(&[0.0, 1.0, 0.0]);

    assert!((cosine(&v1, &v2) - 1.0).abs() < 1e-5);
    assert!((cosine(&v1, &v3) - 0.0).abs() < 1e-5);
}

#[test]
fn test_top_k_and_rrf_merge() {
    let q = normalize(&[1.0, 0.0, 0.0]);
    let c1 = serialize(&normalize(&[1.0, 0.0, 0.0]));
    let c2 = serialize(&normalize(&[0.8, 0.6, 0.0]));
    let c3 = serialize(&normalize(&[0.0, 1.0, 0.0]));

    let candidates = vec![
        (1, Some(c1.as_slice())),
        (2, Some(c2.as_slice())),
        (3, Some(c3.as_slice())),
    ];

    let scored = top_k(&q, &candidates, 2, 0.1);
    assert_eq!(scored.len(), 2);
    assert_eq!(scored[0].0, 1);
    assert_eq!(scored[1].0, 2);

    let fts = vec![2, 1, 3];
    let vec_ids = vec![1, 2, 4];
    let merged = rrf_merge(&fts, &vec_ids, 60.0);

    assert_eq!(merged.len(), 4);
    assert!(merged[0].id == 1 || merged[0].id == 2);
}
