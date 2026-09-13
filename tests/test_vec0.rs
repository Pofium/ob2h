//! Ф35.1 (PLAN_v1.4): эксперимент sqlite-vec — vec0-индекс над graph_nodes.
//! Критерии: recall@10 ≥ 0.99 против полного перебора на синтетике 10K;
//! флаг off / пустой индекс — прежнее поведение (полный перебор).

use std::sync::Arc;
use std::sync::Mutex;

use ob2h::db::Database;
use ob2h::embedding::{EmbeddingProvider, FakeEmbedding};
use ob2h::graph::GraphService;
use ob2h::vector::similarity;
use ob2h::vector::vec0;
use rusqlite::params;

/// env-переменные процесса глобальны — env-зависимые тесты сериализуем.
static ENV_LOCK: Mutex<()> = Mutex::new(());

const DIM: usize = 64;

/// Детерминированный ГПСЧ (xorshift64) — фикстур без внешних зависимостей.
struct Rng(u64);

impl Rng {
    fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) as f32
    }

    fn vec(&mut self) -> Vec<f32> {
        let v: Vec<f32> = (0..DIM).map(|_| self.next_f32() * 2.0 - 1.0).collect();
        similarity::normalize(&v)
    }
}

fn seed_nodes(db: &Database, n: usize) -> Vec<Vec<f32>> {
    let mut rng = Rng(0x5eed_1234_5678_9abc);
    let mut vecs = Vec::with_capacity(n);
    db.with_conn(|conn| {
        let tx = conn.transaction()?;
        for i in 0..n {
            let v = rng.vec();
            let blob = similarity::serialize_q(&v);
            tx.execute(
                "INSERT INTO graph_nodes (node_id, label, node_type, description, val, embedding, created_at, updated_at) \
                 VALUES (?1, ?2, 'entity', 'синтетика', 1, ?3, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                params![format!("n-{i}"), format!("Узел {i}"), blob],
            )?;
            vecs.push(v);
        }
        tx.commit()?;
        Ok(())
    })
    .expect("seed");
    vecs
}

fn brute_top_k(db: &Database, q: &[f32], k: usize) -> Vec<i64> {
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, embedding FROM graph_nodes WHERE embedding IS NOT NULL AND deleted_at IS NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        let mut list = Vec::new();
        for r in rows.flatten() {
            list.push(r);
        }
        let refs: Vec<(i64, Option<&[u8]>)> =
            list.iter().map(|(id, b)| (*id, Some(b.as_slice()))).collect();
        Ok(ob2h::vector::top_k(q, &refs, k, 0.0)
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<i64>>())
    })
    .expect("brute")
}

/// 35.1: на синтетике 10K recall@10 — int8-режим (дефолт) держит критерий ≥ 0.99
/// при os=2; bit-режим замеряется для честной фиксации (Hamming ≈ шум на
/// неструктурированных векторах).
#[test]
fn vec0_recall_at_10_on_synthetic_10k() {
    let db = Database::in_memory().expect("db");
    let _vecs = seed_nodes(&db, 10_000);

    let dim = DIM;
    let mut results: Vec<(String, usize, f64)> = Vec::new();
    for m in [vec0::Mode::Int8, vec0::Mode::Bit] {
        let added = db
            .with_conn(|conn| vec0::build_index(conn, dim, m, true))
            .expect("build");
        assert_eq!(added, 10_000, "проиндексированы все узлы ({:?})", m);
        let st = db.with_conn(|conn| vec0::stats(conn, dim)).expect("stats");
        assert_eq!(st.indexed, 10_000, "индекс полон ({:?})", m);

        let mut rng = Rng(0xabcd_ef01_2345_6789);
        let queries = 40;
        let k = 10;
        for os in [2usize, 4, 8] {
            let mut sum = 0.0;
            for _ in 0..queries {
                let q = rng.vec();
                let brute = brute_top_k(&db, &q, k);
                let approx: Vec<i64> = db
                    .with_conn(|conn| {
                        let _ = m;
                        vec0::search_in(conn, m, &q, k, os)
                    })
                    .expect("vec0 search")
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect();
                assert_eq!(approx.len(), k, "vec0 вернул k результатов");
                sum += vec0::recall_at_k(&brute, &approx, k);
            }
            let r = sum / queries as f64;
            println!("{:>4} os={os}: recall@{k} = {r:.4}", m.as_str());
            results.push((m.as_str().to_string(), os, r));
        }
    }

    let get = |mode: &str, os: usize| {
        results
            .iter()
            .find(|(m, o, _)| m == mode && *o == os)
            .map(|(_, _, r)| *r)
            .unwrap()
    };
    // критерий приёмки §8 / 35.1: recall@10 ≥ 0.99 при os=2 в дефолтном (int8) режиме
    assert!(
        get("int8", 2) >= 0.99,
        "int8 os=2 recall@10 = {:.4}, критерий 0.99",
        get("int8", 2)
    );
    // bit-режим принципиально грубее: фиксируем, что oversample улучшает recall
    assert!(
        get("bit", 8) >= get("bit", 2),
        "bit recall растёт с oversample"
    );
}

/// 35.1: vec0-индекс ускоряет векторный скан (на 10K — заметно дешевле перебора).
#[test]
fn vec0_scan_is_faster_than_brute_force() {
    let db = Database::in_memory().expect("db");
    let vecs = seed_nodes(&db, 10_000);
    db.with_conn(|conn| vec0::build_index(conn, DIM, vec0::Mode::Int8, false))
        .expect("build");

    let q = &vecs[7];
    // прогрев
    let _ = brute_top_k(&db, q, 10);
    let _ = db
        .with_conn(|conn| vec0::search(conn, q, 10, 2))
        .expect("vec0");

    let t = std::time::Instant::now();
    for _ in 0..20 {
        let _ = brute_top_k(&db, q, 10);
    }
    let brute_ms = t.elapsed().as_secs_f64() * 1000.0 / 20.0;

    let t = std::time::Instant::now();
    for _ in 0..20 {
        let _ = db
            .with_conn(|conn| vec0::search(conn, q, 10, 4))
            .expect("vec0");
    }
    let vec0_ms = t.elapsed().as_secs_f64() * 1000.0 / 20.0;
    println!("10K: перебор {brute_ms:.2} мс, vec0 {vec0_ms:.2} мс");
    assert!(
        vec0_ms < brute_ms,
        "vec0 ({vec0_ms:.2} мс) должен быть быстрее перебора ({brute_ms:.2} мс)"
    );
}

/// 35.1: индекс не готов (флаг off или пустой индекс) → поиск графа идёт
/// прежним полным перебором и даёт те же результаты (регрессии нет).
// MutexGuard держится через await намеренно: сериализует env-переменную
// против параллельных тестов, читающих OB2H_VEC0.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn empty_index_falls_back_to_brute_force() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("OB2H_VEC0");

    let db = Database::in_memory().expect("db");
    let _ = seed_nodes(&db, 200);
    let embedder: Arc<FakeEmbedding> = Arc::new(FakeEmbedding::new(DIM));
    let graph = GraphService::new(db.clone(), embedder as Arc<dyn EmbeddingProvider>);

    let without_flag = graph.search("Узел 1", 5, false).await.expect("search off");
    let ids_off: Vec<i64> = without_flag.nodes.iter().map(|n| n.id).collect();
    assert!(!ids_off.is_empty(), "поиск что-то находит");

    // флаг on, но индекса нет → тоже перебор, результат тот же
    std::env::set_var("OB2H_VEC0", "1");
    let st = db.with_conn(|conn| vec0::stats(conn, DIM)).expect("stats");
    assert_eq!(st.indexed, 0, "индекс пуст");
    let with_flag_empty = graph.search("Узел 1", 5, false).await.expect("search on");
    let mut ids_on: Vec<i64> = with_flag_empty.nodes.iter().map(|n| n.id).collect();
    std::env::remove_var("OB2H_VEC0");

    // порядок при равных скорах в GraphService::search недетерминирован (HashMap),
    // поэтому сверяем множества — набор результатов обязан совпасть
    let mut ids_off = ids_off;
    ids_off.sort_unstable();
    ids_on.sort_unstable();
    assert_eq!(ids_off, ids_on, "без индекса результаты совпадают с флагом off");
}

/// 35.1: битовая упаковка MSB-first и Hamming-KNN по vec0.
#[test]
fn bit_packing_and_hamming_order() {
    let v = [1.0f32, -1.0, 1.0, 1.0, -1.0, -1.0, 1.0, -1.0, 1.0];
    let bits = vec0::bits_from_vec(&v);
    assert_eq!(bits.len(), 2, "9 компонент → 2 байта");
    // MSB-first: +,-,+,+,+,-,-,+ → 1011 0010 ; 9-я (+1) → 1000 0000
    assert_eq!(bits[0], 0b1011_0010);
    assert_eq!(bits[1], 0b1000_0000);

    // из int8-блоба знаки читаются без материализации float'ов
    let q = similarity::serialize_q(&v);
    assert_eq!(vec0::bits_from_blob(&q).expect("bits"), bits);

    // Hamming-порядок через vec0
    let db = Database::in_memory().expect("db");
    db.with_conn(|conn| {
        conn.execute_batch("CREATE VIRTUAL TABLE t USING vec0(embedding bit[16])")?;
        for (rowid, bits) in [
            (1i64, vec![0b1000_0000u8, 0]),
            (2, vec![0b1100_0000u8, 0]),
            (3, vec![0b1111_0000u8, 0]),
        ] {
            conn.execute(
                "INSERT INTO t(rowid, embedding) VALUES (?1, vec_bit(?2))",
                params![rowid, bits],
            )?;
        }
        let hits = vec0::knn_bit_in(conn, "t", &[0b1000_0000, 0], 3)?;
        assert_eq!(hits, vec![1, 2, 3], "по возрастанию Hamming");
        Ok(())
    })
    .expect("hamming");
}

/// 35.1: recall_at_k считает пересечение по префиксу k.
#[test]
fn recall_metric_basics() {
    assert_eq!(vec0::recall_at_k(&[1, 2, 3], &[1, 2, 3], 3), 1.0);
    assert!((vec0::recall_at_k(&[1, 2, 3, 4], &[1, 2, 9, 4], 4) - 0.75).abs() < 1e-9);
    assert_eq!(vec0::recall_at_k(&[], &[1], 1), 1.0);
}
