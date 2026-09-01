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

#[test]
fn test_parse_php_code() {
    let extractor = AstCodeExtractor::new();
    let php_code = r#"<?php
    namespace App\Services;

    use App\Repositories\UserRepository;
    use App\Contracts\Auditable;

    class UserService extends BaseService implements Auditable {
        private UserRepository $repo;

        public function __construct(UserRepository $repo) {
            $this->repo = $repo;
        }

        public function findUserById(int $id): ?User {
            return $this->repo->find($id);
        }
    }

    interface Auditable {
        public function auditLog(): void;
    }

    trait Loggable {
        public function log(string $msg): void {}
    }
    "#;

    let mut res = AstScanResult::default();
    extractor.parse_file("src/Services/UserService.php", php_code, &mut res);

    let node_labels: Vec<String> = res.nodes.iter().map(|n| n.label.clone()).collect();
    assert!(node_labels.contains(&"UserService".to_string()));
    assert!(node_labels.contains(&"Auditable".to_string()));
    assert!(node_labels.contains(&"Loggable".to_string()));
    assert!(node_labels.contains(&"findUserById".to_string()));

    let edge_labels: Vec<String> = res.edges.iter().map(|e| e.label.clone()).collect();
    assert!(edge_labels.contains(&"IMPORTS".to_string()));
    assert!(edge_labels.contains(&"DEFINES".to_string()));
    assert!(edge_labels.contains(&"INHERITS".to_string()));
    assert!(edge_labels.contains(&"IMPLEMENTS".to_string()));
}

#[test]
fn test_parse_dart_code() {
    let extractor = AstCodeExtractor::new();
    let dart_code = r#"
    import 'package:flutter/material.dart';
    import 'package:provider/provider.dart';

    class ProfileScreen extends StatefulWidget with RouteAware implements Disposable {
        const ProfileScreen({Key? key}) : super(key: key);

        @override
        State<ProfileScreen> createState() => _ProfileScreenState();
    }

    mixin RouteAware {
        void didPush() {}
    }

    void main() {
        runApp(const MyApp());
    }
    "#;

    let mut res = AstScanResult::default();
    extractor.parse_file("lib/screens/profile_screen.dart", dart_code, &mut res);

    let node_labels: Vec<String> = res.nodes.iter().map(|n| n.label.clone()).collect();
    assert!(node_labels.contains(&"ProfileScreen".to_string()));
    assert!(node_labels.contains(&"RouteAware".to_string()));
    assert!(node_labels.contains(&"main".to_string()));

    let edge_labels: Vec<String> = res.edges.iter().map(|e| e.label.clone()).collect();
    assert!(edge_labels.contains(&"IMPORTS".to_string()));
    assert!(edge_labels.contains(&"DEFINES".to_string()));
    assert!(edge_labels.contains(&"INHERITS".to_string()));
    assert!(edge_labels.contains(&"IMPLEMENTS".to_string()));
}

#[test]
fn test_parse_java_code() {
    let extractor = AstCodeExtractor::new();
    let java_code = r#"
    package com.example.service;

    import java.util.List;
    import com.example.model.Account;

    public class AccountManager extends AbstractManager implements IAccountService {
        private final List<Account> accounts;

        public Account getAccount(String id) {
            return null;
        }

        public void syncAccounts() {
        }
    }

    public interface IAccountService {
        Account getAccount(String id);
    }
    "#;

    let mut res = AstScanResult::default();
    extractor.parse_file("src/main/java/com/example/service/AccountManager.java", java_code, &mut res);

    let node_labels: Vec<String> = res.nodes.iter().map(|n| n.label.clone()).collect();
    assert!(node_labels.contains(&"AccountManager".to_string()));
    assert!(node_labels.contains(&"IAccountService".to_string()));
    assert!(node_labels.contains(&"getAccount".to_string()));
    assert!(node_labels.contains(&"syncAccounts".to_string()));

    let edge_labels: Vec<String> = res.edges.iter().map(|e| e.label.clone()).collect();
    assert!(edge_labels.contains(&"IMPORTS".to_string()));
    assert!(edge_labels.contains(&"DEFINES".to_string()));
    assert!(edge_labels.contains(&"INHERITS".to_string()));
    assert!(edge_labels.contains(&"IMPLEMENTS".to_string()));
}
