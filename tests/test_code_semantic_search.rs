//! Интеграционный тест семантического векторного поиска по коду (Фаза 19).

use std::sync::Arc;
use rusqlite::params;
use ob2h::db::Database;
use ob2h::embedding::fake::FakeEmbedding;
use ob2h::graph::GraphService;
use ob2h::project::ProjectService;

#[tokio::test]
async fn test_code_nodes_embedding_and_hybrid_search() -> anyhow::Result<()> {
    let db = Database::in_memory()?;
    let embedder = Arc::new(FakeEmbedding::new(384));

    let project_service = ProjectService::new_with_embedder(db.conn_arc(), Some(embedder.clone()));
    let graph_service = GraphService::new(db.clone(), embedder.clone());

    // 1. Регистрируем проект
    let proj = project_service.register_project("test_proj", "Test Project", "/app", None, None)?;

    // 2. Добавляем тестовые узлы кода в graph_nodes без векторов
    db.with_conn(|conn| {
        conn.execute(
            r#"
            INSERT INTO graph_nodes (
                node_id, label, node_type, file_path, line_start, line_end, description,
                project_id, provenance, created_at, updated_at
            ) VALUES 
            ('fn_jwt', 'verify_jwt_token', 'Function', 'src/auth/jwt.rs', 10, 45, 'Проверка подписи токена авторизации и валидация claims', 'test_proj', 'ast', '2026-01-01', '2026-01-01'),
            ('fn_db', 'connect_database', 'Function', 'src/db/conn.rs', 15, 60, 'Инициализация пула соединений с базой данных SQLite', 'test_proj', 'ast', '2026-01-01', '2026-01-01'),
            ('struct_pay', 'PaymentGateway', 'Struct', 'src/billing/pay.rs', 5, 80, 'Обработка транзакций и списание денежных средств пользователя', 'test_proj', 'ast', '2026-01-01', '2026-01-01')
            "#,
            [],
        )?;
        Ok(())
    })?;

    // 3. Запускаем батчевую векторизацию узлов
    let embedded_count = project_service.embed_unembedded_nodes(&proj.id).await?;
    assert_eq!(embedded_count, 3);

    // Проверяем, что embedding заполнен в SQLite
    let null_count: i64 = db.with_conn(|conn| {
        conn.query_row(
            "SELECT count(*) FROM graph_nodes WHERE project_id = ?1 AND embedding IS NULL",
            params![proj.id],
            |r| r.get(0),
        )
    })?;
    assert_eq!(null_count, 0);

    // 4. Повторный вызов не должен делать лишнюю работу
    let re_embed = project_service.embed_unembedded_nodes(&proj.id).await?;
    assert_eq!(re_embed, 0);

    // 5. Тестируем гибридный поиск по коду
    let results_auth = graph_service.search_project_nodes_hybrid(
        &proj.id,
        "авторизация jwt",
        "hybrid",
        "all",
        5,
    ).await?;
    assert!(!results_auth.is_empty());
    assert_eq!(results_auth[0].label, "verify_jwt_token");

    let results_pay = graph_service.search_project_nodes_hybrid(
        &proj.id,
        "PaymentGateway",
        "hybrid",
        "all",
        5,
    ).await?;
    assert!(!results_pay.is_empty());
    assert_eq!(results_pay[0].label, "PaymentGateway");

    // 6. Проверка работы режимов text и vector
    let text_only = graph_service.search_project_nodes_hybrid(
        &proj.id,
        "connect_database",
        "text",
        "ast",
        5,
    ).await?;
    assert!(!text_only.is_empty());
    assert_eq!(text_only[0].label, "connect_database");

    let vec_target_text = "Function connect_database in src/db/conn.rs: Инициализация пула соединений с базой данных SQLite";
    let vec_only = graph_service.search_project_nodes_hybrid(
        &proj.id,
        vec_target_text,
        "vector",
        "all",
        5,
    ).await?;
    assert!(!vec_only.is_empty());
    assert_eq!(vec_only[0].label, "connect_database");

    Ok(())
}
