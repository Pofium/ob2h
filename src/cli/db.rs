//! `ob2h db` — служебные операции с БД (Фаза 24 PLAN_v1.3).

use rusqlite::params;

use crate::mcp::AppContext;
use crate::vector::{deserialize_f32, serialize_q};

/// Таблицы с эмбеддингами: (таблица, колонка).
const EMBED_TABLES: [(&str, &str); 3] =
    [("graph_nodes", "embedding"), ("memories", "embedding"), ("chunks", "embedding")];

/// Ф24: разовое int8-квантование эмбеддингов (f32 legacy → v2).
/// Легаси определяется по кратности длины 4; v2 (magic 0x01, длина dim+5) не трогается.
pub fn run_quantize(ctx: &AppContext, dry_run: bool) -> anyhow::Result<()> {
    println!(
        "ob2h db quantize-embeddings{}",
        if dry_run { " [dry-run]" } else { "" }
    );

    let mut total_legacy = 0i64;
    for (table, col) in EMBED_TABLES {
        let (total, legacy): (i64, i64) = ctx.db.with_conn(|conn| {
            let total: i64 = conn.query_row(
                &format!("SELECT count(*) FROM {table} WHERE {col} IS NOT NULL AND length({col}) > 0"),
                [],
                |r| r.get(0),
            )?;
            let legacy: i64 = conn.query_row(
                &format!(
                    "SELECT count(*) FROM {table} WHERE {col} IS NOT NULL AND length({col}) > 0 AND length({col}) % 4 = 0"
                ),
                [],
                |r| r.get(0),
            )?;
            Ok((total, legacy))
        })?;
        println!("  {table}: векторов {total}, легаси f32 {legacy}");
        total_legacy += legacy;
    }

    if dry_run {
        println!("dry-run: изменения не применялись");
        return Ok(());
    }
    if total_legacy == 0 {
        println!("квантовать нечего: все векторы уже в формате v2 (int8)");
        return Ok(());
    }

    // Страховка перед необратимой перезаписью векторов
    let bak = ctx.backup.create()?;
    println!("пре-бэкап: {}", bak.display());

    for (table, col) in EMBED_TABLES {
        let converted = ctx.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT rowid, {col} FROM {table} WHERE {col} IS NOT NULL AND length({col}) > 0 AND length({col}) % 4 = 0"
            ))?;
            let rows: Vec<(i64, Vec<u8>)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .flatten()
                .collect();
            let mut n = 0;
            for (rowid, blob) in rows {
                if let Some(vec) = deserialize_f32(&blob) {
                    let q = serialize_q(&vec);
                    conn.execute(
                        &format!("UPDATE {table} SET {col} = ?1 WHERE rowid = ?2"),
                        params![q, rowid],
                    )?;
                    n += 1;
                }
            }
            Ok(n)
        })?;
        println!("  {table}: квантовано {converted}");
    }

    ctx.db.with_conn(|conn| conn.execute_batch("VACUUM;"))?;
    let size = std::fs::metadata(ctx.settings.db_path()).map(|m| m.len()).unwrap_or(0);
    println!("VACUUM выполнен; размер БД: {:.1} МБ", size as f64 / 1024.0 / 1024.0);
    Ok(())
}
