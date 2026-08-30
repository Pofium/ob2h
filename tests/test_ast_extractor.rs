//! Тесты AST-парсера кода (Rust, Python, TS/JS, SQL) (Фаза 11).

use ob2h::project::ast::{AstCodeExtractor, AstScanResult};

#[test]
fn test_parse_rust_code() {
    let extractor = AstCodeExtractor::new();
    let rust_code = r#"
    use crate::db::Database;
    use crate::embedding::EmbeddingProvider;

    pub struct MemoryService {
        db: Database,
    }

    pub trait KnowledgeProvider {
        fn extract(&self);
    }

    impl MemoryService {
        pub async fn search_hybrid(&self, query: &str, limit: usize) -> Vec<Hit> {
            vec![]
        }
    }
    "#;

    let mut res = AstScanResult::default();
    extractor.parse_file("src/memory/service.rs", rust_code, &mut res);

    assert!(!res.nodes.is_empty());
    
    // Проверяем извлечение структур, трейтов, функций
    let node_labels: Vec<String> = res.nodes.iter().map(|n| n.label.clone()).collect();
    assert!(node_labels.contains(&"MemoryService".to_string()));
    assert!(node_labels.contains(&"KnowledgeProvider".to_string()));
    assert!(node_labels.contains(&"search_hybrid".to_string()));

    // Проверяем извлечение use-импортов
    let edge_labels: Vec<String> = res.edges.iter().map(|e| e.label.clone()).collect();
    assert!(edge_labels.contains(&"IMPORTS".to_string()));
    assert!(edge_labels.contains(&"DEFINES".to_string()));
}

#[test]
fn test_parse_python_code() {
    let extractor = AstCodeExtractor::new();
    let py_code = r#"
    import json
    from typing import List, Optional

    class VectorStore(BaseStore):
        def __init__(self, dim: int):
            self.dim = dim

        def similarity_search(self, query: str) -> List[dict]:
            return []
    "#;

    let mut res = AstScanResult::default();
    extractor.parse_file("app/vector.py", py_code, &mut res);

    let node_labels: Vec<String> = res.nodes.iter().map(|n| n.label.clone()).collect();
    assert!(node_labels.contains(&"VectorStore".to_string()));
    assert!(node_labels.contains(&"similarity_search".to_string()));

    let edge_labels: Vec<String> = res.edges.iter().map(|e| e.label.clone()).collect();
    assert!(edge_labels.contains(&"IMPORTS".to_string()));
    assert!(edge_labels.contains(&"INHERITS".to_string()));
}

#[test]
fn test_parse_sql_code() {
    let extractor = AstCodeExtractor::new();
    let sql_code = r#"
    CREATE TABLE IF NOT EXISTS users (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL
    );

    CREATE TABLE orders (
        id INTEGER PRIMARY KEY,
        user_id INTEGER REFERENCES users(id)
    );
    "#;

    let mut res = AstScanResult::default();
    extractor.parse_file("schema.sql", sql_code, &mut res);

    let node_labels: Vec<String> = res.nodes.iter().map(|n| n.label.clone()).collect();
    assert!(node_labels.contains(&"users".to_string()));
    assert!(node_labels.contains(&"orders".to_string()));

    let edge_labels: Vec<String> = res.edges.iter().map(|e| e.label.clone()).collect();
    assert!(edge_labels.contains(&"FOREIGN_KEY_TO".to_string()));
}
