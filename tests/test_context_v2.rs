//! Ф22: поведение нового build_context — гибридный пул, бюджет символов,
//! touch только вошедших записей, fallback без запроса. Без сети: эмбеддер
//! с фиксированными векторами по словарю.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use ob2h::db::Database;
use ob2h::embedding::EmbeddingProvider;
use ob2h::memory::{ContextOptions, MemoryService};

/// Эмбеддер-словарь: точный текст → вектор; неизвестный текст → нулевой вектор.
struct FixedEmbedder {
    map: HashMap<String, Vec<f32>>,
    dim: usize,
}

impl FixedEmbedder {
    fn new(dim: usize) -> Self {
        Self { map: HashMap::new(), dim }
    }
    fn put(&mut self, text: &str, vec: Vec<f32>) {
        self.map.insert(text.to_string(), vec);
    }
}

#[async_trait]
impl EmbeddingProvider for FixedEmbedder {
    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| self.map.get(t).cloned().unwrap_or_else(|| vec![0.0; self.dim]))
            .collect())
    }
    fn dim(&self) -> usize {
        self.dim
    }
}

fn long_rule(head: &str) -> String {
    format!("{head}{}", "х".repeat(180))
}

async fn setup() -> (MemoryService, Vec<String>) {
    let db = Database::in_memory().expect("db");
    let mut emb = FixedEmbedder::new(2);
    // Векторы записей различны (Ф31.1: cos ≥ 0.98 = identity-дубль → слияние при save),
    // но все в положительной полуплоскости запроса «кофе», чтобы попасть в пул.
    let coffee_a = [1.0f32, 0.0];
    let coffee_b = [0.9f32, 0.435889]; // cos(a, b) ≈ 0.9 — не дубль
    let other = [0.0f32, 1.0];
    let opposite = [-1.0f32, 0.0];
    let query = [1.0f32, 0.0];

    let a = long_rule("Кофе правило номер один");
    let b = long_rule("Кофе правило номер два");
    let c = "Независимая запись про VPS и бэкапы".to_string();
    let d = "Сторонний факт без пересечений по теме".to_string();

    emb.put(&a, coffee_a.to_vec());
    emb.put(&b, coffee_b.to_vec());
    emb.put(&c, other.to_vec());
    emb.put(&d, opposite.to_vec());
    emb.put("кофе", query.to_vec());

    let embedder = Arc::new(emb);
    let service = MemoryService::new(db, embedder);
    let ka = service.save(&a, Some("hmem-a"), "rules", 0.8, "chat", None).await.unwrap();
    let kb = service.save(&b, Some("hmem-b"), "rules", 0.7, "chat", None).await.unwrap();
    let kc = service.save(&c, Some("hmem-c"), "infra", 0.6, "chat", None).await.unwrap();
    let _kd = service.save(&d, Some("hmem-d"), "misc", 0.9, "chat", None).await.unwrap();
    (service, vec![ka, kb, kc])
}

#[tokio::test]
async fn budget_cuts_by_record_boundaries_with_marker() {
    let (service, _) = setup().await;
    // 400: первая длинная (~214) + короткая (~47) + маркер (~25) + шапка/хвост (30) ≈ 316;
    // вторая длинная запись уже не помещается.
    let opts = ContextOptions { max_chars: Some(400), ..Default::default() };
    let block = service
        .build_context(30, Some("кофе"), &opts)
        .await
        .expect("build_context");

    assert!(block.contains("<agent_memory>"));
    assert!(block.contains("</agent_memory>"));
    // Бюджет не превышен (обрезка в Rust, spill в Hermes не уходит)
    assert!(
        block.chars().count() <= 400,
        "блок {} символов > бюджета 400",
        block.chars().count()
    );
    // Первая запись помещена целиком (record-границы), вторая длинная — нет
    assert!(block.contains("Кофе правило номер один"));
    assert!(!block.contains("Кофе правило номер два"));
    assert!(block.contains("truncated"), "маркер обрезки присутствует");
}

#[tokio::test]
async fn touch_counts_only_included_records() {
    let (service, keys) = setup().await;
    // Без бюджета в блок входит весь пул (A, B, C); D не в пуле (косинус < 0, нет FTS).
    let opts = ContextOptions::default();
    service.build_context(30, Some("кофе"), &opts).await.expect("build_context");

    for key in &keys {
        let rec = service.get(key).expect("get").expect("record");
        assert_eq!(rec.access_count, 1, "запись {key} из блока должна быть touch'нута");
    }
    let d = service.get("hmem-d").expect("get").expect("record");
    assert_eq!(d.access_count, 0, "запись вне пула не трогается");
}

#[tokio::test]
async fn trust_boost_lifts_record_in_prefetch_scoring() {
    let (service, _) = setup().await;
    // B менее релевантна запросу «кофе», чем A (cos 0.9 против 1.0), но trust-петля
    // (4× helpful → trust 1.0) должна поднять её на первое место (§22.2: 0.2*trust).
    for _ in 0..4 {
        service.record_feedback("hmem-b", "helpful", None).expect("feedback");
    }
    let a_id = service.get("hmem-a").expect("get").expect("record").id;
    let b_id = service.get("hmem-b").expect("get").expect("record").id;
    let tm = service.trust_map(&[a_id, b_id]).expect("trust_map");
    assert!(tm[&b_id] > tm[&a_id], "trust B должен стать выше A");

    // Бюджет вмещает только первую запись блока — первая позиция и есть проверка.
    let opts = ContextOptions { max_chars: Some(260), ..Default::default() };
    let block = service
        .build_context(30, Some("кофе"), &opts)
        .await
        .expect("build_context");
    assert!(
        block.contains("Кофе правило номер два"),
        "высокий trust должен поднять B на первое место: {block}"
    );
    assert!(!block.contains("Кофе правило номер один"));
}

#[tokio::test]
async fn empty_query_uses_importance_fallback() {
    let (service, _) = setup().await;
    let block = service
        .build_context(10, None, &ContextOptions::default())
        .await
        .expect("build_context");
    // Fallback: топ по importance — hmem-d (0.9) должен быть в блоке
    assert!(block.contains("Сторонний факт"));
    assert!(block.contains("<agent_memory>"));
}

#[tokio::test]
async fn fallback_output_is_deterministic() {
    let (service, _) = setup().await;
    // Fallback-путь (пустой запрос) не трогает access-счётчики → повтор вызова
    // обязан давать байт-в-байт тот же блок. Гибридный путь намеренно
    // недетерминирован между вызовами: touch усиливает реально используемые записи.
    let opts = ContextOptions { max_chars: Some(300), ..Default::default() };
    let first = service.build_context(10, None, &opts).await.expect("ctx");
    let second = service.build_context(10, None, &opts).await.expect("ctx");
    assert_eq!(first, second);
}
