//! Ф33 (PLAN_v1.4): Personalized PageRank по памяти — dual-seed, веса по типу ребра,
//! hub-защита, `memory_search mode=graph` (33.3) и `graph_reason scope=memory` (33.2).
//! Без сети: FakeEmbedding, in-memory БД (движок PPR покрыт юнит-тестами в
//! `src/graph/pagerank.rs` — здесь проверяется связка с памятью и контракт MCP).

use std::sync::Arc;

use ob2h::config::Settings;
use ob2h::db::Database;
use ob2h::embedding::FakeEmbedding;
use ob2h::graph::pagerank::{parse_ppr_weights, weight_for, DEFAULT_PPR_WEIGHTS};
use ob2h::init_app;
use ob2h::mcp::McpServer;
use ob2h::memory::MemoryService;
use rusqlite::params;
use serde_json::json;
use tempfile::tempdir;

fn setup_memory() -> (tempfile::TempDir, MemoryService, Database) {
    let tmp = tempdir().expect("tempdir");
    let db = Database::in_memory().expect("db");
    let memory = MemoryService::new(db.clone(), Arc::new(FakeEmbedding::new(384)));
    (tmp, memory, db)
}

async fn save(memory: &MemoryService, key: &str, content: &str, importance: f64) {
    save_cat(memory, key, content, importance, "notes").await;
}

/// Как `save`, но с явной категорией. Категория важна: `link_after_save` (Ф23.5)
/// автоматически связывает записи одной категории, поэтому для чистых многошаговых
/// сценариев категории разводят по разным значениям.
async fn save_cat(
    memory: &MemoryService,
    key: &str,
    content: &str,
    importance: f64,
    category: &str,
) {
    memory
        .save(content, Some(key), category, importance, "chat", None)
        .await
        .expect("save");
}

fn link(db: &Database, from_key: &str, to_key: &str, kind: &str) {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT OR IGNORE INTO memory_links (from_id, to_id, kind, weight, created_at) \
             VALUES ((SELECT id FROM memories WHERE key=?1), \
                     (SELECT id FROM memories WHERE key=?2), ?3, 1.0, '2026-01-01T00:00:00+00:00')",
            params![from_key, to_key, kind],
        )?;
        Ok(())
    })
    .expect("link");
}

fn id_of(memory: &MemoryService, key: &str) -> i64 {
    memory.get(key).expect("get").expect("record").id
}

/// 29.4/33.3: синтетическая цепочка A→B→C по `memory_links`. Запрос по A достаёт C
/// через PPR (multi-hop), тогда как 1-hop-расширение C не видит.
#[tokio::test]
async fn ppr_reaches_third_hop_where_1hop_does_not() {
    let (_tmp, memory, db) = setup_memory();
    // категории разные — иначе автосвязи Ф23.5 дадут 1-hop-путь A→C в обход цепочки
    save_cat(&memory, "p-a", "Альфа: начало цепочки", 0.5, "chain-a").await;
    save_cat(&memory, "p-b", "Бета: середина цепочки", 0.4, "chain-b").await;
    save_cat(&memory, "p-c", "Гамма: конец цепочки", 0.3, "chain-c").await;
    link(&db, "p-a", "p-b", "manual");
    link(&db, "p-b", "p-c", "manual");

    let a_id = id_of(&memory, "p-a");

    // 1-hop: из A достижим только B
    let one_hop: Vec<String> = memory
        .related_records(&[a_id], 10)
        .expect("related")
        .into_iter()
        .map(|r| r.key)
        .collect();
    assert!(one_hop.contains(&"p-b".to_string()), "1-hop: {one_hop:?}");
    assert!(
        !one_hop.contains(&"p-c".to_string()),
        "1-hop не должен достигать третьего узла: {one_hop:?}"
    );

    // PPR: из A достижим C, и ближний B ранжируется выше дальнего C
    let w = parse_ppr_weights(DEFAULT_PPR_WEIGHTS);
    let expanded = memory
        .ppr_expand_records(&[(a_id, 1.0)], 10, &w, 0.85, 500, None)
        .expect("ppr");
    let keys: Vec<String> = expanded.iter().map(|(r, _)| r.key.clone()).collect();
    assert!(
        keys.contains(&"p-c".to_string()),
        "PPR обязан достигать C: {keys:?}"
    );
    let pos_b = keys.iter().position(|k| k == "p-b").expect("B в выдаче");
    let pos_c = keys.iter().position(|k| k == "p-c").expect("C в выдаче");
    assert!(pos_b < pos_c, "ближний узел выше дальнего: {keys:?}");
}

/// 33.1: hub-защита — запись со 100+ рёбрами не «залипает» выдачу: прямой сосед
/// сида ранжируется выше любого листа хаба (degree-normalization из 29.1).
#[tokio::test]
async fn hub_does_not_flood_expansion() {
    let (_tmp, memory, db) = setup_memory();
    save(&memory, "h-seed", "Зерно: запрос пользователя", 0.6).await;
    save(&memory, "h-direct", "Прямой сосед зерна", 0.5).await;
    save(&memory, "h-hub", "Хаб: запись со множеством связей", 0.4).await;
    for i in 0..120 {
        save(
            &memory,
            &format!("h-leaf-{i}"),
            &format!("Лист хаба номер {i}"),
            0.2,
        )
        .await;
    }
    link(&db, "h-seed", "h-direct", "manual");
    link(&db, "h-seed", "h-hub", "manual");
    for i in 0..120 {
        link(&db, "h-hub", &format!("h-leaf-{i}"), "same_project");
    }

    let seed_id = id_of(&memory, "h-seed");
    let w = parse_ppr_weights(DEFAULT_PPR_WEIGHTS);
    let ranked = memory
        .ppr_rank(&[(seed_id, 1.0)], &w, 0.85, 500, None)
        .expect("ppr");
    let pos = |key: &str| {
        let id = id_of(&memory, key);
        ranked.iter().position(|(node, _)| *node == id)
    };

    let direct = pos("h-direct").expect("прямой сосед в выдаче");
    let best_leaf = (0..120)
        .filter_map(|i| pos(&format!("h-leaf-{i}")))
        .min()
        .expect("лист в выдаче");
    assert!(
        direct < best_leaf,
        "прямой сосед обязан ранжироваться выше любого листа хаба (direct={direct}, leaf={best_leaf})"
    );
}

/// 33.1/33.4: dual-seed — запись, чей нормализованный ключ пересекается с токенами
/// запроса, попадает в сиды (entity-фраза) и получает PPR-массу, даже не имея связей;
/// нерелевантная изолированная запись — нет.
#[tokio::test]
async fn entity_phrase_seeds_reach_unlinked_records() {
    let (_tmp, memory, _db) = setup_memory();
    save(&memory, "e-seed", "Опорная запись по проекту", 0.5).await;
    save(&memory, "пресняков работы", "Заметка про работу", 0.4).await;
    save(&memory, "погода москва", "Прогноз погоды", 0.4).await;

    let seed_id = id_of(&memory, "e-seed");
    let w = parse_ppr_weights(DEFAULT_PPR_WEIGHTS);
    let ranked = memory
        .ppr_rank(&[(seed_id, 1.0)], &w, 0.85, 500, Some("пресняков задачи"))
        .expect("ppr");
    let ids: Vec<i64> = ranked.iter().map(|(id, _)| *id).collect();

    assert!(
        ids.contains(&id_of(&memory, "пресняков работы")),
        "entity-фраза из запроса обязана сидироваться (33.1)"
    );
    assert!(
        !ids.contains(&id_of(&memory, "погода москва")),
        "нерелевантная изолированная запись не должна получать массу"
    );
}

/// 33.1: веса по типу ребра берутся из настроек и зафиксированы дефолтом
/// (`OB2H_PPR_WEIGHTS`); неизвестный kind → 0.5.
#[test]
fn default_weights_and_limits_are_fixed() {
    let s = Settings::from_env();
    assert_eq!(s.ppr_weights.len(), 5, "{:?}", s.ppr_weights);
    assert!((s.ppr_weights["manual"] - 1.0).abs() < 1e-12);
    assert!((s.ppr_weights["contradicts"] - 0.8).abs() < 1e-12);
    assert!((s.ppr_weights["causes"] - 0.8).abs() < 1e-12);
    assert!((s.ppr_weights["category"] - 0.5).abs() < 1e-12);
    assert!((s.ppr_weights["same_project"] - 0.3).abs() < 1e-12);
    // неподконтрольный тип ребра не обнуляет связь — вес 0.5
    let parsed = parse_ppr_weights(r#"{"weird":0.7}"#);
    assert!((weight_for(&parsed, "unknown") - 0.5).abs() < 1e-12);
    assert!((s.ppr_damping - 0.85).abs() < 1e-12);
    assert_eq!(s.graph_reason_memory_max_nodes, 500);
    assert_eq!(s.graph_reason_memory_timeout_ms, 1000);
}

/// 33.2/33.3: контракт MCP. Вызовы без `scope` не изменились (документный граф,
/// регресс контракта), `scope=memory` отвечает по PPR-подграфу памяти,
/// `memory_search mode=graph` отдаёт блок `[ppr]` с multi-hop записью.
#[tokio::test]
async fn mcp_scope_and_graph_mode_contract() {
    let tmp = tempdir().expect("tempdir");
    let mut settings = Settings::from_env();
    settings.data_dir = tmp.path().to_path_buf();
    settings.embed_provider = "fake".to_string();
    let ctx = init_app(settings).expect("init app");
    let db = ctx.db.clone();
    let server = McpServer::new(ctx);

    // (1) регресс: без scope — прежний ответ по графу знаний (он пуст)
    let plain = server
        .call_tool("graph_reason", json!({"query": "что известно про альфа"}))
        .await;
    assert!(
        plain.starts_with("answer: В графе нет данных по запросу."),
        "контракт без scope изменился: {plain}"
    );
    assert!(
        !plain.contains("scope:"),
        "без scope память не подмешивается: {plain}"
    );

    // (2) наполняем память: три записи, у каждой своя категория (чтобы автосвязи
    // Ф23.5 не добавляли рёбер) + ручная цепочка hit → n1 → n2
    for (key, content, imp, cat) in [
        ("q-hit", "Альфа: старт работ по проекту", 0.6, "cat-hit"),
        ("n1", "Промежуточная заметка про бюджет", 0.5, "cat-n1"),
        ("n2", "Конечная заметка про сроки", 0.4, "cat-n2"),
    ] {
        let saved = server
            .call_tool(
                "memory_save",
                json!({"key": key, "content": content, "category": cat, "importance": imp}),
            )
            .await;
        assert!(saved.starts_with("saved"), "memory_save: {saved}");
    }
    // связи ставим через то же соединение приложения (ctx.db)
    link(&db, "q-hit", "n1", "manual");
    link(&db, "n1", "n2", "manual");

    // (3) scope=memory — ответ строится по PPR-подграфу памяти.
    // Запрос — точный термин из записи: сидом становится она сама, и она обязана
    // быть в факт-блоке (у сида всегда есть базовая масса персонализации).
    let mem = server
        .call_tool("graph_reason", json!({"query": "альфа", "scope": "memory"}))
        .await;
    assert!(mem.contains("scope: memory"), "{mem}");
    assert!(mem.contains("memory_confidence:"), "{mem}");
    assert!(mem.contains("q-hit"), "{mem}");

    // (4) mode=graph — PPR-расширение в отдельном блоке [ppr].
    // limit=1 → сид ровно один, второй конец ручной связи гарантированно не сид,
    // значит расширение непустое (сам `related_records` сиды исключает).
    let graph_mode = server
        .call_tool(
            "memory_search",
            json!({"query": "альфа", "mode": "graph", "limit": 1}),
        )
        .await;
    assert!(graph_mode.contains("[ppr]"), "нет PPR-блока: {graph_mode}");
    let expanded = graph_mode.contains("n1") || graph_mode.contains("q-hit");
    assert!(
        expanded,
        "PPR-блок без соседа по memory_links: {graph_mode}"
    );

    // (5) контроль: обычный режим без related не добавляет блок расширения
    let plain_search = server
        .call_tool(
            "memory_search",
            json!({"query": "альфа", "mode": "hybrid", "limit": 1}),
        )
        .await;
    assert!(
        !plain_search.contains("[ppr]"),
        "hybrid-режим не должен включать PPR: {plain_search}"
    );

    // (6) mode=graph с related=true — тот же путь, без падения
    let graph_related = server
        .call_tool(
            "memory_search",
            json!({"query": "альфа", "mode": "graph", "related": true, "limit": 1}),
        )
        .await;
    assert!(graph_related.contains("[ppr]"), "{graph_related}");
}
