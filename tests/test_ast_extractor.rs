//! Тесты AST-парсера кода (Rust, Python, TS/JS, SQL, PHP, Dart, Java, C#, C/C++, Kotlin, Swift, Ruby) (Фаза 11).

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
    extractor.parse_file(
        "src/main/java/com/example/service/AccountManager.java",
        java_code,
        &mut res,
    );

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

#[test]
fn test_parse_csharp_code() {
    let extractor = AstCodeExtractor::new();
    let csharp_code = r#"
    using System;
    using System.Collections.Generic;

    namespace App.Services
    {
        public class AccountService : BaseService, IAuditable
        {
            private readonly IRepository repo;

            public Account GetAccount(int id)
            {
                return null;
            }

            public async Task SyncAsync()
            {
            }
        }

        public interface IAuditable
        {
            void Audit(string action);
        }
    }
    "#;

    let mut res = AstScanResult::default();
    extractor.parse_file("src/Services/AccountService.cs", csharp_code, &mut res);

    let node_labels: Vec<String> = res.nodes.iter().map(|n| n.label.clone()).collect();
    assert!(node_labels.contains(&"AccountService".to_string()));
    assert!(node_labels.contains(&"IAuditable".to_string()));
    assert!(node_labels.contains(&"GetAccount".to_string()));
    assert!(node_labels.contains(&"SyncAsync".to_string()));

    let edge_labels: Vec<String> = res.edges.iter().map(|e| e.label.clone()).collect();
    assert!(edge_labels.contains(&"IMPORTS".to_string()));
    assert!(edge_labels.contains(&"DEFINES".to_string()));
    assert!(edge_labels.contains(&"INHERITS".to_string()));
    assert!(edge_labels.contains(&"IMPLEMENTS".to_string()));
}

#[test]
fn test_parse_c_cpp_code() {
    let extractor = AstCodeExtractor::new();
    let cpp_code = r#"
    #include <string>
    #include "models/user.h"

    class UserService : public BaseService {
    public:
        std::string getUser(int id);
    };

    std::string UserService::getUser(int id) {
        return name;
    }

    struct Point {
        int x;
        int y;
    };

    int main(int argc, char** argv) {
        return 0;
    }
    "#;

    let mut res = AstScanResult::default();
    extractor.parse_file("src/user_service.cpp", cpp_code, &mut res);

    let node_labels: Vec<String> = res.nodes.iter().map(|n| n.label.clone()).collect();
    assert!(node_labels.contains(&"UserService".to_string()));
    assert!(node_labels.contains(&"Point".to_string()));
    assert!(node_labels.contains(&"getUser".to_string()));
    assert!(node_labels.contains(&"main".to_string()));

    let edge_labels: Vec<String> = res.edges.iter().map(|e| e.label.clone()).collect();
    assert!(edge_labels.contains(&"IMPORTS".to_string()));
    assert!(edge_labels.contains(&"DEFINES".to_string()));
    assert!(edge_labels.contains(&"INHERITS".to_string()));
}

#[test]
fn test_parse_kotlin_code() {
    let extractor = AstCodeExtractor::new();
    let kotlin_code = r#"
    import kotlin.collections.List
    import com.example.model.User as UserModel

    data class UserRepository(val api: Api) : BaseRepository(), IAuditable {
        override fun findUser(id: Int): UserModel? {
            return null
        }

        suspend fun syncAll() {
        }
    }

    interface IAuditable {
        fun audit(action: String)
    }
    "#;

    let mut res = AstScanResult::default();
    extractor.parse_file("src/main/kotlin/UserRepository.kt", kotlin_code, &mut res);

    let node_labels: Vec<String> = res.nodes.iter().map(|n| n.label.clone()).collect();
    assert!(node_labels.contains(&"UserRepository".to_string()));
    assert!(node_labels.contains(&"IAuditable".to_string()));
    assert!(node_labels.contains(&"findUser".to_string()));
    assert!(node_labels.contains(&"syncAll".to_string()));

    let edge_labels: Vec<String> = res.edges.iter().map(|e| e.label.clone()).collect();
    assert!(edge_labels.contains(&"IMPORTS".to_string()));
    assert!(edge_labels.contains(&"DEFINES".to_string()));
    assert!(edge_labels.contains(&"INHERITS".to_string()));
    assert!(edge_labels.contains(&"IMPLEMENTS".to_string()));
}

#[test]
fn test_parse_swift_code() {
    let extractor = AstCodeExtractor::new();
    let swift_code = r#"
    import Foundation
    import UIKit

    public class UserStore: ObservableObject, Sendable {
        private var users: [String] = []

        init(name: String) {
        }

        func fetchUsers() -> [String] {
            return users
        }
    }

    protocol Persistable {
        func save()
    }

    extension UserStore: Persistable {
        func save() {
        }
    }

    enum Role: String, CaseIterable {
        case admin
    }
    "#;

    let mut res = AstScanResult::default();
    extractor.parse_file("Sources/App/UserStore.swift", swift_code, &mut res);

    let node_labels: Vec<String> = res.nodes.iter().map(|n| n.label.clone()).collect();
    assert!(node_labels.contains(&"UserStore".to_string()));
    assert!(node_labels.contains(&"Persistable".to_string()));
    assert!(node_labels.contains(&"fetchUsers".to_string()));
    assert!(node_labels.contains(&"save".to_string()));
    assert!(node_labels.contains(&"init".to_string()));

    let edge_labels: Vec<String> = res.edges.iter().map(|e| e.label.clone()).collect();
    assert!(edge_labels.contains(&"IMPORTS".to_string()));
    assert!(edge_labels.contains(&"DEFINES".to_string()));
    assert!(edge_labels.contains(&"INHERITS".to_string()));
    assert!(edge_labels.contains(&"IMPLEMENTS".to_string()));
}

#[test]
fn test_parse_ruby_code() {
    let extractor = AstCodeExtractor::new();
    let ruby_code = r#"
    require 'json'
    require_relative '../models/user'

    module Services
      class UserService < BaseService
        def initialize(repo)
          @repo = repo
        end

        def self.build(repo)
          new(repo)
        end

        def find_user(id)
          @repo.find(id)
        end

        def valid?
          true
        end
      end
    end
    "#;

    let mut res = AstScanResult::default();
    extractor.parse_file("app/services/user_service.rb", ruby_code, &mut res);

    let node_labels: Vec<String> = res.nodes.iter().map(|n| n.label.clone()).collect();
    assert!(node_labels.contains(&"Services".to_string()));
    assert!(node_labels.contains(&"UserService".to_string()));
    assert!(node_labels.contains(&"initialize".to_string()));
    assert!(node_labels.contains(&"build".to_string()));
    assert!(node_labels.contains(&"find_user".to_string()));
    assert!(node_labels.contains(&"valid?".to_string()));

    let edge_labels: Vec<String> = res.edges.iter().map(|e| e.label.clone()).collect();
    assert!(edge_labels.contains(&"IMPORTS".to_string()));
    assert!(edge_labels.contains(&"DEFINES".to_string()));
    assert!(edge_labels.contains(&"INHERITS".to_string()));
}

#[test]
fn test_parse_vue_sfc_code() {
    let extractor = AstCodeExtractor::new();
    let vue_code = r#"<template>
  <div class="user">{{ name }}</div>
</template>

<script setup lang="ts">
import { ref } from 'vue';
import UserService from './UserService';

export class UserView {
    render() {
        return null;
    }
}

function loadUser() {
    return null;
}
</script>

<style scoped>
.user { color: red; }
</style>
"#;

    let mut res = AstScanResult::default();
    extractor.parse_file("src/components/UserView.vue", vue_code, &mut res);

    let node_labels: Vec<String> = res.nodes.iter().map(|n| n.label.clone()).collect();
    assert!(node_labels.contains(&"UserView".to_string()));
    assert!(node_labels.contains(&"loadUser".to_string()));

    // Номер строки символа — относительно всего файла, а не script-блока
    let load_user = res
        .nodes
        .iter()
        .find(|n| n.label == "loadUser")
        .expect("loadUser node");
    assert_eq!(load_user.line_start, 15);

    let edge_labels: Vec<String> = res.edges.iter().map(|e| e.label.clone()).collect();
    assert!(edge_labels.contains(&"IMPORTS".to_string()));
    assert!(edge_labels.contains(&"DEFINES".to_string()));
}

#[test]
fn test_parse_scala_code() {
    let extractor = AstCodeExtractor::new();
    let scala_code = r#"
    package com.example.services

    import scala.collection.mutable
    import com.example.model.User

    case class UserService(val repo: UserRepository) extends BaseService with Auditable, Loggable {
        override def findUser(id: Int): Option[User] = {
            None
        }

        def syncAll(): Unit = {
        }
    }

    trait Auditable {
        def audit(action: String): Unit
    }
    "#;

    let mut res = AstScanResult::default();
    extractor.parse_file("src/main/scala/UserService.scala", scala_code, &mut res);

    let node_labels: Vec<String> = res.nodes.iter().map(|n| n.label.clone()).collect();
    assert!(node_labels.contains(&"UserService".to_string()));
    assert!(node_labels.contains(&"Auditable".to_string()));
    assert!(node_labels.contains(&"findUser".to_string()));
    assert!(node_labels.contains(&"syncAll".to_string()));

    let edge_labels: Vec<String> = res.edges.iter().map(|e| e.label.clone()).collect();
    assert!(edge_labels.contains(&"IMPORTS".to_string()));
    assert!(edge_labels.contains(&"DEFINES".to_string()));
    assert!(edge_labels.contains(&"INHERITS".to_string()));
    assert!(edge_labels.contains(&"IMPLEMENTS".to_string()));
}

#[test]
fn test_parse_lua_code() {
    let extractor = AstCodeExtractor::new();
    let lua_code = r#"
    local json = require("json")
    local M = {}

    function M.new(name)
        return setmetatable({ name = name }, M)
    end

    function M:greet()
        return "hello"
    end

    local function helper()
        return nil
    end

    return M
    "#;

    let mut res = AstScanResult::default();
    extractor.parse_file("src/user_service.lua", lua_code, &mut res);

    let node_labels: Vec<String> = res.nodes.iter().map(|n| n.label.clone()).collect();
    assert!(node_labels.contains(&"new".to_string()));
    assert!(node_labels.contains(&"greet".to_string()));
    assert!(node_labels.contains(&"helper".to_string()));

    let edge_labels: Vec<String> = res.edges.iter().map(|e| e.label.clone()).collect();
    assert!(edge_labels.contains(&"IMPORTS".to_string()));
    assert!(edge_labels.contains(&"DEFINES".to_string()));
}

#[test]
fn test_parse_elixir_code() {
    let extractor = AstCodeExtractor::new();
    let elixir_code = r#"
    defmodule MyApp.Services.UserService do
      alias MyApp.Repo
      import Ecto.Query

      def find_user(id) do
        nil
      end

      defp validate(id), do: id > 0

      defmacro log(msg) do
        quote do
        end
      end
    end

    defprotocol Serializable do
      def to_map(data)
    end
    "#;

    let mut res = AstScanResult::default();
    extractor.parse_file("lib/my_app/services/user_service.ex", elixir_code, &mut res);

    let node_labels: Vec<String> = res.nodes.iter().map(|n| n.label.clone()).collect();
    assert!(node_labels.contains(&"MyApp.Services.UserService".to_string()));
    assert!(node_labels.contains(&"Serializable".to_string()));
    assert!(node_labels.contains(&"find_user".to_string()));
    assert!(node_labels.contains(&"validate".to_string()));
    assert!(node_labels.contains(&"log".to_string()));

    let edge_labels: Vec<String> = res.edges.iter().map(|e| e.label.clone()).collect();
    assert!(edge_labels.contains(&"IMPORTS".to_string()));
    assert!(edge_labels.contains(&"DEFINES".to_string()));
}

#[test]
fn test_parse_shell_code() {
    let extractor = AstCodeExtractor::new();
    let shell_code = r#"#!/usr/bin/env bash
source ./lib.sh
. ./env.sh

deploy_app() {
    echo "deploying"
}

function cleanup {
    rm -rf tmp
}

usage() {
    echo "usage"
}
"#;

    let mut res = AstScanResult::default();
    extractor.parse_file("scripts/deploy.sh", shell_code, &mut res);

    let node_labels: Vec<String> = res.nodes.iter().map(|n| n.label.clone()).collect();
    assert!(node_labels.contains(&"deploy_app".to_string()));
    assert!(node_labels.contains(&"cleanup".to_string()));
    assert!(node_labels.contains(&"usage".to_string()));

    let edge_labels: Vec<String> = res.edges.iter().map(|e| e.label.clone()).collect();
    assert!(edge_labels.contains(&"IMPORTS".to_string()));
    assert!(edge_labels.contains(&"DEFINES".to_string()));
}

#[test]
fn test_ts_function_and_class_body_spans() {
    let extractor = AstCodeExtractor::new();
    let code = r#"import { ref } from 'vue';

export function loadUser(id: string) {
    const payload = {
        id,
        ok: true,
    };
    return payload;
}

export const pick = (v: number) => v + 1;

export class Store {
    items: string[] = [];
}

export const noop = () => console.log('}{ not a brace');
"#;

    let mut res = AstScanResult::default();
    extractor.parse_file("src/User.ts", code, &mut res);

    let span = |label: &str| {
        let n = res
            .nodes
            .iter()
            .find(|n| n.label == label)
            .unwrap_or_else(|| panic!("нет узла {label}"));
        (n.line_start, n.line_end)
    };

    // Ф42 (issue 3.4): у функций/классов есть диапазон тела, а не одна строка
    assert_eq!(span("loadUser"), (3, 9), "функция с вложенным объектом");
    assert_eq!(
        span("pick"),
        (11, 11),
        "expression-body стрелка — одна строка"
    );
    assert_eq!(span("Store"), (13, 15), "класс до закрывающей скобки");
    assert_eq!(span("noop"), (17, 17), "скобки внутри строки не считаются");
}

#[test]
fn test_python_multiline_signature_and_route() {
    // Ф42 (issue 14): многострочные `def` и многострочные route-декораторы.
    let extractor = AstCodeExtractor::new();
    let code = r#"from fastapi import APIRouter
router = APIRouter()

@router.post(
    "/ask",
    status_code=200,
)
async def ask_kag(
    self,
    query: str,
    limit: int = 5,
) -> dict:
    result = self._run(query)
    return result

def helper(
    a: int,
) -> int:
    return a
"#;

    let mut res = AstScanResult::default();
    extractor.parse_file("app/api/chat.py", code, &mut res);

    let find = |label: &str| {
        res.nodes
            .iter()
            .find(|n| n.label == label)
            .unwrap_or_else(|| panic!("нет узла {label}"))
    };

    let ask = find("ask_kag");
    assert_eq!(
        (ask.line_start, ask.line_end),
        (8, 14),
        "многострочная сигнатура"
    );
    let help = find("helper");
    assert_eq!(
        (help.line_start, help.line_end),
        (16, 19),
        "тело после многострочной сигнатуры"
    );

    let route = res
        .edges
        .iter()
        .find(|e| e.label == "ROUTE")
        .expect("ROUTE для многострочного декоратора");
    assert_eq!(route.source_node_id, "fn:app/api/chat.py:ask_kag");
    assert_eq!(route.target_node_id, "route:POST /ask");
}

/// Многострочный декоратор регистрирует функцию как точку входа.
#[test]
fn test_python_multiline_decorator_keeps_entrypoint() {
    let extractor = AstCodeExtractor::new();
    let mut res = AstScanResult::default();
    extractor.parse_file(
        "app/commands.py",
        "@click.command(\n    name=\"run\",\n)\ndef run_command():\n    return 1\n",
        &mut res,
    );
    let node = res.nodes.iter().find(|n| n.label == "run_command").unwrap();
    assert!(node.description.contains("@click.command("));
    assert_eq!((node.line_start, node.line_end), (4, 5));
}

/// Встроенные Python/TS блоки используют координаты всего документа.
#[test]
fn test_embedded_code_spans_keep_offsets() {
    let extractor = AstCodeExtractor::new();
    let mut sfc = AstScanResult::default();
    extractor.parse_file(
        "src/View.vue",
        "<template>\n<div />\n</template>\n<script lang=\"ts\">\nexport function load() {\n  return 1;\n}\n</script>",
        &mut sfc,
    );
    let node = sfc.nodes.iter().find(|n| n.label == "load").unwrap();
    assert_eq!((node.line_start, node.line_end), (5, 7));

    let mut notebook = AstScanResult::default();
    extractor.parse_file(
        "analysis.ipynb",
        r#"{"cells":[{"cell_type":"code","source":["x = 1\n"]},{"cell_type":"code","source":["def compute():\n","    return x\n"]}]}"#,
        &mut notebook,
    );
    let node = notebook
        .nodes
        .iter()
        .find(|n| n.label == "compute")
        .unwrap();
    assert_eq!((node.line_start, node.line_end), (3, 4));
}

/// Go/Zig функции и Zig-тесты сохраняют границы тела, включая вложенные блоки.
#[test]
fn test_go_and_zig_function_body_spans() {
    let extractor = AstCodeExtractor::new();
    let mut go = AstScanResult::default();
    extractor.parse_file(
        "src/main.go",
        "package main\n\nfunc compute(x int) int {\n    if x > 0 {\n        return x\n    }\n    return 0\n}\n",
        &mut go,
    );
    let compute = go
        .nodes
        .iter()
        .find(|node| node.label == "compute")
        .unwrap();
    assert_eq!((compute.line_start, compute.line_end), (3, 8));

    let mut zig = AstScanResult::default();
    extractor.parse_file(
        "src/main.zig",
        "pub fn compute(x: i32) i32 {\n    if (x > 0) {\n        return x;\n    }\n    return 0;\n}\n\ntest \"compute\" {\n    try check(compute(1));\n}\n",
        &mut zig,
    );
    let compute = zig
        .nodes
        .iter()
        .find(|node| node.label == "compute")
        .unwrap();
    assert_eq!((compute.line_start, compute.line_end), (1, 6));
    let test = zig
        .nodes
        .iter()
        .find(|node| node.label == "test_compute")
        .unwrap();
    assert_eq!((test.line_start, test.line_end), (8, 10));
}
