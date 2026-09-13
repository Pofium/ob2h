//! Ф35.1 (PLAN_v1.4): эксперимент sqlite-vec — vec0-индекс над `graph_nodes`
//! за флагом `OB2H_VEC0=1`.
//!
//! Режимы (`OB2H_VEC0_MODE`, дефолт `int8`):
//! - `int8` (§8: «rescore int8 os=2 ≈ 2.6×, recall@10 ≈ 1.0») — в vec0 лежат те
//!   же int8-векторы (Ф24), что и в `graph_nodes.embedding`; первый проход —
//!   косинус на int8 в C-ядре sqlite-vec, рескоринг — наш косинус по `k×os`
//!   кандидатам. Recall сохраняется: int8-искажение ≤ 0.01 (Ф24).
//! - `bit` — 1 бит на компоненту (48 байт/узел при 384 dim): быстрее, но
//!   Hamming-первый проход даёт низкий recall@10 на неструктурированных векторах
//!   (замер на синтетике: os=2 → 0.30, os=8 → 0.54) — годится только как
//!   «первый грубый фильтр» с большим oversample.
//!
//! Индекс — производная структура от `graph_nodes` (строится `ob2h vec0 build`),
//! флаг off / пустой индекс → поиск идёт прежним полным перебором.

use std::sync::Once;

use rusqlite::Connection;
use serde::Serialize;

use super::similarity;

/// Режим индекса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Int8,
    Bit,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Int8 => "int8",
            Mode::Bit => "bit",
        }
    }

    /// Имя vec0-таблицы (rowid = `graph_nodes.id`): режим зашит в имя, чтобы
    /// смена режима не смешивала векторы разной точности.
    pub fn table(&self) -> &'static str {
        match self {
            Mode::Int8 => "graph_nodes_vec0_int8",
            Mode::Bit => "graph_nodes_vec0_bit",
        }
    }
}

static REGISTER: Once = Once::new();

/// Регистрация расширения sqlite-vec в процессе (auto_extension — действует на
/// все соединения, открытые ПОСЛЕ вызова). Идемпотентно.
// Аннотация transmute не нужна: C-точка входа vec0 имеет другой прототип, чем
// ожидает sqlite3_auto_extension (это стандартный приём sqlite-vec для rusqlite).
#[allow(clippy::missing_transmute_annotations)]
pub fn register_once() {
    REGISTER.call_once(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
}

/// Флаг эксперимента: `OB2H_VEC0=1` (дефолт — off, поведение прежнее).
pub fn enabled() -> bool {
    std::env::var("OB2H_VEC0")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Режим индекса: `OB2H_VEC0_MODE=int8|bit` (дефолт int8 — recall-safe).
pub fn mode() -> Mode {
    match std::env::var("OB2H_VEC0_MODE").unwrap_or_default().as_str() {
        "bit" => Mode::Bit,
        _ => Mode::Int8,
    }
}

/// Oversample первого прохода: `OB2H_VEC0_OS` (дефолт 2 — ориентир §8).
pub fn oversample() -> usize {
    std::env::var("OB2H_VEC0_OS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v >= 1)
        .unwrap_or(2)
}

/// Битовый вектор из эмбеддинга: знак каждой компоненты (1 = «+»),
/// упаковка MSB-first внутри байта — формат vec0 `bit[dim]`.
pub fn bits_from_vec(vec: &[f32]) -> Vec<u8> {
    let mut bytes = vec![0u8; vec.len().div_ceil(8)];
    for (i, v) in vec.iter().enumerate() {
        if *v > 0.0 {
            bytes[i / 8] |= 1 << (7 - (i % 8));
        }
    }
    bytes
}

/// Битовый вектор из сохранённого BLOB (`int8 v2` или `f32 v1`).
pub fn bits_from_blob(blob: &[u8]) -> Option<Vec<u8>> {
    if blob.is_empty() {
        return None;
    }
    if blob.first() == Some(&similarity::Q_MAGIC) {
        // int8 v2: [magic][scale f32 LE][i8 × dim] — знак = старший бит байта
        // (scale положителен, знак i8 совпадает со знаком исходного float)
        let dim = blob.len().checked_sub(5)?;
        let mut bytes = vec![0u8; dim.div_ceil(8)];
        for (i, b) in blob[5..].iter().enumerate() {
            if (*b as i8) > 0 {
                bytes[i / 8] |= 1 << (7 - (i % 8));
            }
        }
        Some(bytes)
    } else {
        Some(bits_from_vec(&similarity::deserialize(blob)?))
    }
}

/// int8-полезная нагрузка для vec0: у v2-блоба это уже готовые i8-байты
/// (scale в vec0 не нужен — косинус инвариантен к масштабу), у legacy f32 —
/// квантуем на месте.
pub fn int8_payload_from_vec(vec: &[f32]) -> Vec<u8> {
    similarity::serialize_q(vec)[5..].to_vec()
}

pub fn int8_payload_from_blob(blob: &[u8]) -> Option<Vec<u8>> {
    if blob.is_empty() {
        return None;
    }
    if blob.first() == Some(&similarity::Q_MAGIC) {
        Some(blob[5..].to_vec())
    } else {
        Some(int8_payload_from_vec(&similarity::deserialize(blob)?))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Vec0Stats {
    /// Узлов с эмбеддингом в graph_nodes
    pub nodes_with_embedding: i64,
    /// Строк в vec0-индексе этого режима
    pub indexed: i64,
    /// Строк в индексе другого режима (если он тоже строился)
    pub indexed_other_mode: i64,
    pub dim: usize,
    pub mode: String,
    pub oversample: usize,
}

fn table_exists(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
        [table],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn count_rows(conn: &Connection, table: &str) -> rusqlite::Result<i64> {
    if !table_exists(conn, table)? {
        return Ok(0);
    }
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
}

/// Построить/догнать индекс в заданном режиме. `rebuild=true` — полная
/// перестройка. Возвращает число добавленных строк.
pub fn build_index(conn: &mut Connection, dim: usize, m: Mode, rebuild: bool) -> rusqlite::Result<usize> {
    register_once();
    let table = m.table();
    let ddl = match m {
        // distance_metric=cosine: косинус на int8 — та же метрика, что у нашего
        // рескоринга (scale-инвариантна, искажение ≤ 0.01, Ф24)
        Mode::Int8 => format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS {table} USING vec0(embedding int8[{dim}] distance_metric=cosine)"
        ),
        Mode::Bit => format!("CREATE VIRTUAL TABLE IF NOT EXISTS {table} USING vec0(embedding bit[{dim}])"),
    };
    conn.execute_batch(&ddl)?;

    let mut done: std::collections::HashSet<i64> = Default::default();
    if !rebuild {
        let mut stmt = conn.prepare(&format!("SELECT rowid FROM {table}"))?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        for r in rows {
            done.insert(r?);
        }
    }

    let tx = conn.transaction()?;
    if rebuild {
        tx.execute(&format!("DELETE FROM {table}"), [])?;
    }
    let mut added = 0usize;
    {
        let mut read = tx.prepare(
            "SELECT id, embedding FROM graph_nodes \
             WHERE embedding IS NOT NULL AND LENGTH(embedding) > 0 AND deleted_at IS NULL",
        )?;
        let sql = match m {
            Mode::Int8 => format!("INSERT INTO {table}(rowid, embedding) VALUES (?1, vec_int8(?2))"),
            Mode::Bit => format!("INSERT INTO {table}(rowid, embedding) VALUES (?1, vec_bit(?2))"),
        };
        let mut insert = tx.prepare(&sql)?;
        let rows = read.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
        for row in rows {
            let (id, blob) = row?;
            if done.contains(&id) {
                continue;
            }
            let payload = match m {
                Mode::Int8 => int8_payload_from_blob(&blob),
                Mode::Bit => bits_from_blob(&blob),
            };
            let Some(payload) = payload else { continue };
            insert.execute(rusqlite::params![id, payload])?;
            added += 1;
        }
    }
    tx.commit()?;
    Ok(added)
}

/// Статистика индексов (без построения).
pub fn stats(conn: &Connection, dim: usize) -> rusqlite::Result<Vec0Stats> {
    register_once();
    let nodes_with_embedding: i64 = conn.query_row(
        "SELECT COUNT(*) FROM graph_nodes \
         WHERE embedding IS NOT NULL AND LENGTH(embedding) > 0 AND deleted_at IS NULL",
        [],
        |r| r.get(0),
    )?;
    let m = mode();
    let other = match m {
        Mode::Int8 => Mode::Bit,
        Mode::Bit => Mode::Int8,
    };
    Ok(Vec0Stats {
        nodes_with_embedding,
        indexed: count_rows(conn, m.table())?,
        indexed_other_mode: count_rows(conn, other.table())?,
        dim,
        mode: m.as_str().to_string(),
        oversample: oversample(),
    })
}

/// KNN по битовому вектору (режим `bit`): `k` ближайших rowid по Hamming.
pub fn knn_bit(conn: &Connection, bits: &[u8], k: usize) -> rusqlite::Result<Vec<i64>> {
    knn_bit_in(conn, Mode::Bit.table(), bits, k)
}

/// KNN по битовому вектору в произвольной vec0-таблице (тесты/диагностика).
pub fn knn_bit_in(
    conn: &Connection,
    table: &str,
    bits: &[u8],
    k: usize,
) -> rusqlite::Result<Vec<i64>> {
    register_once();
    if !table_exists(conn, table)? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT rowid FROM {table} WHERE embedding MATCH vec_bit(?1) AND k = ?2"
    ))?;
    let rows = stmt.query_map(rusqlite::params![bits, k as i64], |r| r.get::<_, i64>(0))?;
    Ok(rows.flatten().collect())
}

/// KNN по int8-вектору (режим `int8`, косинус) — только rowid.
pub fn knn_int8(conn: &Connection, payload: &[u8], k: usize) -> rusqlite::Result<Vec<i64>> {
    register_once();
    let table = Mode::Int8.table();
    if !table_exists(conn, table)? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT rowid FROM {table} WHERE embedding MATCH vec_int8(?1) AND k = ?2"
    ))?;
    let rows = stmt.query_map(rusqlite::params![payload, k as i64], |r| r.get::<_, i64>(0))?;
    Ok(rows.flatten().collect())
}

/// Рескоринг кандидатов нашими векторами (косинус), топ-k по убыванию.
pub fn rescore(
    conn: &Connection,
    q_vec: &[f32],
    candidates: &[i64],
    k: usize,
) -> rusqlite::Result<Vec<(i64, f32)>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; candidates.len()].join(",");
    let sql = format!(
        "SELECT id, embedding FROM graph_nodes \
         WHERE id IN ({placeholders}) AND embedding IS NOT NULL"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::ToSql> = candidates
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();
    let rows = stmt.query_map(params.as_slice(), |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;

    let mut scored: Vec<(i64, f32)> = Vec::with_capacity(candidates.len());
    for row in rows {
        let (id, blob) = row?;
        if let Some(v) = similarity::deserialize(&blob) {
            scored.push((id, similarity::cosine(q_vec, &v)));
        }
    }
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k);
    Ok(scored)
}

/// Полный путь vec0 в заданном режиме (тесты/диагностика меряют оба режима
/// независимо от env).
pub fn search_in(
    conn: &Connection,
    m: Mode,
    q_vec: &[f32],
    k: usize,
    os: usize,
) -> rusqlite::Result<Vec<(i64, f32)>> {
    let cand = match m {
        Mode::Int8 => knn_int8(conn, &int8_payload_from_vec(q_vec), k.saturating_mul(os.max(1)))?,
        Mode::Bit => knn_bit(conn, &bits_from_vec(q_vec), k.saturating_mul(os.max(1)))?,
    };
    if cand.is_empty() {
        return Ok(Vec::new());
    }
    rescore(conn, q_vec, &cand, k)
}

/// Полный путь vec0 (режим из env): первый проход (int8-косинус или Hamming) по
/// `k×os` кандидатам → рескоринг нашими векторами. Пустой результат = индекс не
/// готов (caller уходит на полный перебор).
pub fn search(conn: &Connection, q_vec: &[f32], k: usize, os: usize) -> rusqlite::Result<Vec<(i64, f32)>> {
    search_in(conn, mode(), q_vec, k, os)
}

/// Recall@k приближённой выдачи относительно полного перебора.
pub fn recall_at_k(brute: &[i64], approx: &[i64], k: usize) -> f64 {
    let b: std::collections::HashSet<i64> = brute.iter().take(k).copied().collect();
    if b.is_empty() {
        return 1.0;
    }
    let hit = approx.iter().take(k).filter(|id| b.contains(id)).count();
    hit as f64 / b.len() as f64
}
