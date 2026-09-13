//! Ф23: trust, feedback-петля, memory_links. Без сети (FakeLLM не нужен —
//! ревизия вызывается напрямую).

use std::sync::Arc;

use ob2h::db::Database;
use ob2h::embedding::FakeEmbedding;
use ob2h::memory::MemoryService;

fn service() -> MemoryService {
    let db = Database::in_memory().expect("db");
    let embedder = Arc::new(FakeEmbedding::new(384));
    MemoryService::new(db, embedder)
}

#[tokio::test]
async fn feedback_clamps_trust_to_0_1() {
    let svc = service();
    let k = svc
        .save("Пользователь любит кофе", Some("hmem-coffee"), "preferences", 0.5, "chat", None)
        .await
        .expect("save");

    // helpful: 0.5 + 0.15
    let t = svc.record_feedback(&k, "helpful", None).expect("feedback").expect("trust");
    assert!((t - 0.65).abs() < 1e-9);

    // Кламп сверху: много helpful — не выше 1.0
    for _ in 0..10 {
        let t = svc.record_feedback(&k, "helpful", None).expect("feedback").unwrap();
        assert!(t <= 1.0 + 1e-9);
    }

    // Кламп снизу: много unhelpful — не ниже 0.0
    for _ in 0..20 {
        let t = svc.record_feedback(&k, "unhelpful", None).expect("feedback").unwrap();
        assert!(t >= 0.0 - 1e-9);
    }

    // Неверный вердикт — ошибка
    assert!(svc.record_feedback(&k, "magic", None).is_err());
    // Несуществующий ключ — None
    assert!(svc.record_feedback("hmem-ghost", "helpful", None).expect("none").is_none());
}

#[tokio::test]
async fn candidate_for_forget_marks_but_never_deletes() {
    let svc = service();
    let k = svc
        .save("Устаревший факт", Some("hmem-old"), "general", 0.3, "chat", None)
        .await
        .expect("save");

    // Утопить trust ниже 0.15: outdated −0.3 дважды с 0.5 → 0.5−0.3=0.2 → 0.2−0.3→кламп 0
    let _ = svc.record_feedback(&k, "outdated", None).expect("fb1");
    let _ = svc.record_feedback(&k, "outdated", None).expect("fb2");

    let rec = svc.get(&k).expect("get").expect("запись жива");
    assert_eq!(rec.access_count, 0);
    let meta = rec.meta.as_deref().unwrap_or("{}");
    assert!(
        meta.contains("candidate_for_forget"),
        "метка кандидата на forget проставлена: {meta}"
    );
    // Никакого автоудаления: запись по-прежнему читается
    assert!(!meta.contains("deleted"));
}

#[tokio::test]
async fn lowest_trust_orders_by_trust() {
    let svc = service();
    let k_low = svc
        .save("Сомнительная запись", Some("hmem-low"), "misc", 0.4, "chat", None)
        .await
        .expect("save low");
    svc.save("Надёжная запись", Some("hmem-high"), "misc", 0.9, "chat", None)
        .await
        .expect("save high");

    // Понижаем первую
    svc.revise_trust_by_key(&k_low, "contradicted").expect("revise").expect("trust");

    let low = svc.lowest_trust(1).expect("lowest");
    assert_eq!(low.len(), 1);
    assert_eq!(low[0].0.key, "hmem-low");
}

#[tokio::test]
async fn auto_links_created_deduped_and_cascaded() {
    let svc = service();
    let k1 = svc
        .save("Правило А про деплой", Some("hmem-a"), "devops", 0.7, "chat", Some("proj1"))
        .await
        .expect("save 1");
    let k2 = svc
        .save("Правило Б про деплой", Some("hmem-b"), "devops", 0.7, "chat", Some("proj1"))
        .await
        .expect("save 2");
    // Повторное сохранение той же записи — дедуп связей по PK
    svc.save("Правило Б про деплой", Some("hmem-b"), "devops", 0.7, "chat", Some("proj1"))
        .await
        .expect("re-save 2");

    let rec_a = svc.get(&k1).expect("get a").expect("a");
    let related_to_a = svc.related_records(&[rec_a.id], 10).expect("related");
    assert!(
        related_to_a.iter().any(|r| r.key == k2),
        "связь category/same_project проставлена"
    );

    // Forget — каскад: связи удалённой записи рвутся
    svc.forget(&k2).expect("forget");
    let related_after = svc.related_records(&[rec_a.id], 10).expect("related");
    assert!(
        !related_after.iter().any(|r| r.key == k2),
        "после forget связь исчезла"
    );
    // И запись k2 более не в выдаче related (tombstone)
    assert!(svc.get(&k2).expect("get").is_none());
}
