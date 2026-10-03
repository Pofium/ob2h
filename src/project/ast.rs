//! Высокопроизводительный статический AST/код-парсер репозиториев (Rust, Python, TS/JS, Go, SQL,
//! PHP, Dart, Java, C#, C/C++, Kotlin, Swift, Ruby).
//! Работает полностью детерминированно и локально, без вызовов LLM и без расхода токенов.
//!
//! Ф47 (PLAN_v1.7): список языков/форматов — единый реестр [`LANGS`] (ADR-AST-2):
//! `collect_files`, `watcher::is_code_file` и [`AstCodeExtractor::parse_file`] читают его,
//! дублирующих whitelist'ов нет. Новый формат = запись в `LANGS` + `parse_*` + фикстура в тесте.

use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Извлеченный узел графа кода.
#[derive(Debug, Clone)]
pub struct AstNode {
    pub node_id: String,
    pub label: String,
    /// Словарь node_type (Ф47.3, полный перечень v1.7 — см. docs/ARCHITECTURE.md):
    /// код — `Module|Struct|Class|Interface|Trait|Function|Table|File`;
    /// авто-создаваемые цели рёбер — `Route|Table|ExternalModule` (+ по префиксам
    /// [`NODE_TYPE_BY_PREFIX`]: `Service|Endpoint|Contract|Schema|ExternalPackage|
    /// CssClass|CssId|CssVar|Element|Resource|K8sResource|BaseImage`);
    /// из манифестов/спец-форматов (Ф51–Ф52) — `Script|Mapping|Notebook|DataSource|
    /// Variable|Output|EnvVar|Port|Volume|Network`.
    pub node_type: String,
    pub description: String,
    pub file_path: String,
    pub line_start: usize,
    pub line_end: usize,
}

/// Извлеченное ребро графа кода.
#[derive(Debug, Clone)]
pub struct AstEdge {
    pub source_node_id: String,
    pub target_node_id: String,
    /// Словарь label (Ф47.3, полный перечень v1.7 — см. docs/ARCHITECTURE.md):
    /// кодовые — `IMPORTS|CALLS|DEFINES|IMPLEMENTS|INHERITS|DEPENDS_ON|FOREIGN_KEY_TO`;
    /// framework-эвристики Ф40.2 — `ROUTE|QUERIES_TABLE`;
    /// новые v1.7 (Ф48–Ф50, ADR-AST-5) — `STYLES|BINDS_CLASS|PROVIDES_RPC|
    /// SERVES_ENDPOINT|QUERIES_FIELD|USES_SCHEMA|MOUNTS|BINDS_SELECTOR`.
    /// Новые метки в `USAGE_EDGE_LABELS` (callpath.rs) не попадают — см. ADR-AST-5.
    pub label: String,
    pub weight: f64,
    pub context: String,
    /// Ф39.2 (M8): происхождение ребра — EXTRACTED (обычный скан), RESOLVED
    /// (type-pass Ф39.1), INFERRED (эвристики Ф40). Старые рёбра в БД хранят
    /// своё значение ('ast'/'manual') и читаются как есть.
    pub provenance: String,
}

/// Ф39.1: замеченное место вызова (для последующего резолва type-pass'ом).
#[derive(Debug, Clone)]
pub struct CallSite {
    pub rel_path: String,
    pub line: usize,
    pub name: String,
}

/// Ф39.1: импорт с элементом и алиасом (use/import ... as ...).
#[derive(Debug, Clone)]
pub struct ImportRef {
    pub rel_path: String,
    pub module: String,
    pub item: Option<String>,
    pub alias: Option<String>,
}

/// Ф49.4б: клиентский HTTP-вызов с path-литералом (для мостов к route:-узлам).
#[derive(Debug, Clone)]
pub struct HttpCall {
    pub rel_path: String,
    pub method: String,
    pub path: String,
}

/// Ф49.4в: идентификатор из строки GraphQL-запроса (кандидат QUERIES_FIELD).
#[derive(Debug, Clone)]
pub struct GqlFieldRef {
    pub rel_path: String,
    pub field: String,
}

/// Ф50: ссылка на узел, резолвимая по всем узлам скана (depends_on в tf,
/// configMapRef/secretRef в k8s). Неразрешённые отбрасываются — висячих
/// узлов не создаём.
#[derive(Debug, Clone)]
pub struct PendingRef {
    pub source_node_id: String,
    /// Префикс node_id цели ("resource:", "k8s:"…)
    pub target_prefix: String,
    /// Хвост node_id цели (":aws_vpc.main"; для k8s — label "Deployment/api")
    pub target_suffix: String,
    pub label: String,
    pub context: String,
}

/// Результат AST-сканирования кодовой базы.
#[derive(Debug, Clone, Default)]
pub struct AstScanResult {
    pub files_scanned: usize,
    pub lines_total: usize,
    pub nodes: Vec<AstNode>,
    pub edges: Vec<AstEdge>,
    pub file_hashes: HashMap<String, String>,
    pub file_meta: HashMap<String, (usize, usize)>, // rel_path -> (file_size, lines_count)
    /// Ф39.1: места вызовов (Rust/Python) для type-pass.
    pub call_sites: Vec<CallSite>,
    /// Ф39.1: импорты с алиасами (Rust/Python) для type-pass.
    pub imports: Vec<ImportRef>,
    /// Ф49.4б: клиентские HTTP-вызовы (ts/js/py) для мостов SERVES_ENDPOINT.
    pub http_calls: Vec<HttpCall>,
    /// Ф49.4в: идентификаторы из gql-строк для мостов QUERIES_FIELD.
    pub gql_fields: Vec<GqlFieldRef>,
    /// Ф50: ссылки, резолвимые после полного скана (tf depends_on, k8s refs).
    pub pending_refs: Vec<PendingRef>,
}

// ============================================================================
// Ф47.1 (PLAN_v1.7, ADR-AST-2): единый реестр языков и форматов.
// `collect_files` (обходчик), `watcher::is_code_file` (вотчер) и
// `parse_file` (диспетчер) читают одну таблицу LANGS — раньше было три
// расходящихся списка, и расширение, добавленное в обходчик, но забытое
// в вотчере, молча выпадало из инкрементального обновления.
// ============================================================================

/// Единый список исключаемых каталогов (Ф47.2). Один источник для обходчика и
/// вотчера; раньше списки расходились (вотчер не знал `vendor`, обходчик —
/// `logs`/`backups`).
pub const SKIP_DIRS: &[&str] = &[
    ".git", "target", "node_modules", "dist", "build", "venv", ".venv", "vendor", "data", "logs",
    "backups", "__pycache__",
];

/// Отсутствие ограничения на размер файла. Для существующих исходников cap не
/// вводим (поведение v1.6 сохраняется 1:1); cap 2 МБ / 5 МБ появится у форматов
/// Ф48–Ф52 (ADR-AST-4).
pub const NO_CAP: usize = usize::MAX;

/// Способ сопоставления файла с языком. Расширение — для исходников; точное
/// имя файла — для манифестов/контрактов (`Cargo.toml`, `openapi.yaml`,
/// `Dockerfile`, ADR-AST-4); предикат — спец-случаи (`*.Dockerfile`).
#[derive(Clone, Copy)]
pub enum Matcher {
    Ext(&'static [&'static str]),
    FileName(&'static [&'static str]),
    Predicate(fn(&str) -> bool),
}

/// Тег парсера для диспетчера parse_file (не fn-указатель: parse_* берут &self).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parser {
    Rust,
    Python,
    TsJs,
    Go,
    Sql,
    Php,
    Dart,
    Java,
    CSharp,
    CCpp,
    Kotlin,
    Swift,
    Ruby,
    Sfc,
    Scala,
    Lua,
    Elixir,
    Shell,
    /// Ф48.1: HTML (и Laravel blade — `.blade.php` через предикат).
    Html,
    /// Ф48.2: CSS/SCSS.
    Css,
    /// Ф52.1: Jupyter-ноутбук.
    Notebook,
    /// Ф52.2: Solidity.
    Solidity,
    /// Ф52.3: Zig.
    Zig,
    /// Ф49.1: Protocol Buffers.
    Proto,
    /// Ф49.2: GraphQL.
    Graphql,
    /// Ф49.3: OpenAPI/Swagger (json/yaml).
    OpenApi,
    /// Ф50.1: Terraform/OpenTofu.
    Terraform,
    /// Ф50.2: Dockerfile.
    Dockerfile,
    /// Ф50.3: docker-compose.
    Compose,
    /// Ф50.4: Kubernetes/Helm YAML.
    K8s,
    /// Ф51: манифесты пакетов (диспетчер по имени файла).
    Manifest,
}

/// Спецификация языка/формата в реестре [`LANGS`].
pub struct LangSpec {
    /// Человекочитаемое имя (логи, доки, тесты).
    pub name: &'static str,
    matcher: Matcher,
    /// Типы узлов, которые создаёт парсер (словарь node_type v1.7).
    pub node_types: &'static [&'static str],
    /// Метки рёбер, которые может создавать парсер (словарь edge_label v1.7).
    pub edge_labels: &'static [&'static str],
    /// Верхняя граница размера файла (байт); больше — файл трекается по хэшу,
    /// но не парсится (ADR-AST-4).
    pub max_bytes: usize,
    pub parser: Parser,
    /// Минимальная фикстура для теста консистентности реестра (47.5,
    /// tests/test_ast_langs.rs). Заполняется при добавлении формата.
    pub fixture: &'static str,
    /// Путь-образец для теста; пусто — выводится из matcher (Predicate-спеки
    /// вроде blade указывают его явно).
    pub sample_rel: &'static str,
}

/// Реестр языков и форматов — **единственный** whitelist сканера (Ф47.1).
pub const LANGS: &[LangSpec] = &[
    LangSpec {
        name: "rust",
        matcher: Matcher::Ext(&["rs"]),
        node_types: &["File", "Struct", "Trait", "Function"],
        edge_labels: &["IMPORTS", "DEFINES", "ROUTE"],
        max_bytes: NO_CAP,
        parser: Parser::Rust,
        fixture: r#"use crate::db::Store;

pub struct Row {
    pub id: u64,
}

pub trait Repo {
    fn get(&self, id: u64) -> Option<Row>;
}

#[get("/health")]
pub async fn health() -> &'static str {
    "ok"
}
"#,
        sample_rel: "",
    },
    LangSpec {
        name: "python",
        matcher: Matcher::Ext(&["py"]),
        node_types: &["File", "Class", "Function"],
        edge_labels: &["IMPORTS", "DEFINES", "INHERITS", "QUERIES_TABLE", "ROUTE"],
        max_bytes: NO_CAP,
        parser: Parser::Python,
        fixture: r#"import os
from typing import List

class User(Base):
    __tablename__ = "users"

    def save(self) -> None:
        pass

@app.get("/users")
def list_users():
    return []
"#,
        sample_rel: "",
    },
    LangSpec {
        name: "ts-js",
        matcher: Matcher::Ext(&["ts", "tsx", "js", "jsx"]),
        node_types: &["File", "Class", "Interface", "Function"],
        edge_labels: &["IMPORTS", "DEFINES"],
        max_bytes: NO_CAP,
        parser: Parser::TsJs,
        fixture: r"import { Store } from './store';

export class UserService extends Base implements Repo {
}

export interface Repo {
}

export function load(id) {
    return null;
}
",
        sample_rel: "",
    },
    LangSpec {
        name: "go",
        matcher: Matcher::Ext(&["go"]),
        node_types: &["File", "Struct", "Function"],
        edge_labels: &["DEFINES"],
        max_bytes: NO_CAP,
        parser: Parser::Go,
        fixture: r"package store

type Row struct {
    ID uint64
}

func Get(id uint64) *Row {
    return nil
}
",
        sample_rel: "",
    },
    LangSpec {
        name: "sql",
        matcher: Matcher::Ext(&["sql"]),
        node_types: &["File", "Table"],
        edge_labels: &["DEFINES", "FOREIGN_KEY_TO"],
        max_bytes: NO_CAP,
        parser: Parser::Sql,
        fixture: r"CREATE TABLE users (
    id INTEGER PRIMARY KEY
);

CREATE TABLE orders (
    user_id INTEGER REFERENCES users(id)
);
",
        sample_rel: "",
    },
    LangSpec {
        name: "html",
        matcher: Matcher::Ext(&["html", "htm"]),
        node_types: &["File", "Element"],
        edge_labels: &["IMPORTS", "STYLES", "BINDS_CLASS", "ROUTE", "DEFINES"],
        max_bytes: DEFAULT_CAP_BYTES,
        parser: Parser::Html,
        fixture: r#"<!DOCTYPE html>
<html>
<head>
    <link rel="stylesheet" href="assets/app.css">
    <script src="https://cdn.example.com/app.js"></script>
</head>
<body>
    <div id="header" class="card wide">Hello</div>
    <a href="/users">Users</a>
    <form action="/api/login" method="post">
        <input id="login" class="form-input">
    </form>
    <button hx-post="/api/save">Save</button>
</body>
</html>
"#,
        sample_rel: "",
    },
    LangSpec {
        name: "blade",
        matcher: Matcher::Predicate(is_blade_path),
        node_types: &["File", "Element"],
        edge_labels: &["IMPORTS", "STYLES", "BINDS_CLASS", "ROUTE", "DEFINES"],
        max_bytes: DEFAULT_CAP_BYTES,
        parser: Parser::Html,
        fixture: r#"@extends('layouts.app')

@section('content')
<div id="page" class="container">
    <h1 class="card">{{ $title }}</h1>
    <a href="/dashboard">Dashboard</a>
</div>
@endsection
"#,
        sample_rel: "resources/views/home.blade.php",
    },
    LangSpec {
        name: "css",
        matcher: Matcher::Ext(&["css", "scss"]),
        node_types: &["File", "CssClass", "CssId", "CssVar"],
        edge_labels: &["IMPORTS", "DEFINES"],
        max_bytes: DEFAULT_CAP_BYTES,
        parser: Parser::Css,
        fixture: r#"@import "variables.css";

:root {
    --main-color: #333;
}

.card {
    color: var(--main-color);
}

#sidebar {
    border: 1px solid;
}

@media (max-width: 600px) {
    .card { padding: 4px; }
}
"#,
        sample_rel: "",
    },
    LangSpec {
        name: "jupyter",
        matcher: Matcher::Ext(&["ipynb"]),
        node_types: &["File", "Notebook", "Class", "Function"],
        edge_labels: &["IMPORTS", "DEFINES", "INHERITS", "QUERIES_TABLE", "ROUTE"],
        max_bytes: NOTEBOOK_CAP_BYTES,
        parser: Parser::Notebook,
        fixture: r##"{
 "cells": [
  {
   "cell_type": "markdown",
   "source": ["# Заголовок\n"]
  },
  {
   "cell_type": "code",
   "source": ["import os\n", "def load(path):\n", "    return os.path.exists(path)\n"],
   "outputs": ["<base64-image-placeholder>"]
  },
  {
   "cell_type": "code",
   "source": ["class Repo(Base):\n", "    def get(self, id):\n", "        return None\n"],
   "outputs": []
  }
 ],
 "metadata": {},
 "nbformat": 4
}
"##,
        sample_rel: "",
    },
    LangSpec {
        name: "solidity",
        matcher: Matcher::Ext(&["sol"]),
        node_types: &["File", "Class", "Interface", "Module", "Function", "Mapping"],
        edge_labels: &["IMPORTS", "DEFINES", "INHERITS", "IMPLEMENTS", "USES_SCHEMA"],
        max_bytes: DEFAULT_CAP_BYTES,
        parser: Parser::Solidity,
        fixture: r#"// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import "./IToken.sol";

using SafeMath for uint256;

contract Token is IToken, Ownable {
    mapping(address => uint256) public balances;

    event Transfer(address indexed from, address indexed to, uint256 value);

    constructor() {
        balances[msg.sender] = 1;
    }

    function transfer(address to, uint256 amount) public {
        emit Transfer(msg.sender, to, amount);
    }
}
"#,
        sample_rel: "",
    },
    LangSpec {
        name: "zig",
        matcher: Matcher::Ext(&["zig"]),
        node_types: &["File", "Function", "Struct", "Variable"],
        edge_labels: &["IMPORTS", "DEFINES"],
        max_bytes: DEFAULT_CAP_BYTES,
        parser: Parser::Zig,
        fixture: r#"const std = @import("std");
const mod = @import("mod.zig");

pub const version: i32 = 1;

pub const Point = struct {
    x: i32,
    y: i32,
};

pub fn main() !void {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
}

export fn compute(a: i32) i32 {
    return a * 2;
}

test "compute doubles" {
    try std.testing.expectEqual(@as(i32, 4), compute(2));
}
"#,
        sample_rel: "",
    },
    LangSpec {
        name: "proto",
        matcher: Matcher::Ext(&["proto"]),
        node_types: &["File", "Module", "Service", "Function", "Struct", "Enum"],
        edge_labels: &["IMPORTS", "DEFINES", "USES_SCHEMA"],
        max_bytes: DEFAULT_CAP_BYTES,
        parser: Parser::Proto,
        fixture: r#"syntax = "proto3";

package demo.v1;

import "google/protobuf/timestamp.proto";

option (version) = "1.2.3";

message User {
    string name = 1;
    int64 created_at = 2;
}

enum Status {
    STATUS_UNSPECIFIED = 0;
    STATUS_ACTIVE = 1;
}

service UserService {
    rpc GetUser(GetUserRequest) returns (User);
}
"#,
        sample_rel: "",
    },
    LangSpec {
        name: "graphql",
        matcher: Matcher::Ext(&["graphql", "gql"]),
        node_types: &["File", "Schema", "Struct", "Interface", "Enum", "Function"],
        edge_labels: &["DEFINES"],
        max_bytes: DEFAULT_CAP_BYTES,
        parser: Parser::Graphql,
        fixture: r#"schema {
    query: Query
}

type Query {
    users(limit: Int): [User]
    user(id: ID!): User
}

type Mutation {
    createUser(name: String!): User
}

type User {
    id: ID!
    name: String!
}

interface Node {
    id: ID!
}

enum Role {
    ADMIN
    USER
}
"#,
        sample_rel: "",
    },
    LangSpec {
        name: "openapi",
        matcher: Matcher::FileName(&[
            "openapi.json",
            "openapi.yaml",
            "openapi.yml",
            "swagger.json",
            "swagger.yaml",
        ]),
        node_types: &["File", "Endpoint", "Route", "Struct", "Field"],
        edge_labels: &["DEFINES", "ROUTE", "SERVES_ENDPOINT", "USES_SCHEMA"],
        max_bytes: DEFAULT_CAP_BYTES,
        parser: Parser::OpenApi,
        fixture: r##"{
  "openapi": "3.0.0",
  "info": { "title": "Demo API", "version": "1.0.0" },
  "paths": {
    "/api/users": {
      "get": {
        "summary": "Список пользователей",
        "parameters": [{ "name": "limit", "in": "query", "schema": { "type": "integer" } }]
      },
      "post": {
        "requestBody": { "$ref": "#/components/schemas/NewUser" }
      }
    }
  },
  "components": {
    "schemas": {
      "NewUser": {
        "type": "object",
        "properties": {
          "profile": { "$ref": "#/components/schemas/Profile" }
        }
      },
      "Profile": { "type": "object" }
    }
  }
}
"##,
        sample_rel: "",
    },
    LangSpec {
        name: "terraform",
        matcher: Matcher::Ext(&["tf", "hcl"]),
        node_types: &["File", "Resource", "DataSource", "Module", "Variable", "Output"],
        edge_labels: &["DEFINES", "DEPENDS_ON"],
        max_bytes: DEFAULT_CAP_BYTES,
        parser: Parser::Terraform,
        fixture: r#"terraform {
    backend "s3" {}
}

variable "region" {
    default = "eu-north1"
}

output "endpoint" {
    value = "https://example.internal"
}

resource "yandex_compute_instance" "app" {
    name = "app-server"
}

module "network" {
    source = "./modules/vpc"
}

resource "yandex_vpc_subnet" "subnet" {
    depends_on = [yandex_compute_instance.app]
}
"#,
        sample_rel: "",
    },
    LangSpec {
        name: "dockerfile",
        matcher: Matcher::Predicate(is_dockerfile_path),
        node_types: &["File", "BaseImage", "EnvVar"],
        edge_labels: &["DEFINES", "DEPENDS_ON"],
        max_bytes: DEFAULT_CAP_BYTES,
        parser: Parser::Dockerfile,
        fixture: r#"FROM node:20-alpine AS builder
ARG NODE_ENV=production
ENV APP_HOME=/app
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci
COPY . .
EXPOSE 3000
CMD ["node", "server.js"]

FROM nginx:1.27
COPY --from=builder /app/dist /usr/share/nginx/html
"#,
        sample_rel: "Dockerfile",
    },
    LangSpec {
        name: "compose",
        matcher: Matcher::FileName(&[
            "docker-compose.yml",
            "docker-compose.yaml",
            "compose.yaml",
            "compose.yml",
        ]),
        node_types: &["File", "Service", "BaseImage", "EnvVar", "Port", "Volume", "Network"],
        edge_labels: &["DEFINES", "DEPENDS_ON", "MOUNTS"],
        max_bytes: DEFAULT_CAP_BYTES,
        parser: Parser::Compose,
        fixture: r#"services:
  api:
    image: ghcr.io/example/api:1.4.2
    build:
      context: ./api
    ports:
      - "8080:3000"
    environment:
      - RUST_LOG=info
      - DATABASE_URL=postgres://db/db
    depends_on:
      - db
    volumes:
      - ./data:/data
    networks:
      - backend
  db:
    image: postgres:16
networks:
  backend: {}
"#,
        sample_rel: "compose.yaml",
    },
    LangSpec {
        name: "k8s",
        matcher: Matcher::Predicate(is_k8s_yaml_path),
        node_types: &["File", "K8sResource", "BaseImage"],
        edge_labels: &["DEFINES", "DEPENDS_ON", "BINDS_SELECTOR"],
        max_bytes: DEFAULT_CAP_BYTES,
        parser: Parser::K8s,
        fixture: r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: api
  labels:
    app: api
spec:
  selector:
    matchLabels:
      app: api
  template:
    spec:
      containers:
        - image: ghcr.io/example/api:1.4.2
          envFrom:
            - configMapRef:
                name: api-config
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: api-config
  labels:
    app: api
data:
  LOG_LEVEL: info
"#,
        sample_rel: "k8s/app.yaml",
    },
    LangSpec {
        name: "manifest-cargo",
        matcher: Matcher::FileName(&["Cargo.toml"]),
        node_types: &["File", "ExternalPackage"],
        edge_labels: &["DEFINES", "DEPENDS_ON"],
        max_bytes: DEFAULT_CAP_BYTES,
        parser: Parser::Manifest,
        fixture: r#"[package]
name = "demo"
version = "0.1.0"

[dependencies]
serde_json = "1.0"
local-crate = { path = "../local-crate" }
git-crate = { git = "https://github.com/x/y" }

[dev-dependencies]
tempfile = "^3.10"
"#,
        sample_rel: "Cargo.toml",
    },
    LangSpec {
        name: "manifest-npm",
        matcher: Matcher::FileName(&["package.json"]),
        node_types: &["File", "ExternalPackage", "Script"],
        edge_labels: &["DEFINES", "DEPENDS_ON"],
        max_bytes: DEFAULT_CAP_BYTES,
        parser: Parser::Manifest,
        fixture: r#"{
  "name": "demo",
  "scripts": {
    "build": "vite build",
    "test": "vitest run"
  },
  "dependencies": {
    "react": "^18.2.0",
    "axios": "1.7.0"
  },
  "devDependencies": {
    "typescript": "~5.5.0"
  }
}
"#,
        sample_rel: "package.json",
    },
    LangSpec {
        name: "manifest-pyproject",
        matcher: Matcher::FileName(&["pyproject.toml"]),
        node_types: &["File", "ExternalPackage"],
        edge_labels: &["DEFINES", "DEPENDS_ON"],
        max_bytes: DEFAULT_CAP_BYTES,
        parser: Parser::Manifest,
        fixture: r#"[project]
name = "demo"
dependencies = ["requests>=2.31", "pydantic==2.7.0"]

[tool.poetry.dependencies]
fastapi = "^0.111"

[dependency-groups]
dev = ["pytest>=8.0"]
"#,
        sample_rel: "pyproject.toml",
    },
    LangSpec {
        name: "manifest-requirements",
        matcher: Matcher::FileName(&["requirements.txt"]),
        node_types: &["File", "ExternalPackage"],
        edge_labels: &["DEPENDS_ON"],
        max_bytes: DEFAULT_CAP_BYTES,
        parser: Parser::Manifest,
        fixture: r#"# комментарий
requests==2.31.0
flask>=3.0
-r other.txt
gunicorn
"#,
        sample_rel: "requirements.txt",
    },
    LangSpec {
        name: "manifest-gomod",
        matcher: Matcher::FileName(&["go.mod"]),
        node_types: &["File", "ExternalPackage"],
        edge_labels: &["DEPENDS_ON"],
        max_bytes: DEFAULT_CAP_BYTES,
        parser: Parser::Manifest,
        fixture: r#"module example.com/demo

go 1.22

require (
    github.com/pkg/errors v0.9.1
    golang.org/x/net v0.28.0 // indirect
)

require gopkg.in/yaml.v3 v3.0.1
"#,
        sample_rel: "go.mod",
    },
    LangSpec {
        name: "manifest-pubspec",
        matcher: Matcher::FileName(&["pubspec.yaml", "pubspec.yml"]),
        node_types: &["File", "ExternalPackage"],
        edge_labels: &["DEPENDS_ON"],
        max_bytes: DEFAULT_CAP_BYTES,
        parser: Parser::Manifest,
        fixture: r#"name: demo

dependencies:
  http: ^1.2.0
  intl: 0.19.0

dev_dependencies:
  test: ^1.25.0
"#,
        sample_rel: "pubspec.yaml",
    },
    LangSpec {
        name: "manifest-composer",
        matcher: Matcher::FileName(&["composer.json"]),
        node_types: &["File", "ExternalPackage"],
        edge_labels: &["DEPENDS_ON"],
        max_bytes: DEFAULT_CAP_BYTES,
        parser: Parser::Manifest,
        fixture: r#"{
  "name": "vendor/demo",
  "require": {
    "laravel/framework": "^12.0",
    "guzzlehttp/guzzle": "7.8.0"
  },
  "require-dev": {
    "phpunit/phpunit": "^11.0"
  }
}
"#,
        sample_rel: "composer.json",
    },
    LangSpec {
        name: "php",
        matcher: Matcher::Ext(&["php"]),
        node_types: &["File", "Class", "Interface", "Trait", "Function"],
        edge_labels: &["IMPORTS", "DEFINES", "INHERITS", "IMPLEMENTS"],
        max_bytes: NO_CAP,
        parser: Parser::Php,
        fixture: r"<?php
namespace App\Models;

use App\Support\Repo;

class UserRepository extends Base implements Repo {
}

interface Repo {
}

trait Timestamps {
}

function helper() {
}
",
        sample_rel: "",
    },
    LangSpec {
        name: "dart",
        matcher: Matcher::Ext(&["dart"]),
        node_types: &["File", "Class", "Trait", "Function"],
        edge_labels: &["IMPORTS", "DEFINES", "INHERITS", "IMPLEMENTS"],
        max_bytes: NO_CAP,
        parser: Parser::Dart,
        fixture: r"import 'dart:async';

class UserService extends Base with Mix implements Repo {
}

mixin Mix {
}

Future<void> load() async {
}
",
        sample_rel: "",
    },
    LangSpec {
        name: "java",
        matcher: Matcher::Ext(&["java"]),
        node_types: &["File", "Class", "Interface", "Function"],
        edge_labels: &["IMPORTS", "DEFINES", "INHERITS", "IMPLEMENTS"],
        max_bytes: NO_CAP,
        parser: Parser::Java,
        fixture: r"package com.example;

import java.util.List;

public class UserService extends Base implements Repo {
}

public interface Repo {
    public void load();
}
",
        sample_rel: "",
    },
    LangSpec {
        name: "csharp",
        matcher: Matcher::Ext(&["cs"]),
        node_types: &["File", "Class", "Interface", "Function"],
        edge_labels: &["IMPORTS", "DEFINES", "INHERITS"],
        max_bytes: NO_CAP,
        parser: Parser::CSharp,
        fixture: r"using System;

namespace App {
    public class UserService : Base {
    }

    public interface Repo {
    }

    public void Load() {
    }
}
",
        sample_rel: "",
    },
    LangSpec {
        name: "c-cpp",
        matcher: Matcher::Ext(&["c", "cpp", "cc", "cxx", "h", "hpp", "hxx"]),
        node_types: &["File", "Class", "Struct", "Function"],
        edge_labels: &["IMPORTS", "DEFINES", "INHERITS"],
        max_bytes: NO_CAP,
        parser: Parser::CCpp,
        fixture: r"#include <stdio.h>

struct Row {
    int id;
};

int get(int id) {
    return id;
}
",
        sample_rel: "",
    },
    LangSpec {
        name: "kotlin",
        matcher: Matcher::Ext(&["kt", "kts"]),
        node_types: &["File", "Class", "Interface", "Function"],
        edge_labels: &["IMPORTS", "DEFINES", "INHERITS"],
        max_bytes: NO_CAP,
        parser: Parser::Kotlin,
        fixture: r"import com.example.Repo

class UserService : Base() {
}

interface Repo {
}

fun load(): String {
    return x
}
",
        sample_rel: "",
    },
    LangSpec {
        name: "swift",
        matcher: Matcher::Ext(&["swift"]),
        node_types: &["File", "Class", "Interface", "Function"],
        edge_labels: &["IMPORTS", "DEFINES", "INHERITS"],
        max_bytes: NO_CAP,
        parser: Parser::Swift,
        fixture: r"import Foundation

class UserService: Base {
}

protocol Repo {
}

func load() -> String {
    return x
}
",
        sample_rel: "",
    },
    LangSpec {
        name: "ruby",
        matcher: Matcher::Ext(&["rb"]),
        node_types: &["File", "Class", "Module", "Function"],
        edge_labels: &["IMPORTS", "DEFINES", "INHERITS"],
        max_bytes: NO_CAP,
        parser: Parser::Ruby,
        fixture: r"require 'json'

module Store
  class Row < Base
    def get(id)
      nil
    end
  end
end
",
        sample_rel: "",
    },
    LangSpec {
        name: "sfc",
        matcher: Matcher::Ext(&["vue", "svelte"]),
        node_types: &["File", "Class", "Interface", "Function", "CssClass", "CssId", "CssVar"],
        edge_labels: &["IMPORTS", "DEFINES", "STYLES", "BINDS_CLASS"],
        max_bytes: NO_CAP,
        parser: Parser::Sfc,
        fixture: r#"<template>
  <div class="card">
    <CardTitle />
  </div>
</template>

<script>
import CardTitle from './CardTitle.vue';

export default {
    data() {
        return { title: "x" };
        sample_rel: "",
    },
};
</script>

<style>
.card { color: red; }
</style>
"#,
        sample_rel: "",
    },
    LangSpec {
        name: "scala",
        matcher: Matcher::Ext(&["scala"]),
        node_types: &["File", "Class", "Trait", "Function"],
        edge_labels: &["IMPORTS", "DEFINES", "INHERITS", "IMPLEMENTS"],
        max_bytes: NO_CAP,
        parser: Parser::Scala,
        fixture: r"import scala.collection.mutable

class UserService extends Base with Repo {
}

trait Repo {
}

def load(): Unit = {
}
",
        sample_rel: "",
    },
    LangSpec {
        name: "lua",
        matcher: Matcher::Ext(&["lua"]),
        node_types: &["File", "Function"],
        edge_labels: &["IMPORTS", "DEFINES"],
        max_bytes: NO_CAP,
        parser: Parser::Lua,
        fixture: r"local store = require('store')

local function get(id)
  return nil
end

function M.put(id, row)
  return true
end
",
        sample_rel: "",
    },
    LangSpec {
        name: "elixir",
        matcher: Matcher::Ext(&["ex", "exs"]),
        node_types: &["File", "Module", "Interface", "Function"],
        edge_labels: &["IMPORTS", "DEFINES"],
        max_bytes: NO_CAP,
        parser: Parser::Elixir,
        fixture: r"defmodule Store do
  alias Store.Row

  def get(id) do
    nil
  end
end

defprotocol Repo do
end
",
        sample_rel: "",
    },
    LangSpec {
        name: "shell",
        matcher: Matcher::Ext(&["sh", "bash"]),
        node_types: &["File", "Function"],
        edge_labels: &["IMPORTS", "DEFINES"],
        max_bytes: NO_CAP,
        parser: Parser::Shell,
        fixture: r#"#!/usr/bin/env bash
source ./lib.sh

get() {
  echo "$1"
}

function put {
  echo "$2"
}
"#,
        sample_rel: "",
    },
];

impl LangSpec {
    /// Единая точка «какой язык/формат у пути» (Ф47.1). Вход — путь проекта
    /// (относительный или абсолютный, разделители любые). Расширение — в нижнем
    /// регистре, имя файла — без учёта регистра (`Dockerfile`/`dockerfile`).
    pub fn for_path(rel_path: &str) -> Option<&'static LangSpec> {
        let norm = rel_path.replace('\\', "/");
        let file_name = norm.rsplit('/').next().unwrap_or(&norm);
        // Как у Path::extension: ведущая точка ("Dockerfile", ".gitignore") — не расширение
        let ext = match file_name.rsplit_once('.') {
            Some((stem, e)) if !stem.is_empty() => Some(e.to_lowercase()),
            _ => None,
        };
        LANGS.iter().find(|spec| match spec.matcher {
            Matcher::Ext(exts) => ext.as_deref().is_some_and(|e| exts.contains(&e)),
            Matcher::FileName(names) => names.iter().any(|n| file_name.eq_ignore_ascii_case(n)),
            Matcher::Predicate(f) => f(&norm),
        })
    }

    /// Путь-образец для теста фикстуры: явный `sample_rel`, иначе выводится
    /// из matcher.
    pub fn sample_rel_path(&self) -> Option<String> {
        if !self.sample_rel.is_empty() {
            return Some(self.sample_rel.to_string());
        }
        match self.matcher {
            Matcher::Ext(exts) => Some(format!("sample.{}", exts[0])),
            Matcher::FileName(names) => Some(names[0].to_string()),
            Matcher::Predicate(_) => None,
        }
    }
}

/// Путь содержит исключаемый каталог на любом уровне (`target/x.rs` → true).
/// Обходчик `ignore` не обрезает поддерево по имени каталога — проверяется
/// относительный путь целиком (раньше это делал второй list-чек в collect_files).
fn rel_has_skip_dir(rel: &str) -> bool {
    rel.split('/').any(|c| SKIP_DIRS.contains(&c))
}

/// Ф47.3 (ADR-AST-6): тип узла для авто-создания цели ребра в scan_project по
/// префиксу node_id. Единая таблица вместо if/else; расширяется фазами Ф48–Ф52
/// (сейчас реально встречаются `route:`, `table:`, `file:`, `css:` и заглушки
/// Ф50/Ф51).
pub const NODE_TYPE_BY_PREFIX: &[(&str, &str)] = &[
    ("route:", "Route"),
    ("table:", "Table"),
    ("file:", "File"),
    ("css:", "Stylesheet"),
    ("service:", "Service"),
    ("svc:", "Service"),
    ("endpoint:", "Endpoint"),
    ("contract:", "Contract"),
    ("schema:", "Schema"),
    ("pkg:", "ExternalPackage"),
    ("cssclass:", "CssClass"),
    ("cssid:", "CssId"),
    ("cssvar:", "CssVar"),
    ("resource:", "Resource"),
    ("k8s:", "K8sResource"),
    ("image:", "BaseImage"),
    ("el:", "Element"),
];

/// Тип и метка авто-создаваемого узла-цели по node_id (None → ExternalModule).
pub fn node_type_for_prefix(node_id: &str) -> Option<(&'static str, String)> {
    NODE_TYPE_BY_PREFIX
        .iter()
        .find_map(|(p, t)| node_id.strip_prefix(p).map(|rest| (*t, rest.to_string())))
}

/// Ф51.3: нормализация внешнего модуля импорта в id пакета (`pkg:<eco>:<name>`).
/// Внутренние/относительные пути → None (остаются module:-узлами).
pub fn package_of_module(lang: &str, module: &str) -> Option<String> {
    let m = module.trim();
    if m.is_empty() {
        return None;
    }
    match lang {
        "rust" => {
            if m.starts_with("crate::") || m.starts_with("self::") || m.starts_with("super::") {
                return None;
            }
            let first = m.split("::").next()?;
            Some(format!("pkg:rust:{}", first))
        }
        "python" => {
            if m.starts_with('.') {
                return None;
            }
            let first = m.split('.').next()?;
            Some(format!("pkg:pypi:{}", first))
        }
        "ts-js" | "sfc" => {
            if m.starts_with('.') || m.starts_with('/') {
                return None;
            }
            if let Some(rest) = m.strip_prefix('@') {
                // scope сохраняется: @scope/pkg → pkg:npm:@scope
                let (scope, _) = rest.split_once('/')?;
                Some(format!("pkg:npm:@{}", scope))
            } else {
                let first = m.split('/').next()?;
                Some(format!("pkg:npm:{}", first))
            }
        }
        "go" => {
            if m.contains("://") {
                None
            } else {
                Some(format!("pkg:go:{}", m.trim_matches('"')))
            }
        }
        "php" => {
            let parts: Vec<&str> = m.split('\\').filter(|p| !p.is_empty()).collect();
            if parts.len() < 2 {
                return None; // vendor\package минимум
            }
            Some(format!(
                "pkg:composer:{}/{}",
                parts[0].to_lowercase(),
                parts[1].to_lowercase()
            ))
        }
        "dart" => {
            let rest = m.strip_prefix("package:")?;
            let first = rest.split('/').next()?;
            Some(format!("pkg:pub:{}", first))
        }
        _ => None,
    }
}

/// Ф51.3: слияние внешних импортов с pkg:-узлами манифестов — на этапе
/// scan_project (не в парсерах). IMPORTS-рёбра file → module:X переписываются
/// в file → pkg:<eco>:X по нормализации [`package_of_module`]; internal/relative
/// модули не трогаются. Возвращает число переписанных рёбер.
pub fn consolidate_external_imports(scan: &mut AstScanResult) -> usize {
    let mut rewritten = 0usize;
    for edge in scan.edges.iter_mut() {
        if edge.label != "IMPORTS" || !edge.source_node_id.starts_with("file:") {
            continue;
        }
        if !edge.target_node_id.starts_with("module:") {
            continue;
        }
        let module = edge.target_node_id.strip_prefix("module:").unwrap_or("");
        let rel_path = edge.source_node_id.strip_prefix("file:").unwrap_or("");
        let Some(spec) = LangSpec::for_path(rel_path) else {
            continue;
        };
        if let Some(pkg_id) = package_of_module(spec.name, module) {
            edge.target_node_id = pkg_id;
            rewritten += 1;
        }
    }
    rewritten
}

/// Cap размера файла по умолчанию — 2 МБ (ADR-AST-4); для `.ipynb` — свой cap (Ф52).
pub const DEFAULT_CAP_BYTES: usize = 2 * 1024 * 1024;

/// Cap для Jupyter-ноутбуков — 5 МБ (52.1: outputs не читаются, но сами
/// ноутбуки бывают велики из-за base64-выводов).
pub const NOTEBOOK_CAP_BYTES: usize = 5 * 1024 * 1024;

/// Laravel blade: `*.blade.php` — HTML-парсер раньше PHP-экстрактора (Ф48.1).
fn is_blade_path(rel_path: &str) -> bool {
    rel_path.ends_with(".blade.php")
}

/// Ф50.2: Dockerfile (`Dockerfile`, `*.Dockerfile`, `Dockerfile.*`).
fn is_dockerfile_path(rel_path: &str) -> bool {
    let name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    name == "Dockerfile" || name.ends_with(".Dockerfile") || name.starts_with("Dockerfile.")
}

/// Ф50.4: k8s/deploy/helm-каталоги с YAML (values.yaml/Chart.yaml — по
/// каталогу; случайный корневой yaml не матчится — классификатор 50.5).
fn is_k8s_yaml_path(rel_path: &str) -> bool {
    let p = rel_path.to_lowercase();
    let ext_ok = p.ends_with(".yaml") || p.ends_with(".yml");
    ext_ok
        && (p.contains("k8s/") || p.contains("kubernetes/") || p.contains("deploy/")
            || p.contains("helm/") || p.contains("charts/") || p.contains("chart/"))
}

/// Ф48.1: `<script src>` — внешняя ссылка (http/https/протокол-относительная) →
/// `module:` (ExternalModule), локальный путь со скриптовым расширением →
/// `file:`-узел (реальный файл, узел уточнится при его скане), прочее →
/// `module:`.
fn external_or_local_ref(src: &str, local_exts: &[&str]) -> String {
    let s = src.trim();
    if s.starts_with("http://") || s.starts_with("https://") || s.starts_with("//") {
        return format!("module:{}", s);
    }
    let ext = Path::new(s)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());
    if ext.is_some_and(|e| local_exts.contains(&e.as_str())) {
        format!("file:{}", s.trim_start_matches("./"))
    } else {
        format!("module:{}", s)
    }
}

/// Ф48.1/48.2: цель ребра STYLES/IMPORTS на стилевой файл. Расширение
/// стиля (.css/.scss/.sass/.less) → `file:` (реальный файл), иначе → `css:`
/// (Stylesheet-заглушка: «первое существующее» из `x`/`x.css`/`x.scss`
/// требует fs, которого у парсера нет — 48.2).
fn stylesheet_target(href: &str) -> String {
    let s = href.trim();
    if Path::new(s)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_lowercase().as_str(), "css" | "scss" | "sass" | "less"))
        .unwrap_or(false)
    {
        format!("file:{}", s.trim_start_matches("./"))
    } else {
        format!("css:{}", s)
    }
}

/// Ф48.1: `ROUTE`-ребро (INFERRED) из action/href/hx-*; только in-app пути
/// (`/...`), внешний URL и якоря — шум.
fn push_html_route(
    out: &mut AstScanResult,
    file_node_id: &str,
    method: &str,
    path: &str,
    seen: &mut HashSet<String>,
) {
    let p = path.trim();
    if !p.starts_with('/') || p.starts_with("//") {
        return;
    }
    let node_target = format!("route:{} {}", method.to_uppercase(), p);
    if !seen.insert(node_target.clone()) {
        return;
    }
    out.edges.push(AstEdge {
        source_node_id: file_node_id.to_string(),
        target_node_id: node_target,
        label: "ROUTE".to_string(),
        weight: 1.0,
        context: format!("{} {}", method.to_uppercase(), p),
        provenance: "INFERRED".to_string(),
    });
}

/// Ключевые слова, которые не считаются вызовами (Rust).
const RUST_CALL_SKIP: &[&str] = &[
    "if", "for", "while", "match", "loop", "return", "unsafe", "fn", "let", "struct", "impl",
    "mod", "use", "pub", "crate", "self", "super", "as", "in", "else", "await", "move", "box",
    "where", "type", "dyn", "ref", "const", "static", "enum", "trait",
];

/// Ключевые слова, которые не считаются вызовами (Python).
const PY_CALL_SKIP: &[&str] = &[
    "if", "for", "while", "return", "def", "class", "import", "from", "with", "elif", "else",
    "except", "lambda", "yield", "assert", "del", "in", "is", "not", "and", "or", "await", "async",
    "raise", "global", "nonlocal",
];

/// AST-экстрактор для проектов.
#[allow(dead_code)]
pub struct AstCodeExtractor {
    rust_fn_re: Regex,
    rust_struct_re: Regex,
    rust_trait_re: Regex,
    rust_impl_re: Regex,
    rust_use_re: Regex,

    py_class_re: Regex,
    py_fn_re: Regex,
    py_import_re: Regex,

    ts_class_re: Regex,
    ts_interface_re: Regex,
    ts_fn_re: Regex,
    ts_import_re: Regex,

    go_fn_re: Regex,
    go_struct_re: Regex,
    go_import_re: Regex,

    sql_table_re: Regex,
    sql_fk_re: Regex,

    php_use_re: Regex,
    php_class_re: Regex,
    php_interface_re: Regex,
    php_trait_re: Regex,
    php_fn_re: Regex,

    /// Ф39.1: общий паттерн места вызова `name(` (Rust/Python type-pass).
    call_re: Regex,

    /// Ф40.2: маршруты axum/actix `#[get("/path")]`.
    rust_route_re: Regex,
    /// Ф40.2: маршруты FastAPI/Flask `@app.get("/path")`.
    py_route_re: Regex,
    /// Ф40.2: SQLAlchemy `__tablename__ = "x"`.
    py_tablename_re: Regex,

    dart_import_re: Regex,
    dart_class_re: Regex,
    dart_mixin_re: Regex,
    dart_fn_re: Regex,

    java_package_re: Regex,
    java_import_re: Regex,
    java_class_re: Regex,
    java_interface_re: Regex,
    java_fn_re: Regex,

    cs_using_re: Regex,
    cs_type_re: Regex,
    cs_interface_re: Regex,
    cs_fn_re: Regex,

    cpp_include_re: Regex,
    cpp_class_re: Regex,
    cpp_struct_re: Regex,
    cpp_fn_re: Regex,

    kt_import_re: Regex,
    kt_class_re: Regex,
    kt_interface_re: Regex,
    kt_fn_re: Regex,

    sw_import_re: Regex,
    sw_class_re: Regex,
    sw_protocol_re: Regex,
    sw_fn_re: Regex,
    sw_init_re: Regex,

    rb_require_re: Regex,
    rb_module_re: Regex,
    rb_class_re: Regex,
    rb_fn_re: Regex,

    /// Vue/Svelte SFC: `<script>`-блок (тело переиспользует parse_ts_js).
    sfc_script_re: Regex,
    /// Ф48.3: `<style>`-блок SFC (тело переиспользует parse_css).
    sfc_style_re: Regex,
    /// Ф48.3: `<template>`-блок SFC (упрощённый HTML-разбор).
    sfc_template_re: Regex,

    /// Ф48.1: HTML — теги с атрибутами (кавычки внутри attrs не рвут матч).
    html_tag_re: Regex,
    /// Ф48.1: HTML — отдельный атрибут `name="value"`.
    html_attr_re: Regex,
    /// Ф48.1: HTML — комментарии (вырезаются до разбора).
    html_comment_re: Regex,
    /// Ф48.1/48.3: `class="a b"` в HTML/SFC-шаблонах (не `:class` — перед
    /// `class` требуется пробел).
    html_class_re: Regex,
    /// Ф48.3: PascalCase-компоненты в SFC-шаблоне (`<CardTitle`).
    sfc_component_re: Regex,

    /// Ф48.2: CSS — селекторные группы (`^|{` или `}` перед селектором;
    /// вложенность/@media покрываются `{`).
    css_selector_re: Regex,
    /// Ф48.2: @import "x" / @import url(x).
    css_import_re: Regex,
    /// Ф48.2: `--custom-prop:` (объявление, не `var(--x)` — использования без `:`).
    css_var_re: Regex,
    /// Токены классов/идентификаторов внутри текста селектора.
    css_sel_class_re: Regex,
    css_sel_id_re: Regex,

    /// Ф52.2: Solidity.
    sol_import_re: Regex,
    sol_contract_re: Regex,
    sol_interface_re: Regex,
    sol_library_re: Regex,
    sol_fn_re: Regex,
    sol_special_re: Regex,
    sol_mapping_re: Regex,
    sol_using_re: Regex,

    /// Ф52.3: Zig.
    zig_import_re: Regex,
    zig_fn_re: Regex,
    zig_test_re: Regex,
    zig_type_re: Regex,
    zig_pub_var_re: Regex,

    /// Ф49.1: Protocol Buffers.
    proto_package_re: Regex,
    proto_service_re: Regex,
    proto_rpc_re: Regex,
    proto_message_re: Regex,
    proto_enum_re: Regex,
    proto_import_re: Regex,
    proto_field_re: Regex,

    /// Ф49.2: GraphQL.
    gql_type_re: Regex,
    gql_interface_re: Regex,
    gql_enum_re: Regex,
    gql_schema_re: Regex,
    gql_query_block_re: Regex,
    /// Поле внутри Query/Mutation/Subscription-блока: `name(args): Type`.
    gql_field_re: Regex,

    /// Ф49.4б: клиентские HTTP-вызовы (ts/js: fetch/axios; py: requests).
    ts_http_re: Regex,
    py_http_re: Regex,
    /// Ф49.4в: строки GraphQL-запросов (gql`…` в ts/js, gql("""…""") в py).
    ts_gql_re: Regex,
    py_gql_re: Regex,
    /// Идентификаторы внутри тела GraphQL-запроса (кандидаты QUERIES_FIELD).
    gql_ident_re: Regex,

    /// Ф50.1: Terraform.
    tf_block_re: Regex,
    tf_source_re: Regex,
    tf_depends_on_re: Regex,

    /// Ф50.2: Dockerfile (line-based, без regex-полей кроме FROM/COPY).
    docker_from_re: Regex,
    docker_copy_re: Regex,
    docker_env_re: Regex,

    scala_import_re: Regex,
    scala_type_re: Regex,
    scala_fn_re: Regex,

    lua_require_re: Regex,
    lua_fn_re: Regex,

    ex_module_re: Regex,
    ex_protocol_re: Regex,
    ex_fn_re: Regex,
    ex_alias_re: Regex,

    sh_fn_paren_re: Regex,
    sh_fn_kw_re: Regex,
    sh_source_re: Regex,
}

impl Default for AstCodeExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl AstCodeExtractor {
    pub fn new() -> Self {
        Self {
            // Rust patterns
            rust_fn_re: Regex::new(r"(?m)^\s*(?:pub(?:\([^)]+\))?\s+)?(?:async\s+)?fn\s+([a-zA-Z0-9_]+)\s*(?:<[^>]+>)?\s*\(([^)]*)\)(?:\s*->\s*([^{]+))?").unwrap(),
            rust_struct_re: Regex::new(r"(?m)^\s*(?:pub(?:\([^)]+\))?\s+)?(?:struct|enum)\s+([a-zA-Z0-9_]+)").unwrap(),
            rust_trait_re: Regex::new(r"(?m)^\s*(?:pub(?:\([^)]+\))?\s+)?trait\s+([a-zA-Z0-9_]+)").unwrap(),
            rust_impl_re: Regex::new(r"(?m)^\s*impl(?:<[^>]+>)?\s+(?:([a-zA-Z0-9_:]+)\s+for\s+)?([a-zA-Z0-9_:]+)").unwrap(),
            rust_use_re: Regex::new(r"(?m)^\s*use\s+([^;]+);").unwrap(),

            // Ф39.1: место вызова `name(` (для type-pass)
            call_re: Regex::new(r"([a-zA-Z_][a-zA-Z0-9_]*)\s*\(").unwrap(),

            // Ф40.2: framework-эвристики (ROUTE / QUERIES_TABLE, provenance=INFERRED)
            rust_route_re: Regex::new(r#"#\[\s*(get|post|put|delete|patch|route)\s*\(\s*"([^"]+)""#).unwrap(),
            py_route_re: Regex::new(r#"(?:app|router|api)\.(get|post|put|delete|patch)\s*\(\s*["']([^"']+)["']"#).unwrap(),
            py_tablename_re: Regex::new(r#"__tablename__\s*=\s*["'](\w+)["']"#).unwrap(),

            // Python patterns
            py_class_re: Regex::new(r"(?m)^\s*class\s+([a-zA-Z0-9_]+)(?:\(([^)]*)\))?:").unwrap(),
            py_fn_re: Regex::new(r"(?m)^\s*(?:async\s+)?def\s+([a-zA-Z0-9_]+)\s*\(([^)]*)\)(?:\s*->\s*([^:]+))?:").unwrap(),
            py_import_re: Regex::new(r"(?m)^\s*(?:from\s+([a-zA-Z0-9_.]+)\s+import\s+([^#\n]+)|import\s+([^#\n]+))").unwrap(),

            // TS/JS patterns
            ts_class_re: Regex::new(r"(?m)^\s*(?:export\s+)?(?:default\s+)?class\s+([a-zA-Z0-9_]+)(?:\s+extends\s+([a-zA-Z0-9_]+))?(?:\s+implements\s+([a-zA-Z0-9_, ]+))?").unwrap(),
            ts_interface_re: Regex::new(r"(?m)^\s*(?:export\s+)?(?:interface|type)\s+([a-zA-Z0-9_]+)").unwrap(),
            ts_fn_re: Regex::new(r"(?m)^\s*(?:export\s+)?(?:async\s+)?(?:function\s+([a-zA-Z0-9_]+)|(?:const|let|var)\s+([a-zA-Z0-9_]+)\s*=\s*(?:async\s+)?\([^)]*\)\s*=>)").unwrap(),
            ts_import_re: Regex::new(r#"(?m)^\s*import\s+(?:(?:\{([^}]+)\}|\*\s+as\s+([a-zA-Z0-9_]+)|([a-zA-Z0-9_]+))\s+from\s+)?['"]([^'"]+)['"]"#).unwrap(),

            // Go patterns
            go_fn_re: Regex::new(r"(?m)^\s*func\s+(?:\((?:[^)]+)\)\s+)?([a-zA-Z0-9_]+)\s*\(").unwrap(),
            go_struct_re: Regex::new(r"(?m)^\s*type\s+([a-zA-Z0-9_]+)\s+(?:struct|interface)").unwrap(),
            go_import_re: Regex::new(r#"(?m)^\s*(?:import\s+['"]([^'"]+)['"]|import\s*\(([^)]+)\))"#).unwrap(),

            // SQL patterns
            sql_table_re: Regex::new(r"(?i)CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?([a-zA-Z0-9_`\[\]]+)").unwrap(),
            sql_fk_re: Regex::new(r"(?i)REFERENCES\s+([a-zA-Z0-9_`\[\]]+)\s*\(([a-zA-Z0-9_`\[\]]+)\)").unwrap(),

            // PHP patterns
            php_use_re: Regex::new(r"(?m)^\s*use\s+(?:function\s+|const\s+)?([a-zA-Z0-9_\\]+)(?:\s+as\s+([a-zA-Z0-9_]+))?;").unwrap(),
            php_class_re: Regex::new(r"(?m)^\s*(?:(?:final|abstract|readonly)\s+)*class\s+([a-zA-Z0-9_]+)(?:\s+extends\s+([a-zA-Z0-9_\\]+))?(?:\s+implements\s+([a-zA-Z0-9_\\,\s]+))?").unwrap(),
            php_interface_re: Regex::new(r"(?m)^\s*interface\s+([a-zA-Z0-9_]+)(?:\s+extends\s+([a-zA-Z0-9_\\,\s]+))?").unwrap(),
            php_trait_re: Regex::new(r"(?m)^\s*trait\s+([a-zA-Z0-9_]+)").unwrap(),
            php_fn_re: Regex::new(r"(?m)^\s*(?:(?:public|protected|private|static|final|abstract)\s+)*function\s+([a-zA-Z0-9_]+)\s*\(([^)]*)\)").unwrap(),

            // Dart patterns
            dart_import_re: Regex::new(r#"(?m)^\s*(?:import|export)\s+['"]([^'"]+)['"](?:\s+as\s+([a-zA-Z0-9_]+))?"#).unwrap(),
            dart_class_re: Regex::new(r"(?m)^\s*(?:(?:abstract|base|final|interface|sealed)\s+)*class\s+([a-zA-Z0-9_]+)(?:<[^>]+>)?(?:\s+extends\s+([a-zA-Z0-9_]+)(?:<[^>]+>)?)?(?:\s+with\s+([a-zA-Z0-9_,\s]+))?(?:\s+implements\s+([a-zA-Z0-9_,\s]+))?").unwrap(),
            dart_mixin_re: Regex::new(r"(?m)^\s*mixin\s+([a-zA-Z0-9_]+)(?:<[^>]+>)?(?:\s+on\s+([a-zA-Z0-9_,\s]+))?").unwrap(),
            dart_fn_re: Regex::new(r"(?m)^\s*(?:(?:static|async|void|[a-zA-Z0-9_<>?]+)\s+)+([a-zA-Z0-9_]+)\s*\(([^)]*)\)\s*(?:async\*?|=>|\{)").unwrap(),

            // Java patterns
            java_package_re: Regex::new(r"(?m)^\s*package\s+([a-zA-Z0-9_.]+);").unwrap(),
            java_import_re: Regex::new(r"(?m)^\s*import\s+(?:static\s+)?([a-zA-Z0-9_.*]+);").unwrap(),
            java_class_re: Regex::new(r"(?m)^\s*(?:(?:public|protected|private|static|final|abstract|sealed|non-sealed)\s+)*(?:class|enum|record)\s+([a-zA-Z0-9_]+)(?:<[^>]+>)?(?:\s+extends\s+([a-zA-Z0-9_.]+)(?:<[^>]+>)?)?(?:\s+implements\s+([a-zA-Z0-9_.,\s]+))?").unwrap(),
            java_interface_re: Regex::new(r"(?m)^\s*(?:(?:public|protected|private|static|sealed|non-sealed)\s+)*interface\s+([a-zA-Z0-9_]+)(?:<[^>]+>)?(?:\s+extends\s+([a-zA-Z0-9_.,\s]+))?").unwrap(),
            java_fn_re: Regex::new(r"(?m)^\s*(?:(?:public|protected|private|static|final|abstract|synchronized|native|default)\s+)+(?:<[^>]+>\s+)?([a-zA-Z0-9_<>\[\]]+)\s+([a-zA-Z0-9_]+)\s*\(([^)]*)\)").unwrap(),

            // C# patterns
            cs_using_re: Regex::new(r#"(?m)^\s*using\s+(?:static\s+)?([a-zA-Z0-9_.]+)\s*(?:=[^;]*)?;"#).unwrap(),
            cs_type_re: Regex::new(r"(?m)^\s*(?:(?:public|private|protected|internal|static|sealed|abstract|partial|readonly|ref|file)\s+)*(?:class|record(?:\s+(?:class|struct))?|struct|enum)\s+([a-zA-Z0-9_]+)(?:<[^>]+>)?(?:\s*:\s*([^{;]+))?").unwrap(),
            cs_interface_re: Regex::new(r"(?m)^\s*(?:(?:public|private|protected|internal|file)\s+)*interface\s+([a-zA-Z0-9_]+)(?:<[^>]+>)?(?:\s*:\s*([^{;]+))?").unwrap(),
            cs_fn_re: Regex::new(r"(?m)^\s*(?:(?:public|private|protected|internal|static|virtual|override|sealed|abstract|async|extern|unsafe|new|partial|required)\s+)+(?:<[^>]+>\s+)?([a-zA-Z0-9_<>\[\]?,]+)\s+([a-zA-Z0-9_]+)\s*\(([^)]*)\)").unwrap(),

            // C/C++ patterns
            cpp_include_re: Regex::new(r#"(?m)^\s*#\s*include\s+[<"]([^">]+)[">]"#).unwrap(),
            cpp_class_re: Regex::new(r"(?m)^\s*(?:template\s*<[^>]*>\s*)?class\s+([a-zA-Z0-9_]+)(?:\s*:\s*([^{;]+))?").unwrap(),
            cpp_struct_re: Regex::new(r"(?m)^\s*(?:template\s*<[^>]*>\s*)?struct\s+([a-zA-Z0-9_]+)(?:\s*:\s*([^{;]+))?").unwrap(),
            cpp_fn_re: Regex::new(r"(?m)^\s*(?:[a-zA-Z_][a-zA-Z0-9_:<>,\*&\s]*?\s+)?(?:[a-zA-Z_][a-zA-Z0-9_]*::)*([a-zA-Z_][a-zA-Z0-9_]*)\s*\([^;{]*\)\s*(?:const\s*)?(?:noexcept\s*)?(?:\s*:\s*[a-zA-Z0-9_:<>,\s&\*\(\)]+)?\{").unwrap(),

            // Kotlin patterns
            kt_import_re: Regex::new(r"(?m)^\s*import\s+([a-zA-Z0-9_.]+)(?:\s+as\s+([a-zA-Z0-9_]+))?").unwrap(),
            kt_class_re: Regex::new(r"(?m)^\s*(?:(?:public|private|protected|internal|open|final|abstract|sealed|data|value|inner|enum|annotation|fun)\s+)*(?:class|object)\s+([a-zA-Z0-9_]+)(?:<[^>]+>)?(?:\([^)]*\))?(?:\s*:\s*([^{]+))?").unwrap(),
            kt_interface_re: Regex::new(r"(?m)^\s*(?:(?:public|private|protected|internal|fun)\s+)*interface\s+([a-zA-Z0-9_]+)(?:<[^>]+>)?(?:\s*:\s*([^{]+))?").unwrap(),
            kt_fn_re: Regex::new(r"(?m)^\s*(?:(?:public|private|protected|internal|open|override|final|abstract|suspend|inline|operator|infix|external|tailrec|expect|actual|const)\s+)*fun\s+(?:<[^>]+>\s+)?(?:[a-zA-Z_][a-zA-Z0-9_]*\.)?([a-zA-Z_][a-zA-Z0-9_]*)\s*\(").unwrap(),

            // Swift patterns
            sw_import_re: Regex::new(r"(?m)^\s*(?:@testable\s+|@_exported\s+)?import\s+(?:class\s+|struct\s+|enum\s+|protocol\s+|func\s+|var\s+|let\s+|typealias\s+|extension\s+)?([a-zA-Z0-9_.]+)").unwrap(),
            sw_class_re: Regex::new(r"(?m)^\s*(?:(?:public|private|fileprivate|internal|open|final|@objc|@objcMembers|indirect|frozen)\s+)*(class|struct|enum|extension)\s+([a-zA-Z0-9_]+)(?:<[^>]+>)?(?:\s*:\s*([^{]+))?").unwrap(),
            sw_protocol_re: Regex::new(r"(?m)^\s*(?:(?:public|private|fileprivate|internal|open)\s+)*protocol\s+([a-zA-Z0-9_]+)(?:<[^>]+>)?(?:\s*:\s*([^{]+))?").unwrap(),
            sw_fn_re: Regex::new(r"(?m)^\s*(?:(?:public|private|fileprivate|internal|open|static|class|final|override|mutating|nonmutating|@discardableResult|required|convenience|@objc)\s+)*func\s+(?:<[^>]+>\s+)?([a-zA-Z_][a-zA-Z0-9_]*)\s*(?:<[^>]+>)?\(").unwrap(),
            sw_init_re: Regex::new(r"(?m)^\s*(?:(?:public|private|fileprivate|internal|open|override|required|convenience)\s+)*init(?:\?|!)?\s*(?:<[^>]+>)?\(").unwrap(),

            // Ruby patterns
            rb_require_re: Regex::new(r#"(?m)^\s*(?:require|require_relative)\s+['"]([^'"]+)['"]"#).unwrap(),
            rb_module_re: Regex::new(r"(?m)^\s*module\s+([A-Z][a-zA-Z0-9_]*)").unwrap(),
            rb_class_re: Regex::new(r"(?m)^\s*class\s+([A-Z][a-zA-Z0-9_]*(?:::[A-Z][a-zA-Z0-9_]*)*)\s*(?:<\s*([A-Za-z0-9_:.]+))?").unwrap(),
            rb_fn_re: Regex::new(r"(?m)^\s*def\s+(?:self\.)?([a-zA-Z_][a-zA-Z0-9_]*[?!]?)").unwrap(),

            // Vue/Svelte SFC (script-блоки прогоняются через parse_ts_js)
            sfc_script_re: Regex::new(r"(?is)<script[^>]*>(.*?)</script>").unwrap(),
            // Ф48.3: style/template блоки SFC (template — лениво до первого
            // `</template>`: вложенные <template #slot> обрезают хвост, осознанное
            // упрощение — классы всё равно собираются отдельным проходом)
            sfc_style_re: Regex::new(r"(?is)<style[^>]*>(.*?)</style>").unwrap(),
            sfc_template_re: Regex::new(r"(?is)<template[^>]*>(.*?)</template>").unwrap(),
            sfc_component_re: Regex::new(r"<([A-Z][A-Za-z0-9_]*)").unwrap(),

            // Ф48.1: HTML (attrs: кавыченные значения не содержат `>` внутри)
            html_tag_re: Regex::new(r#"(?is)<([a-zA-Z][a-zA-Z0-9-]*)((?:[^>"']|"[^"]*"|'[^']*')*)>"#).unwrap(),
            html_attr_re: Regex::new(r#"(?is)\s([a-zA-Z-]+)\s*=\s*("([^"]*)"|'([^']*)')"#).unwrap(),
            html_comment_re: Regex::new(r"(?s)<!--.*?-->").unwrap(),
            html_class_re: Regex::new(r#"(?is)\sclass\s*=\s*(?:"([^"]+)"|'([^']+)')"#).unwrap(),

            // Ф48.2: CSS
            css_selector_re: Regex::new(r"(?m)(?:^|[{}])\s*([^{}@;]+)\{").unwrap(),
            css_import_re: Regex::new(r#"(?is)@import\s+(?:url\s*\(\s*["']?([^"')]+?)["']?\s*\)|["']([^"']+)["'])"#).unwrap(),
            css_var_re: Regex::new(r"(?m)--([a-zA-Z0-9_-]+)\s*:").unwrap(),
            css_sel_class_re: Regex::new(r"\.([a-zA-Z_][a-zA-Z0-9_-]*)").unwrap(),
            css_sel_id_re: Regex::new(r"#([a-zA-Z_][a-zA-Z0-9_-]*)").unwrap(),

            // Ф52.2: Solidity
            sol_import_re: Regex::new(r#"(?m)^\s*import\s+["']([^"']+)["']"#).unwrap(),
            sol_contract_re: Regex::new(r"(?m)^\s*(?:abstract\s+)?contract\s+([A-Za-z0-9_]+)(?:\s+is\s+([^{]+))?").unwrap(),
            sol_interface_re: Regex::new(r"(?m)^\s*interface\s+([A-Za-z0-9_]+)(?:\s+is\s+([^{]+))?").unwrap(),
            sol_library_re: Regex::new(r"(?m)^\s*library\s+([A-Za-z0-9_]+)").unwrap(),
            sol_fn_re: Regex::new(r"(?m)^\s*(function)\s+([A-Za-z0-9_]+)\s*\(").unwrap(),
            sol_special_re: Regex::new(r"(?m)^\s*(constructor|modifier|event|error)\s*([A-Za-z0-9_]*)").unwrap(),
            sol_mapping_re: Regex::new(r"mapping\s*\([^)]*\)\s*(?:public|private|internal)?\s*([A-Za-z0-9_]+)\s*[;=]").unwrap(),
            sol_using_re: Regex::new(r"using\s+([A-Za-z0-9_]+)\s+for\s+([A-Za-z0-9_.\[\]]+)").unwrap(),

            // Ф52.3: Zig
            zig_import_re: Regex::new(r#"@import\("([^"]+)"\)"#).unwrap(),
            zig_fn_re: Regex::new(r"(?m)^\s*(?:(?:pub|export|inline)\s+)*fn\s+([A-Za-z0-9_]+)").unwrap(),
            zig_test_re: Regex::new(r#"(?m)^\s*test\s+"([^"]+)""#).unwrap(),
            zig_type_re: Regex::new(r"(?m)^\s*(?:pub\s+)?(?:const|var)\s+([A-Za-z0-9_]+)\s*=\s*(struct|enum|union|opaque)\b").unwrap(),
            zig_pub_var_re: Regex::new(r"(?m)^\s*pub\s+(?:const|var)\s+([A-Za-z0-9_]+)\s*(?::[^=]+)?\s*=").unwrap(),

            // Ф49.1: Protocol Buffers
            proto_package_re: Regex::new(r"(?m)^\s*package\s+([a-zA-Z0-9_.]+)\s*;").unwrap(),
            proto_service_re: Regex::new(r"(?m)^\s*service\s+([A-Za-z0-9_]+)\s*\{").unwrap(),
            proto_rpc_re: Regex::new(r"(?m)^\s*rpc\s+([A-Za-z0-9_]+)\s*\(").unwrap(),
            proto_message_re: Regex::new(r"(?m)^\s*message\s+([A-Za-z0-9_]+)\s*\{").unwrap(),
            proto_enum_re: Regex::new(r"(?m)^\s*enum\s+([A-Za-z0-9_]+)\s*\{").unwrap(),
            proto_import_re: Regex::new(r#"(?m)^\s*import\s+(?:public\s+)?["']([^"']+)["']\s*;"#).unwrap(),
            proto_field_re: Regex::new(r"(?m)^\s*(?:repeated|optional|required\s+)?([A-Za-z][A-Za-z0-9_.]*)\s+[a-z][a-z0-9_]*\s*=\s*\d+").unwrap(),

            // Ф49.2: GraphQL
            gql_type_re: Regex::new(r"(?m)^\s*(?:input|type)\s+([A-Za-z0-9_]+)").unwrap(),
            gql_interface_re: Regex::new(r"(?m)^\s*interface\s+([A-Za-z0-9_]+)").unwrap(),
            gql_enum_re: Regex::new(r"(?m)^\s*enum\s+([A-Za-z0-9_]+)").unwrap(),
            gql_schema_re: Regex::new(r"schema\s*\{").unwrap(),
            gql_query_block_re: Regex::new(r"(?is)(?:type|extend type)\s+(Query|Mutation|Subscription)\s*\{([^{}]*)\}").unwrap(),
            gql_field_re: Regex::new(r"^\s*([a-z][a-zA-Z0-9_]*)\s*(?:\([^)]*\))?\s*:").unwrap(),

            // Ф49.4: клиентские вызовы и gql-строки
            ts_http_re: Regex::new(r#"(?i)(?:fetch|axios(?:\.(get|post|put|delete|patch))?)\s*\(\s*["'`]([^"'`]+)["'`]"#).unwrap(),
            py_http_re: Regex::new(r#"requests\.(get|post|put|delete|patch)\s*\(\s*["']([^"']+)["']"#).unwrap(),
            ts_gql_re: Regex::new(r#"(?:gql|graphql)\s*`([^`]*)`"#).unwrap(),
            py_gql_re: Regex::new(r#"(?s)(?:gql|graphql)\s*\(\s*(?:"""(.*?)"""|'''(.*?)''')"#).unwrap(),
            gql_ident_re: Regex::new(r"([a-zA-Z_][a-zA-Z0-9_]*)\s*(?:\(|\{)").unwrap(),

            // Ф50.1: Terraform
            tf_block_re: Regex::new(r#"(?m)^\s*(resource|module|variable|output|data)\s+"([^"]+)"(?:\s+"([^"]+)")?"#).unwrap(),
            tf_source_re: Regex::new(r#"source\s*=\s*"([^"]+)""#).unwrap(),
            tf_depends_on_re: Regex::new(r#"(?s)depends_on\s*=\s*\[([^\]]*)\]"#).unwrap(),

            // Ф50.2: Dockerfile
            docker_from_re: Regex::new(r"(?im)^\s*FROM\s+(\S+)(?:\s+AS\s+\S+)?").unwrap(),
            docker_copy_re: Regex::new(r"(?im)^\s*(?:COPY|ADD)\s+(?:--[^\s]+\s+)*(.+)$").unwrap(),
            docker_env_re: Regex::new(r"(?im)^\s*(?:ARG|ENV)\s+([A-Za-z_][A-Za-z0-9_]*)(?:\s*=|\s+)").unwrap(),

            // Scala patterns
            scala_import_re: Regex::new(r"(?m)^\s*import\s+([a-zA-Z0-9_.{}]+)").unwrap(),
            scala_type_re: Regex::new(r"(?m)^\s*(?:(?:abstract|final|sealed|case|implicit|private|protected)\s+)*(class|object|trait)\s+([a-zA-Z0-9_]+)(?:<[^>]+>)?(?:\s*\([^)]*\))?(?:\s+extends\s+([^{]+))?").unwrap(),
            scala_fn_re: Regex::new(r"(?m)^\s*(?:(?:override|final|abstract|private|protected|implicit|lazy|inline)\s+)*def\s+([a-zA-Z0-9_]+)").unwrap(),

            // Lua patterns
            lua_require_re: Regex::new(r#"(?m)^\s*(?:local\s+[a-zA-Z0-9_.]+\s*=\s*)?require\s*\(?["']([^"']+)["']"#).unwrap(),
            lua_fn_re: Regex::new(r"(?m)^\s*(?:local\s+)?function\s+(?:[a-zA-Z0-9_]+[.:])*([a-zA-Z0-9_]+)\s*\(").unwrap(),

            // Elixir patterns
            ex_module_re: Regex::new(r"(?m)^\s*defmodule\s+([A-Za-z0-9_.]+)\s+do").unwrap(),
            ex_protocol_re: Regex::new(r"(?m)^\s*defprotocol\s+([A-Za-z0-9_.]+)\s+do").unwrap(),
            ex_fn_re: Regex::new(r"(?m)^\s*def(?:macro)?p?\s+([a-zA-Z0-9_]+[!?]?)").unwrap(),
            ex_alias_re: Regex::new(r"(?m)^\s*(?:alias|import|use|require)\s+([A-Za-z0-9_.{}]+)").unwrap(),

            // Shell patterns
            sh_fn_paren_re: Regex::new(r"(?m)^\s*([a-zA-Z_][a-zA-Z0-9_-]*)\s*\(\s*\)\s*\{?").unwrap(),
            sh_fn_kw_re: Regex::new(r"(?m)^\s*function\s+([a-zA-Z_][a-zA-Z0-9_-]*)").unwrap(),
            sh_source_re: Regex::new(r#"(?m)^\s*(?:source|\.)\s+([^"'`\s;|&]+)"#).unwrap(),
        }
    }

    /// Вычисляет SHA256 хэш файла.
    pub fn file_sha256(content: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content);
        hex::encode(hasher.finalize())
    }

    /// Сканирует кодовую базу репозитория по указанному пути.
    pub fn scan_directory(
        &self,
        root: &Path,
        known_hashes: Option<&HashMap<String, String>>,
    ) -> AstScanResult {
        let mut result = AstScanResult::default();
        let mut files_to_scan = Vec::new();

        self.collect_files(root, &mut files_to_scan);

        for (rel_path, abs_path) in files_to_scan {
            if let Ok(bytes) = fs::read(&abs_path) {
                let hash = Self::file_sha256(&bytes);
                let file_size = bytes.len();
                result.file_hashes.insert(rel_path.clone(), hash.clone());

                if let Some(known) = known_hashes {
                    if let Some(prev_hash) = known.get(&rel_path) {
                        if prev_hash == &hash {
                            continue; // Файл не изменился
                        }
                    }
                }

                // Ф47.1 (ADR-AST-4): cap размера — файл трекается (хэш/мета),
                // но не парсится (защита от гигантских openapi.json/манифестов)
                if let Some(spec) = LangSpec::for_path(&rel_path) {
                    if bytes.len() > spec.max_bytes {
                        tracing::warn!(
                            "AST: файл {} ({} байт) превышает cap {} байт для '{}' — пропущен без парсинга",
                            rel_path, bytes.len(), spec.max_bytes, spec.name
                        );
                        continue;
                    }
                }

                if let Ok(content) = String::from_utf8(bytes) {
                    let lines_count = content.lines().count();
                    result.files_scanned += 1;
                    result.lines_total += lines_count;
                    result
                        .file_meta
                        .insert(rel_path.clone(), (file_size, lines_count));
                    self.parse_file(&rel_path, &content, &mut result);
                }
            }
        }

        result
    }

    fn collect_files(&self, root: &Path, out: &mut Vec<(String, PathBuf)>) {
        let mut builder = ignore::WalkBuilder::new(root);
        builder
            .hidden(true)
            .parents(true)
            .ignore(true)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .add_custom_ignore_filename(".gitignore");

        for entry in builder.build().flatten() {
            let path = entry.path();
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                continue;
            }
            if let Ok(rel) = path.strip_prefix(root) {
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                // Ф47.2: единый SKIP_DIRS на любом уровне вложенности
                // (сам обходчик не обрезает поддерево по имени каталога)
                if rel_has_skip_dir(&rel_str) {
                    continue;
                }
                // Ф47.1: реестр LANGS — единственный whitelist форматов
                if LangSpec::for_path(&rel_str).is_some() {
                    out.push((rel_str, path.to_path_buf()));
                }
            }
        }
    }

    /// Парсит содержимое отдельного файла в узлы и ребра AST.
    pub fn parse_file(&self, rel_path: &str, content: &str, out: &mut AstScanResult) {
        let file_node_id = format!("file:{}", rel_path);
        let file_label = rel_path.to_string();

        out.nodes.push(AstNode {
            node_id: file_node_id.clone(),
            label: file_label,
            node_type: "File".to_string(),
            description: format!("Исходный файл проекта: {}", rel_path),
            file_path: rel_path.to_string(),
            line_start: 1,
            line_end: content.lines().count().max(1),
        });

        // Ф47.1: диспетчер через реестр (бывший match ext.as_str())
        if let Some(spec) = LangSpec::for_path(rel_path) {
            spec.parser
                .execute(self, rel_path, &file_node_id, content, out);
        }
    }

    fn parse_rust(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. Извлечение use-импортов
        for cap in self.rust_use_re.captures_iter(content) {
            if let Some(import_path) = cap.get(1) {
                let import_str = import_path.as_str().trim();
                let target_node_id = format!("module:{}", import_str);
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id,
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("use {};", import_str),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 1b. Ф39.1: импорты с алиасами для type-pass
        for cap in self.rust_use_re.captures_iter(content) {
            if let Some(import_path) = cap.get(1) {
                let import_str = import_path.as_str().trim();
                if let Some((module, item, alias)) = split_rust_use(import_str) {
                    out.imports.push(ImportRef {
                        rel_path: rel_path.to_string(),
                        module,
                        item,
                        alias,
                    });
                }
            }
        }

        // 1c. Ф39.1: места вызовов для type-pass
        collect_call_sites(rel_path, &lines, RUST_CALL_SKIP, &self.call_re, 0, out);

        // 2. Извлечение struct/enum
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.rust_struct_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let struct_name = name_match.as_str();
                    let node_id = format!("struct:{}:{}", rel_path, struct_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: struct_name.to_string(),
                        node_type: "Struct".to_string(),
                        description: format!(
                            "Структура/перечисление `{}` в {}",
                            struct_name, rel_path
                        ),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        // 3. Извлечение traits
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.rust_trait_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let trait_name = name_match.as_str();
                    let node_id = format!("trait:{}:{}", rel_path, trait_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: trait_name.to_string(),
                        node_type: "Trait".to_string(),
                        description: format!("Трейт `{}` в {}", trait_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        // 4. Извлечение functions
        // Ф40.2: маршруты axum/actix — `#[get("/path")]` перед fn
        let mut route_by_line: std::collections::HashMap<usize, (String, String)> =
            std::collections::HashMap::new();
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.rust_route_re.captures(line) {
                if let (Some(m), Some(p)) = (cap.get(1), cap.get(2)) {
                    route_by_line.insert(i, (m.as_str().to_uppercase(), p.as_str().to_string()));
                }
            }
        }
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.rust_fn_re.captures(line) {
                if let Some(fn_match) = cap.get(1) {
                    let fn_name = fn_match.as_str();
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    let sig = line.trim().trim_end_matches('{').trim();
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("Функция `{}` ({}) в {}", fn_name, sig, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: sig.to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                    // Ф40.2: ROUTE — framework edge (provenance=INFERRED):
                    // атрибут маршрута на этой или одной из двух предыдущих строк
                    let route = [i.checked_sub(2), i.checked_sub(1), Some(i)]
                        .into_iter()
                        .flatten()
                        .find_map(|k| route_by_line.get(&k).cloned());
                    if let Some((method, path)) = route {
                        out.edges.push(AstEdge {
                            source_node_id: node_id,
                            target_node_id: format!("route:{} {}", method, path),
                            label: "ROUTE".to_string(),
                            weight: 1.0,
                            context: format!("{} {}", method, path),
                            provenance: "INFERRED".to_string(),
                        });
                    }
                }
            }
        }
    }

    fn parse_python(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        line_offset: usize,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // Ф49.4б/в: клиентские HTTP-вызовы и gql-строки (кандидаты мостов)
        collect_http_calls(rel_path, content, &self.py_http_re, out);
        for cap in self.py_gql_re.captures_iter(content) {
            let body = cap
                .get(1)
                .or_else(|| cap.get(2))
                .map(|m| m.as_str())
                .unwrap_or("");
            for icap in self.gql_ident_re.captures_iter(body) {
                if let Some(ident) = icap.get(1) {
                    out.gql_fields.push(GqlFieldRef {
                        rel_path: rel_path.to_string(),
                        field: ident.as_str().to_string(),
                    });
                }
            }
        }

        // 1. Извлечение imports
        for cap in self.py_import_re.captures_iter(content) {
            let mod_name = cap
                .get(1)
                .or_else(|| cap.get(3))
                .map(|m| m.as_str().trim())
                .unwrap_or("");
            if !mod_name.is_empty() {
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", mod_name),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("import {}", mod_name),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 1b. Ф39.1: импорты с алиасами для type-pass
        for cap in self.py_import_re.captures_iter(content) {
            if let Some(m) = cap.get(1) {
                // from X import a, b as c
                let module = m.as_str().trim();
                let items = cap.get(2).map(|mm| mm.as_str()).unwrap_or("");
                if module.is_empty() {
                    continue;
                }
                for it in items.split(',') {
                    let it = it.trim();
                    if it.is_empty() {
                        continue;
                    }
                    let (item, alias) = match it.split_once(" as ") {
                        Some((n, a)) => (n.trim().to_string(), Some(a.trim().to_string())),
                        None => (it.to_string(), None),
                    };
                    out.imports.push(ImportRef {
                        rel_path: rel_path.to_string(),
                        module: module.to_string(),
                        item: Some(item),
                        alias,
                    });
                }
            } else if let Some(m) = cap.get(3) {
                // import x.y
                let mod_name = m.as_str().trim();
                if !mod_name.is_empty() {
                    out.imports.push(ImportRef {
                        rel_path: rel_path.to_string(),
                        module: mod_name.to_string(),
                        item: None,
                        alias: None,
                    });
                }
            }
        }

        // 1c. Ф39.1: места вызовов для type-pass
        collect_call_sites(rel_path, &lines, PY_CALL_SKIP, &self.call_re, line_offset, out);

        // 2. Извлечение classes
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.py_class_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let class_name = name_match.as_str();
                    let node_id = format!("class:{}:{}", rel_path, class_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: class_name.to_string(),
                        node_type: "Class".to_string(),
                        description: format!("Класс `{}` в {}", class_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1 + line_offset,
                        line_end: i + 1 + line_offset,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    // Базовые классы
                    if let Some(bases_match) = cap.get(2) {
                        for base in bases_match.as_str().split(',') {
                            let base_clean = base.trim();
                            if !base_clean.is_empty() {
                                out.edges.push(AstEdge {
                                    source_node_id: node_id.clone(),
                                    target_node_id: format!("class:{}", base_clean),
                                    label: "INHERITS".to_string(),
                                    weight: 1.0,
                                    context: format!("extends {}", base_clean),
                                    provenance: "EXTRACTED".to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }

        // 2b. Ф40.2: QUERIES_TABLE — SQLAlchemy `__tablename__ = "x"` в классе
        let mut last_class: Option<String> = None;
        for line in lines.iter() {
            let trimmed = line.trim_start();
            if let Some(cap) = self.py_class_re.captures(line) {
                if let Some(nm) = cap.get(1) {
                    last_class = Some(format!("class:{}:{}", rel_path, nm.as_str()));
                    continue;
                }
            }
            if let Some(cap) = self.py_tablename_re.captures(line) {
                if let (Some(tbl), Some(cls)) = (cap.get(1), last_class.clone()) {
                    out.edges.push(AstEdge {
                        source_node_id: cls,
                        target_node_id: format!("table:{}", tbl.as_str()),
                        label: "QUERIES_TABLE".to_string(),
                        weight: 1.0,
                        context: format!("__tablename__ = \"{}\"", tbl.as_str()),
                        provenance: "INFERRED".to_string(),
                    });
                }
            }
            let _ = trimmed;
        }

        // 3. Извлечение functions
        // Ф40.2: маршруты FastAPI/Flask — `@app.get("/path")` перед def
        let mut route_by_line: std::collections::HashMap<usize, (String, String)> =
            std::collections::HashMap::new();
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.py_route_re.captures(line) {
                if let (Some(m), Some(p)) = (cap.get(1), cap.get(2)) {
                    route_by_line.insert(i, (m.as_str().to_uppercase(), p.as_str().to_string()));
                }
            }
        }
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.py_fn_re.captures(line) {
                if let Some(fn_match) = cap.get(1) {
                    let fn_name = fn_match.as_str();
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("Функция/метод `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1 + line_offset,
                        line_end: i + 1 + line_offset,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                    let route = [i.checked_sub(2), i.checked_sub(1), Some(i)]
                        .into_iter()
                        .flatten()
                        .find_map(|k| route_by_line.get(&k).cloned());
                    if let Some((method, path)) = route {
                        out.edges.push(AstEdge {
                            source_node_id: node_id,
                            target_node_id: format!("route:{} {}", method, path),
                            label: "ROUTE".to_string(),
                            weight: 1.0,
                            context: format!("{} {}", method, path),
                            provenance: "INFERRED".to_string(),
                        });
                    }
                }
            }
        }
    }

    fn parse_ts_js(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        line_offset: usize,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // Ф49.4б/в: клиентские HTTP-вызовы и gql-строки (кандидаты мостов)
        collect_http_calls(rel_path, content, &self.ts_http_re, out);
        for cap in self.ts_gql_re.captures_iter(content) {
            if let Some(body) = cap.get(1) {
                for icap in self.gql_ident_re.captures_iter(body.as_str()) {
                    if let Some(ident) = icap.get(1) {
                        out.gql_fields.push(GqlFieldRef {
                            rel_path: rel_path.to_string(),
                            field: ident.as_str().to_string(),
                        });
                    }
                }
            }
        }

        // 1. Imports
        for cap in self.ts_import_re.captures_iter(content) {
            if let Some(mod_src) = cap.get(4) {
                let mod_name = mod_src.as_str();
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", mod_name),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("from '{}'", mod_name),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 2. Classes & Interfaces
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.ts_class_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let class_name = name_match.as_str();
                    let node_id = format!("class:{}:{}", rel_path, class_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: class_name.to_string(),
                        node_type: "Class".to_string(),
                        description: format!("TypeScript/JS класс `{}` в {}", class_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1 + line_offset,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            } else if let Some(cap) = self.ts_interface_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let iface_name = name_match.as_str();
                    let node_id = format!("interface:{}:{}", rel_path, iface_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: iface_name.to_string(),
                        node_type: "Interface".to_string(),
                        description: format!("Интерфейс/тип `{}` в {}", iface_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1 + line_offset,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        // 3. Functions
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.ts_fn_re.captures(line) {
                let fn_name = cap
                    .get(1)
                    .or_else(|| cap.get(2))
                    .map(|m| m.as_str())
                    .unwrap_or("");
                if !fn_name.is_empty() {
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("Функция `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1 + line_offset,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }

    fn parse_go(&self, rel_path: &str, file_node_id: &str, content: &str, out: &mut AstScanResult) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. Structs
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.go_struct_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let struct_name = name_match.as_str();
                    let node_id = format!("struct:{}:{}", rel_path, struct_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: struct_name.to_string(),
                        node_type: "Struct".to_string(),
                        description: format!(
                            "Go структура/интерфейс `{}` в {}",
                            struct_name, rel_path
                        ),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        // 2. Functions
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.go_fn_re.captures(line) {
                if let Some(fn_match) = cap.get(1) {
                    let fn_name = fn_match.as_str();
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("Go функция/метод `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }

    fn parse_sql(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let mut tables = Vec::new();

        for cap in self.sql_table_re.captures_iter(content) {
            if let Some(table_match) = cap.get(1) {
                let table_name = table_match
                    .as_str()
                    .trim_matches(|c| c == '`' || c == '[' || c == ']');
                let node_id = format!("table:{}", table_name);
                tables.push(node_id.clone());
                out.nodes.push(AstNode {
                    node_id: node_id.clone(),
                    label: table_name.to_string(),
                    node_type: "Table".to_string(),
                    description: format!("SQL таблица `{}` в {}", table_name, rel_path),
                    file_path: rel_path.to_string(),
                    line_start: 1,
                    line_end: 1,
                });
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: node_id,
                    label: "DEFINES".to_string(),
                    weight: 1.0,
                    context: format!("CREATE TABLE {}", table_name),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // Foreign keys
        for cap in self.sql_fk_re.captures_iter(content) {
            if let Some(fk_match) = cap.get(1) {
                let fk_target = fk_match
                    .as_str()
                    .trim_matches(|c| c == '`' || c == '[' || c == ']');
                if let Some(src_table) = tables.first() {
                    out.edges.push(AstEdge {
                        source_node_id: src_table.clone(),
                        target_node_id: format!("table:{}", fk_target),
                        label: "FOREIGN_KEY_TO".to_string(),
                        weight: 1.0,
                        context: format!("REFERENCES {}", fk_target),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }

    fn parse_php(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. Imports (use)
        for cap in self.php_use_re.captures_iter(content) {
            if let Some(target) = cap.get(1) {
                let target_str = target.as_str().trim();
                if !target_str.is_empty() {
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: format!("module:{}", target_str),
                        label: "IMPORTS".to_string(),
                        weight: 1.0,
                        context: format!("use {};", target_str),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        // 2. Classes, Interfaces, Traits
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.php_class_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let class_name = name_match.as_str();
                    let node_id = format!("class:{}:{}", rel_path, class_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: class_name.to_string(),
                        node_type: "Class".to_string(),
                        description: format!("PHP класс `{}` в {}", class_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    // Extends
                    if let Some(ext_match) = cap.get(2) {
                        let base = ext_match.as_str().trim();
                        if !base.is_empty() {
                            out.edges.push(AstEdge {
                                source_node_id: node_id.clone(),
                                target_node_id: format!("class:{}", base),
                                label: "INHERITS".to_string(),
                                weight: 1.0,
                                context: format!("extends {}", base),
                                provenance: "EXTRACTED".to_string(),
                            });
                        }
                    }

                    // Implements
                    if let Some(impl_match) = cap.get(3) {
                        for iface in impl_match.as_str().split(',') {
                            let iface_clean = iface.trim();
                            if !iface_clean.is_empty() {
                                out.edges.push(AstEdge {
                                    source_node_id: node_id.clone(),
                                    target_node_id: format!("interface:{}", iface_clean),
                                    label: "IMPLEMENTS".to_string(),
                                    weight: 1.0,
                                    context: format!("implements {}", iface_clean),
                                    provenance: "EXTRACTED".to_string(),
                                });
                            }
                        }
                    }
                }
            } else if let Some(cap) = self.php_interface_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let iface_name = name_match.as_str();
                    let node_id = format!("interface:{}:{}", rel_path, iface_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: iface_name.to_string(),
                        node_type: "Interface".to_string(),
                        description: format!("PHP интерфейс `{}` в {}", iface_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    if let Some(ext_match) = cap.get(2) {
                        for base in ext_match.as_str().split(',') {
                            let base_clean = base.trim();
                            if !base_clean.is_empty() {
                                out.edges.push(AstEdge {
                                    source_node_id: node_id.clone(),
                                    target_node_id: format!("interface:{}", base_clean),
                                    label: "INHERITS".to_string(),
                                    weight: 1.0,
                                    context: format!("extends {}", base_clean),
                                    provenance: "EXTRACTED".to_string(),
                                });
                            }
                        }
                    }
                }
            } else if let Some(cap) = self.php_trait_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let trait_name = name_match.as_str();
                    let node_id = format!("trait:{}:{}", rel_path, trait_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: trait_name.to_string(),
                        node_type: "Trait".to_string(),
                        description: format!("PHP трейт `{}` в {}", trait_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        // 3. Functions & Methods
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.php_fn_re.captures(line) {
                if let Some(fn_match) = cap.get(1) {
                    let fn_name = fn_match.as_str();
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("PHP функция/метод `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }

    fn parse_dart(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. Imports
        for cap in self.dart_import_re.captures_iter(content) {
            if let Some(mod_src) = cap.get(1) {
                let mod_name = mod_src.as_str();
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", mod_name),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("import '{}';", mod_name),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 2. Classes & Mixins
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.dart_class_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let class_name = name_match.as_str();
                    let node_id = format!("class:{}:{}", rel_path, class_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: class_name.to_string(),
                        node_type: "Class".to_string(),
                        description: format!("Dart класс `{}` в {}", class_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    // Extends
                    if let Some(ext_match) = cap.get(2) {
                        let base = ext_match.as_str().trim();
                        if !base.is_empty() {
                            out.edges.push(AstEdge {
                                source_node_id: node_id.clone(),
                                target_node_id: format!("class:{}", base),
                                label: "INHERITS".to_string(),
                                weight: 1.0,
                                context: format!("extends {}", base),
                                provenance: "EXTRACTED".to_string(),
                            });
                        }
                    }

                    // With mixins
                    if let Some(with_match) = cap.get(3) {
                        for mixin in with_match.as_str().split(',') {
                            let mixin_clean = mixin.trim();
                            if !mixin_clean.is_empty() {
                                out.edges.push(AstEdge {
                                    source_node_id: node_id.clone(),
                                    target_node_id: format!("trait:{}", mixin_clean),
                                    label: "IMPLEMENTS".to_string(),
                                    weight: 1.0,
                                    context: format!("with {}", mixin_clean),
                                    provenance: "EXTRACTED".to_string(),
                                });
                            }
                        }
                    }

                    // Implements
                    if let Some(impl_match) = cap.get(4) {
                        for iface in impl_match.as_str().split(',') {
                            let iface_clean = iface.trim();
                            if !iface_clean.is_empty() {
                                out.edges.push(AstEdge {
                                    source_node_id: node_id.clone(),
                                    target_node_id: format!("interface:{}", iface_clean),
                                    label: "IMPLEMENTS".to_string(),
                                    weight: 1.0,
                                    context: format!("implements {}", iface_clean),
                                    provenance: "EXTRACTED".to_string(),
                                });
                            }
                        }
                    }
                }
            } else if let Some(cap) = self.dart_mixin_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let mixin_name = name_match.as_str();
                    let node_id = format!("trait:{}:{}", rel_path, mixin_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: mixin_name.to_string(),
                        node_type: "Trait".to_string(),
                        description: format!("Dart миксин `{}` в {}", mixin_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        // 3. Functions & Methods
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.dart_fn_re.captures(line) {
                if let Some(fn_match) = cap.get(1) {
                    let fn_name = fn_match.as_str();
                    if matches!(fn_name, "if" | "for" | "while" | "switch" | "catch") {
                        continue;
                    }
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("Dart функция/метод `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }

    fn parse_java(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. Package & Imports
        for cap in self.java_package_re.captures_iter(content) {
            if let Some(pkg) = cap.get(1) {
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", pkg.as_str()),
                    label: "DEFINES".to_string(),
                    weight: 1.0,
                    context: format!("package {};", pkg.as_str()),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        for cap in self.java_import_re.captures_iter(content) {
            if let Some(imp) = cap.get(1) {
                let imp_str = imp.as_str();
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", imp_str),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("import {};", imp_str),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 2. Classes, Interfaces, Enums, Records
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.java_class_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let class_name = name_match.as_str();
                    let node_id = format!("class:{}:{}", rel_path, class_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: class_name.to_string(),
                        node_type: "Class".to_string(),
                        description: format!("Java класс/тип `{}` в {}", class_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    // Extends
                    if let Some(ext_match) = cap.get(2) {
                        let base = ext_match.as_str().trim();
                        if !base.is_empty() {
                            out.edges.push(AstEdge {
                                source_node_id: node_id.clone(),
                                target_node_id: format!("class:{}", base),
                                label: "INHERITS".to_string(),
                                weight: 1.0,
                                context: format!("extends {}", base),
                                provenance: "EXTRACTED".to_string(),
                            });
                        }
                    }

                    // Implements
                    if let Some(impl_match) = cap.get(3) {
                        for iface in impl_match.as_str().split(',') {
                            let iface_clean = iface.trim();
                            if !iface_clean.is_empty() {
                                out.edges.push(AstEdge {
                                    source_node_id: node_id.clone(),
                                    target_node_id: format!("interface:{}", iface_clean),
                                    label: "IMPLEMENTS".to_string(),
                                    weight: 1.0,
                                    context: format!("implements {}", iface_clean),
                                    provenance: "EXTRACTED".to_string(),
                                });
                            }
                        }
                    }
                }
            } else if let Some(cap) = self.java_interface_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let iface_name = name_match.as_str();
                    let node_id = format!("interface:{}:{}", rel_path, iface_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: iface_name.to_string(),
                        node_type: "Interface".to_string(),
                        description: format!("Java интерфейс `{}` в {}", iface_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    if let Some(ext_match) = cap.get(2) {
                        for base in ext_match.as_str().split(',') {
                            let base_clean = base.trim();
                            if !base_clean.is_empty() {
                                out.edges.push(AstEdge {
                                    source_node_id: node_id.clone(),
                                    target_node_id: format!("interface:{}", base_clean),
                                    label: "INHERITS".to_string(),
                                    weight: 1.0,
                                    context: format!("extends {}", base_clean),
                                    provenance: "EXTRACTED".to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }

        // 3. Methods
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.java_fn_re.captures(line) {
                if let Some(fn_match) = cap.get(2) {
                    let fn_name = fn_match.as_str();
                    if matches!(fn_name, "if" | "for" | "while" | "switch" | "catch") {
                        continue;
                    }
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("Java метод `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }

    fn parse_csharp(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. Usings
        for cap in self.cs_using_re.captures_iter(content) {
            if let Some(imp) = cap.get(1) {
                let ns = imp.as_str();
                if ns == "var" {
                    continue; // `using var x = ...` — disposable, не импорт
                }
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", ns),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("using {};", ns),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 2. Типы (class/record/struct/enum) и интерфейсы
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.cs_type_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let type_name = name_match.as_str();
                    let node_id = format!("class:{}:{}", rel_path, type_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: type_name.to_string(),
                        node_type: "Class".to_string(),
                        description: format!("C# класс/тип `{}` в {}", type_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    // Базовый тип и интерфейсы: первый — наследование, остальные — реализации
                    if let Some(bases_match) = cap.get(2) {
                        for (idx, base) in bases_match.as_str().split(',').enumerate() {
                            let base_clean = base.trim();
                            let base_clean = match base_clean.find(" where") {
                                Some(p) => &base_clean[..p], // обрезаем generic-контрейны
                                None => base_clean,
                            };
                            if base_clean.is_empty() {
                                continue;
                            }
                            let (target, label) = if idx == 0 {
                                (format!("class:{}", base_clean), "INHERITS")
                            } else {
                                (format!("interface:{}", base_clean), "IMPLEMENTS")
                            };
                            out.edges.push(AstEdge {
                                source_node_id: node_id.clone(),
                                target_node_id: target,
                                label: label.to_string(),
                                weight: 1.0,
                                context: format!(": {}", base_clean),
                                provenance: "EXTRACTED".to_string(),
                            });
                        }
                    }
                }
            } else if let Some(cap) = self.cs_interface_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let iface_name = name_match.as_str();
                    let node_id = format!("interface:{}:{}", rel_path, iface_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: iface_name.to_string(),
                        node_type: "Interface".to_string(),
                        description: format!("C# интерфейс `{}` в {}", iface_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    if let Some(bases_match) = cap.get(2) {
                        for base in bases_match.as_str().split(',') {
                            let base_clean = base.trim();
                            if base_clean.is_empty() {
                                continue;
                            }
                            out.edges.push(AstEdge {
                                source_node_id: node_id.clone(),
                                target_node_id: format!("interface:{}", base_clean),
                                label: "INHERITS".to_string(),
                                weight: 1.0,
                                context: format!(": {}", base_clean),
                                provenance: "EXTRACTED".to_string(),
                            });
                        }
                    }
                }
            }
        }

        // 3. Методы
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.cs_fn_re.captures(line) {
                if let Some(fn_match) = cap.get(2) {
                    let fn_name = fn_match.as_str();
                    if matches!(fn_name, "if" | "for" | "while" | "switch" | "catch") {
                        continue;
                    }
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("C# метод `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }

    fn parse_c_cpp(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. #include
        for cap in self.cpp_include_re.captures_iter(content) {
            if let Some(inc) = cap.get(1) {
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", inc.as_str()),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("#include {}", inc.as_str()),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 2. Классы и структуры
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.cpp_class_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let class_name = name_match.as_str();
                    let node_id = format!("class:{}:{}", rel_path, class_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: class_name.to_string(),
                        node_type: "Class".to_string(),
                        description: format!("C++ класс `{}` в {}", class_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    if let Some(bases_match) = cap.get(2) {
                        for base in bases_match.as_str().split(',') {
                            let base_clean = base
                                .trim()
                                .trim_start_matches("public ")
                                .trim_start_matches("protected ")
                                .trim_start_matches("private ")
                                .trim_start_matches("virtual ")
                                .trim();
                            if base_clean.is_empty() {
                                continue;
                            }
                            out.edges.push(AstEdge {
                                source_node_id: node_id.clone(),
                                target_node_id: format!("class:{}", base_clean),
                                label: "INHERITS".to_string(),
                                weight: 1.0,
                                context: format!(": {}", base_clean),
                                provenance: "EXTRACTED".to_string(),
                            });
                        }
                    }
                }
            } else if let Some(cap) = self.cpp_struct_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let struct_name = name_match.as_str();
                    let node_id = format!("struct:{}:{}", rel_path, struct_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: struct_name.to_string(),
                        node_type: "Struct".to_string(),
                        description: format!("C++ структура `{}` в {}", struct_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        // 3. Функции и методы (только определения с телом `{`)
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.cpp_fn_re.captures(line) {
                if let Some(fn_match) = cap.get(1) {
                    let fn_name = fn_match.as_str();
                    if matches!(
                        fn_name,
                        "if" | "for"
                            | "while"
                            | "switch"
                            | "catch"
                            | "return"
                            | "else"
                            | "do"
                            | "sizeof"
                            | "throw"
                    ) {
                        continue;
                    }
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("C++ функция/метод `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }

    fn parse_kotlin(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. Imports
        for cap in self.kt_import_re.captures_iter(content) {
            if let Some(m) = cap.get(1) {
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", m.as_str()),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("import {}", m.as_str()),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 2. Классы/объекты и интерфейсы
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.kt_class_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let class_name = name_match.as_str();
                    let node_id = format!("class:{}:{}", rel_path, class_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: class_name.to_string(),
                        node_type: "Class".to_string(),
                        description: format!("Kotlin класс/объект `{}` в {}", class_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    // База после `:`: первый — суперкласс, остальные — интерфейсы
                    if let Some(bases_match) = cap.get(2) {
                        let cleaned = strip_paren_groups(bases_match.as_str());
                        for (idx, base) in cleaned.split(',').enumerate() {
                            let base_clean = match base.trim().find('<') {
                                Some(p) => &base.trim()[..p],
                                None => base.trim(),
                            };
                            if base_clean.is_empty() {
                                continue;
                            }
                            let (target, label) = if idx == 0 {
                                (format!("class:{}", base_clean), "INHERITS")
                            } else {
                                (format!("interface:{}", base_clean), "IMPLEMENTS")
                            };
                            out.edges.push(AstEdge {
                                source_node_id: node_id.clone(),
                                target_node_id: target,
                                label: label.to_string(),
                                weight: 1.0,
                                context: format!(": {}", base_clean),
                                provenance: "EXTRACTED".to_string(),
                            });
                        }
                    }
                }
            } else if let Some(cap) = self.kt_interface_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let iface_name = name_match.as_str();
                    let node_id = format!("interface:{}:{}", rel_path, iface_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: iface_name.to_string(),
                        node_type: "Interface".to_string(),
                        description: format!("Kotlin интерфейс `{}` в {}", iface_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    if let Some(bases_match) = cap.get(2) {
                        for base in bases_match.as_str().split(',') {
                            let base_clean = base.trim();
                            if base_clean.is_empty() {
                                continue;
                            }
                            out.edges.push(AstEdge {
                                source_node_id: node_id.clone(),
                                target_node_id: format!("interface:{}", base_clean),
                                label: "INHERITS".to_string(),
                                weight: 1.0,
                                context: format!(": {}", base_clean),
                                provenance: "EXTRACTED".to_string(),
                            });
                        }
                    }
                }
            }
        }

        // 3. Функции (включая extension-функции — берём последний сегмент)
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.kt_fn_re.captures(line) {
                if let Some(fn_match) = cap.get(1) {
                    let fn_name = fn_match.as_str();
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("Kotlin функция `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }

    fn parse_swift(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();
        // Raw-value типы enum — не протоколы, рёбра соответствия для них не выдумываем
        const SWIFT_RAW_TYPES: &[&str] = &[
            "String", "Int", "Int8", "Int16", "Int32", "Int64", "UInt", "UInt8", "UInt16",
            "UInt32", "UInt64", "Double", "Float", "Bool", "Character", "Decimal",
        ];

        // 1. Imports
        for cap in self.sw_import_re.captures_iter(content) {
            if let Some(m) = cap.get(1) {
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", m.as_str()),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("import {}", m.as_str()),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 2. Типы (class/struct/enum/extension) и протоколы
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.sw_class_re.captures(line) {
                let kind = cap.get(1).map(|m| m.as_str()).unwrap_or("class");
                if let Some(name_match) = cap.get(2) {
                    let type_name = name_match.as_str();
                    let node_id = if kind == "extension" {
                        format!("extension:{}:{}", rel_path, type_name)
                    } else {
                        format!("class:{}:{}", rel_path, type_name)
                    };
                    let description = match kind {
                        "extension" => format!("Swift расширение `{}` в {}", type_name, rel_path),
                        "struct" => format!("Swift структура `{}` в {}", type_name, rel_path),
                        "enum" => format!("Swift перечисление `{}` в {}", type_name, rel_path),
                        _ => format!("Swift класс `{}` в {}", type_name, rel_path),
                    };
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: type_name.to_string(),
                        node_type: "Class".to_string(),
                        description,
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    // Только классы наследуются; struct/enum/extension — соответствие протоколам
                    if let Some(bases_match) = cap.get(3) {
                        for (idx, part) in bases_match.as_str().split(',').enumerate() {
                            for token in part.split('&') {
                                let token_trimmed = token.trim();
                                let base_clean = match token_trimmed.find('<') {
                                    Some(p) => &token_trimmed[..p],
                                    None => token_trimmed,
                                };
                                if base_clean.is_empty() {
                                    continue;
                                }
                                if kind == "enum" && SWIFT_RAW_TYPES.contains(&base_clean) {
                                    continue; // raw value, не протокол
                                }
                                let (target, label) = if kind == "class" && idx == 0 {
                                    (format!("class:{}", base_clean), "INHERITS")
                                } else {
                                    (format!("interface:{}", base_clean), "IMPLEMENTS")
                                };
                                out.edges.push(AstEdge {
                                    source_node_id: node_id.clone(),
                                    target_node_id: target,
                                    label: label.to_string(),
                                    weight: 1.0,
                                    context: format!(": {}", base_clean),
                                    provenance: "EXTRACTED".to_string(),
                                });
                            }
                        }
                    }
                }
            } else if let Some(cap) = self.sw_protocol_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let proto_name = name_match.as_str();
                    let node_id = format!("interface:{}:{}", rel_path, proto_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: proto_name.to_string(),
                        node_type: "Interface".to_string(),
                        description: format!("Swift протокол `{}` в {}", proto_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    if let Some(bases_match) = cap.get(2) {
                        for base in bases_match.as_str().split(',') {
                            let base_clean = base.trim();
                            if base_clean.is_empty() {
                                continue;
                            }
                            out.edges.push(AstEdge {
                                source_node_id: node_id.clone(),
                                target_node_id: format!("interface:{}", base_clean),
                                label: "INHERITS".to_string(),
                                weight: 1.0,
                                context: format!(": {}", base_clean),
                                provenance: "EXTRACTED".to_string(),
                            });
                        }
                    }
                }
            }
        }

        // 3. Функции, методы и инициализаторы
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.sw_fn_re.captures(line) {
                if let Some(fn_match) = cap.get(1) {
                    let fn_name = fn_match.as_str();
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("Swift функция/метод `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            } else if self.sw_init_re.is_match(line) {
                let node_id = format!("fn:{}:init", rel_path);
                out.nodes.push(AstNode {
                    node_id: node_id.clone(),
                    label: "init".to_string(),
                    node_type: "Function".to_string(),
                    description: format!("Swift инициализатор в {}", rel_path),
                    file_path: rel_path.to_string(),
                    line_start: i + 1,
                    line_end: i + 1,
                });
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: node_id,
                    label: "DEFINES".to_string(),
                    weight: 1.0,
                    context: line.trim().to_string(),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }
    }

    fn parse_ruby(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. require / require_relative
        for cap in self.rb_require_re.captures_iter(content) {
            if let Some(m) = cap.get(1) {
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", m.as_str()),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("require '{}'", m.as_str()),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 2. Классы и модули
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.rb_class_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let class_name = name_match.as_str();
                    let node_id = format!("class:{}:{}", rel_path, class_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: class_name.to_string(),
                        node_type: "Class".to_string(),
                        description: format!("Ruby класс `{}` в {}", class_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    if let Some(base_match) = cap.get(2) {
                        let base = base_match.as_str().trim();
                        if !base.is_empty() {
                            out.edges.push(AstEdge {
                                source_node_id: node_id.clone(),
                                target_node_id: format!("class:{}", base),
                                label: "INHERITS".to_string(),
                                weight: 1.0,
                                context: format!("< {}", base),
                                provenance: "EXTRACTED".to_string(),
                            });
                        }
                    }
                }
            } else if let Some(cap) = self.rb_module_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let module_name = name_match.as_str();
                    let node_id = format!("module:{}:{}", rel_path, module_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: module_name.to_string(),
                        node_type: "Module".to_string(),
                        description: format!("Ruby модуль `{}` в {}", module_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        // 3. Методы (включая self.-методы и суффиксы ?/!)
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.rb_fn_re.captures(line) {
                if let Some(fn_match) = cap.get(1) {
                    let fn_name = fn_match.as_str();
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("Ruby метод `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }

    /// Vue/Svelte SFC: содержимое `<script>`-блоков переиспользует TS/JS-парсер.
    /// line_offset — чтобы номера строк символов были относительно всего файла.
    fn parse_sfc(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        for cap in self.sfc_script_re.captures_iter(content) {
            let Some(m) = cap.get(1) else { continue };
            // Переводы строк до начала тела: смещение корректно и когда
            // `<script ...>` лежит на строке тега, и когда тело с новой строки
            let offset = content[..m.start()].matches('\n').count();
            self.parse_ts_js(rel_path, file_node_id, m.as_str(), offset, out);
        }

        // Ф48.3: `<style>` → parse_css со смещением строк (номера — относительно
        // всего SFC-файла, как у script-блоков с Ф10)
        for cap in self.sfc_style_re.captures_iter(content) {
            let Some(m) = cap.get(1) else { continue };
            let offset = content[..m.start()].matches('\n').count();
            self.parse_css(rel_path, file_node_id, m.as_str(), offset, out);
        }

        // Ф48.3: `<template>` — упрощённый HTML-разбор: class → BINDS_CLASS,
        // PascalCase-компоненты → IMPORTS (DOM-дерево не строим, ADR-AST-1)
        for cap in self.sfc_template_re.captures_iter(content) {
            let Some(m) = cap.get(1) else { continue };
            let body = m.as_str();
            self.collect_class_bindings(file_node_id, body, out);
            for ccap in self.sfc_component_re.captures_iter(body) {
                if let Some(name) = ccap.get(1) {
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: format!("module:{}", name.as_str()),
                        label: "IMPORTS".to_string(),
                        weight: 1.0,
                        context: format!("<{} />", name.as_str()),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }

    /// Ф48.1: HTML (и blade-шаблоны) — теги с `id` → `Element`, `<script src>` →
    /// `IMPORTS` (локальный путь → `file:`, иначе `module:`), `<link
    /// rel=stylesheet>` → `STYLES` (расширение стиля → `file:`, иначе `css:`),
    /// `class="a b"` → `BINDS_CLASS` (INFERRED), `action`/`href`/`hx-*` →
    /// `ROUTE` (INFERRED) — связь шаблонов с эндпоинтами, как в Ф40.2.
    fn parse_html(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let clean = self.html_comment_re.replace_all(content, "");
        let mut seen_ids: HashSet<String> = HashSet::new();
        let mut seen_routes: HashSet<String> = HashSet::new();
        let mut seen_imports: HashSet<String> = HashSet::new();
        let mut seen_styles: HashSet<String> = HashSet::new();

        for cap in self.html_tag_re.captures_iter(&clean) {
            let Some(tag_m) = cap.get(1) else { continue };
            let attrs_str = cap.get(2).map(|m| m.as_str()).unwrap_or("");
            let tag = tag_m.as_str().to_lowercase();
            let line = content[..cap.get(0).map(|m| m.start()).unwrap_or(0)]
                .matches('\n')
                .count()
                + 1;
            let attrs = self.parse_html_attrs(attrs_str);

            // Тег с id → Element (el:{file}#{id}, дедуп по id)
            if let Some(id) = attrs.get("id") {
                if seen_ids.insert(id.clone()) {
                    let node_id = format!("el:{}#{}", rel_path, id);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: format!("{}#{}", tag, id),
                        node_type: "Element".to_string(),
                        description: format!(
                            "Элемент `<{} id=\"{}\">` в {}:{}",
                            tag, id, rel_path, line
                        ),
                        file_path: rel_path.to_string(),
                        line_start: line,
                        line_end: line,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: format!("<{} id=\"{}\">", tag, id),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }

            match tag.as_str() {
                "script" => {
                    if let Some(src) = attrs.get("src") {
                        if seen_imports.insert(src.clone()) {
                            let target =
                                external_or_local_ref(src, &["js", "mjs", "cjs", "ts", "tsx", "jsx"]);
                            out.edges.push(AstEdge {
                                source_node_id: file_node_id.to_string(),
                                target_node_id: target,
                                label: "IMPORTS".to_string(),
                                weight: 1.0,
                                context: format!("<script src=\"{}\">", src),
                                provenance: "EXTRACTED".to_string(),
                            });
                        }
                    }
                }
                "link" => {
                    let is_stylesheet = attrs
                        .get("rel")
                        .map(|r| r.to_lowercase().contains("stylesheet"))
                        .unwrap_or(false);
                    if is_stylesheet {
                        if let Some(href) = attrs.get("href") {
                            if seen_styles.insert(href.clone()) {
                                out.edges.push(AstEdge {
                                    source_node_id: file_node_id.to_string(),
                                    target_node_id: stylesheet_target(href),
                                    label: "STYLES".to_string(),
                                    weight: 1.0,
                                    context: format!(
                                        "<link rel=\"stylesheet\" href=\"{}\">",
                                        href
                                    ),
                                    provenance: "EXTRACTED".to_string(),
                                });
                            }
                        }
                    }
                }
                "a" => {
                    if let Some(href) = attrs.get("href") {
                        push_html_route(out, file_node_id, "GET", href, &mut seen_routes);
                    }
                }
                "form" => {
                    if let Some(action) = attrs.get("action") {
                        let method = attrs
                            .get("method")
                            .map(|m| m.to_uppercase())
                            .unwrap_or_else(|| "GET".to_string());
                        push_html_route(out, file_node_id, &method, action, &mut seen_routes);
                    }
                }
                _ => {}
            }

            // htmx: hx-post/hx-get/... — маршрут независимо от имени тега
            for (attr, method) in [
                ("hx-post", "POST"),
                ("hx-get", "GET"),
                ("hx-put", "PUT"),
                ("hx-delete", "DELETE"),
                ("hx-patch", "PATCH"),
            ] {
                if let Some(path) = attrs.get(attr) {
                    push_html_route(out, file_node_id, method, path, &mut seen_routes);
                }
            }
        }

        // class="a b" — отдельный проход по всему файлу (включая шаблоны
        // со смешанным содержимым); `:class="expr"` не матчится (перед class
        // нет пробела-имени атрибута)
        self.collect_class_bindings(file_node_id, &clean, out);
    }

    /// Атрибуты тега → карта имя (нижний регистр) → значение.
    fn parse_html_attrs(&self, attrs: &str) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for cap in self.html_attr_re.captures_iter(attrs) {
            let Some(name) = cap.get(1) else { continue };
            let value = cap
                .get(3)
                .or_else(|| cap.get(4))
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            map.insert(name.as_str().to_lowercase(), value);
        }
        map
    }

    /// Ф48.1/48.3: `class="a b"` → `BINDS_CLASS` (INFERRED) к `cssclass:{name}`,
    /// дедуп по имени класса в пределах файла.
    fn collect_class_bindings(&self, file_node_id: &str, text: &str, out: &mut AstScanResult) {
        let mut seen: HashSet<String> = HashSet::new();
        for cap in self.html_class_re.captures_iter(text) {
            let value = cap
                .get(1)
                .or_else(|| cap.get(2))
                .map(|m| m.as_str())
                .unwrap_or("");
            for name in value.split_whitespace() {
                if name.is_empty() || !seen.insert(name.to_string()) {
                    continue;
                }
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("cssclass:{}", name),
                    label: "BINDS_CLASS".to_string(),
                    weight: 1.0,
                    context: format!("class=\"{}\"", value),
                    provenance: "INFERRED".to_string(),
                });
            }
        }
    }

    /// Ф48.2: CSS/SCSS — селекторы `.foo` → `CssClass`, `#bar` → `CssId` (по
    /// одному узлу на имя на файл, дедуп), `@import` → `IMPORTS`,
    /// `--custom-prop:` → `CssVar`; `@media/@supports` без узлов (шум),
    /// вложенность/миксины SCSS не резолвим (бэклог). line_offset — для
    /// `<style>`-блоков SFC (Ф48.3).
    fn parse_css(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        line_offset: usize,
        out: &mut AstScanResult,
    ) {
        // 1. @import (цель: расширение стиля → file:, иначе css:)
        for cap in self.css_import_re.captures_iter(content) {
            let target_raw = cap
                .get(1)
                .or_else(|| cap.get(2))
                .map(|m| m.as_str().trim().to_string());
            if let Some(target) = target_raw {
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: stylesheet_target(&target),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("@import \"{}\"", target),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 2. Селекторы: .class / #id — один узел на имя на файл (48.5)
        let mut seen_classes: HashMap<String, usize> = HashMap::new();
        let mut seen_ids: HashMap<String, usize> = HashMap::new();
        for cap in self.css_selector_re.captures_iter(content) {
            let Some(sel) = cap.get(1) else { continue };
            let line = content[..sel.start()].matches('\n').count() + 1 + line_offset;
            for ccap in self.css_sel_class_re.captures_iter(sel.as_str()) {
                if let Some(name) = ccap.get(1) {
                    let name = name.as_str().to_string();
                    seen_classes.entry(name).or_insert(line);
                }
            }
            for icap in self.css_sel_id_re.captures_iter(sel.as_str()) {
                if let Some(name) = icap.get(1) {
                    let name = name.as_str().to_string();
                    seen_ids.entry(name).or_insert(line);
                }
            }
        }
        for (name, line) in seen_classes {
            let node_id = format!("cssclass:{}", name);
            out.nodes.push(AstNode {
                node_id: node_id.clone(),
                label: name.clone(),
                node_type: "CssClass".to_string(),
                description: format!("CSS-класс `.{}` в {}:{}", name, rel_path, line),
                file_path: rel_path.to_string(),
                line_start: line,
                line_end: line,
            });
            out.edges.push(AstEdge {
                source_node_id: file_node_id.to_string(),
                target_node_id: node_id,
                label: "DEFINES".to_string(),
                weight: 1.0,
                context: format!(".{}", name),
                provenance: "EXTRACTED".to_string(),
            });
        }
        for (name, line) in seen_ids {
            let node_id = format!("cssid:{}", name);
            out.nodes.push(AstNode {
                node_id: node_id.clone(),
                label: name.clone(),
                node_type: "CssId".to_string(),
                description: format!("CSS-идентификатор `#{}` в {}:{}", name, rel_path, line),
                file_path: rel_path.to_string(),
                line_start: line,
                line_end: line,
            });
            out.edges.push(AstEdge {
                source_node_id: file_node_id.to_string(),
                target_node_id: node_id,
                label: "DEFINES".to_string(),
                weight: 1.0,
                context: format!("#{}", name),
                provenance: "EXTRACTED".to_string(),
            });
        }

        // 3. --custom-prop: value → CssVar (DEFINES); использования var(--x)
        // без двоеточия не матчатся
        let mut seen_vars: HashMap<String, usize> = HashMap::new();
        for cap in self.css_var_re.captures_iter(content) {
            if let Some(name) = cap.get(1) {
                let line = content[..cap.get(0).map(|m| m.start()).unwrap_or(0)]
                    .matches('\n')
                    .count()
                    + 1
                    + line_offset;
                seen_vars.entry(name.as_str().to_string()).or_insert(line);
            }
        }
        for (name, line) in seen_vars {
            let node_id = format!("cssvar:{}", name);
            out.nodes.push(AstNode {
                node_id: node_id.clone(),
                label: name.clone(),
                node_type: "CssVar".to_string(),
                description: format!("CSS-переменная `--{}` в {}:{}", name, rel_path, line),
                file_path: rel_path.to_string(),
                line_start: line,
                line_end: line,
            });
            out.edges.push(AstEdge {
                source_node_id: file_node_id.to_string(),
                target_node_id: node_id,
                label: "DEFINES".to_string(),
                weight: 1.0,
                context: format!("--{}", name),
                provenance: "EXTRACTED".to_string(),
            });
        }
    }

    /// Ф52.1: Jupyter `.ipynb` — код code-ячеек через parse_python со
    /// сквозным line_offset (номера строк — по склеенному коду ячеек).
    /// `outputs` не читаются (base64-картинки — риск памяти), cap 5 МБ.
    fn parse_notebook(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(content) else {
            return; // битый JSON — остаётся только File-узел от parse_file
        };
        let Some(cells) = v.get("cells").and_then(|c| c.as_array()) else {
            return; // нет cells — только File-узел (52.1)
        };

        let mut code_cells = 0usize;
        let mut offset = 0usize;
        for cell in cells {
            if cell.get("cell_type").and_then(|t| t.as_str()) != Some("code") {
                continue; // markdown/raw — не код
            }
            // source: массив строк или одна строка; outputs не трогаем
            let code = match cell.get("source") {
                Some(serde_json::Value::Array(lines)) => lines
                    .iter()
                    .filter_map(|l| l.as_str())
                    .collect::<Vec<_>>()
                    .join(""),
                Some(serde_json::Value::String(s)) => s.clone(),
                _ => continue,
            };
            if code.trim().is_empty() {
                continue;
            }
            code_cells += 1;
            self.parse_python(rel_path, file_node_id, &code, offset, out);
            offset += code.lines().count() + 1; // +1 пустая строка между ячейками
        }

        if code_cells > 0 {
            let node_id = format!("notebook:{}", rel_path);
            let label = Path::new(rel_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| rel_path.to_string());
            out.nodes.push(AstNode {
                node_id: node_id.clone(),
                label,
                node_type: "Notebook".to_string(),
                description: format!(
                    "Jupyter ноутбук `{}` (code-ячеек: {}) в {}",
                    rel_path, code_cells, rel_path
                ),
                file_path: rel_path.to_string(),
                line_start: 1,
                line_end: offset.max(1),
            });
            out.edges.push(AstEdge {
                source_node_id: file_node_id.to_string(),
                target_node_id: node_id,
                label: "DEFINES".to_string(),
                weight: 1.0,
                context: format!("notebook ({} code cells)", code_cells),
                provenance: "EXTRACTED".to_string(),
            });
        }
    }

    /// Ф52.2: Solidity — `contract C is A, B` → Class (+INHERITS первой базе,
    /// IMPLEMENTS остальным, как в PHP/Java), `interface` → Interface,
    /// `library` → Module, `function`/`modifier`/`event`/`error`/`constructor`
    /// → Function (тип в description), `mapping(...)` → Mapping (DEFINES),
    /// `import "x.sol"` → IMPORTS, `using X for Y` → USES_SCHEMA.
    fn parse_solidity(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. import "x.sol"
        for cap in self.sol_import_re.captures_iter(content) {
            if let Some(m) = cap.get(1) {
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", m.as_str()),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("import \"{}\"", m.as_str()),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 2. Контракты/интерфейсы/библиотеки; `is A, B` — первая база INHERITS,
        // остальные IMPLEMENTS (как PHP extends/implements)
        let push_bases = |out: &mut AstScanResult, node_id: &str, bases: &str| {
            for (k, base) in bases
                .split(',')
                .map(str::trim)
                .filter(|b| !b.is_empty())
                .enumerate()
            {
                out.edges.push(AstEdge {
                    source_node_id: node_id.to_string(),
                    target_node_id: if k == 0 {
                        format!("class:{}", base)
                    } else {
                        format!("interface:{}", base)
                    },
                    label: if k == 0 { "INHERITS" } else { "IMPLEMENTS" }.to_string(),
                    weight: 1.0,
                    context: format!("is {}", base),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        };
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.sol_contract_re.captures(line) {
                let Some(name) = cap.get(1) else { continue };
                let node_id = format!("class:{}:{}", rel_path, name.as_str());
                out.nodes.push(AstNode {
                    node_id: node_id.clone(),
                    label: name.as_str().to_string(),
                    node_type: "Class".to_string(),
                    description: format!(
                        "Solidity контракт `{}` в {}",
                        name.as_str(),
                        rel_path
                    ),
                    file_path: rel_path.to_string(),
                    line_start: i + 1,
                    line_end: i + 1,
                });
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: node_id.clone(),
                    label: "DEFINES".to_string(),
                    weight: 1.0,
                    context: line.trim().to_string(),
                    provenance: "EXTRACTED".to_string(),
                });
                if let Some(bases) = cap.get(2) {
                    push_bases(out, &node_id, bases.as_str());
                }
                continue;
            }
            if let Some(cap) = self.sol_interface_re.captures(line) {
                let Some(name) = cap.get(1) else { continue };
                let node_id = format!("interface:{}:{}", rel_path, name.as_str());
                out.nodes.push(AstNode {
                    node_id: node_id.clone(),
                    label: name.as_str().to_string(),
                    node_type: "Interface".to_string(),
                    description: format!(
                        "Solidity интерфейс `{}` в {}",
                        name.as_str(),
                        rel_path
                    ),
                    file_path: rel_path.to_string(),
                    line_start: i + 1,
                    line_end: i + 1,
                });
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: node_id.clone(),
                    label: "DEFINES".to_string(),
                    weight: 1.0,
                    context: line.trim().to_string(),
                    provenance: "EXTRACTED".to_string(),
                });
                if let Some(bases) = cap.get(2) {
                    push_bases(out, &node_id, bases.as_str());
                }
                continue;
            }
            if let Some(cap) = self.sol_library_re.captures(line) {
                let Some(name) = cap.get(1) else { continue };
                let node_id = format!("module:{}:{}", rel_path, name.as_str());
                out.nodes.push(AstNode {
                    node_id: node_id.clone(),
                    label: name.as_str().to_string(),
                    node_type: "Module".to_string(),
                    description: format!(
                        "Solidity библиотека `{}` в {}",
                        name.as_str(),
                        rel_path
                    ),
                    file_path: rel_path.to_string(),
                    line_start: i + 1,
                    line_end: i + 1,
                });
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: node_id,
                    label: "DEFINES".to_string(),
                    weight: 1.0,
                    context: line.trim().to_string(),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 3. function/modifier/event/error/constructor → Function
        for (i, line) in lines.iter().enumerate() {
            let (name, kind) = if let Some(cap) = self.sol_fn_re.captures(line) {
                match (cap.get(1), cap.get(2)) {
                    (Some(k), Some(n)) => (n.as_str().to_string(), k.as_str().to_string()),
                    _ => continue,
                }
            } else if let Some(cap) = self.sol_special_re.captures(line) {
                match (cap.get(1), cap.get(2)) {
                    // constructor без имени: label = kind
                    (Some(k), Some(n)) if !n.is_empty() => (n.as_str().to_string(), k.as_str().to_string()),
                    (Some(k), _) => (k.as_str().to_string(), k.as_str().to_string()),
                    _ => continue,
                }
            } else {
                continue;
            };
            let node_id = format!("fn:{}:{}", rel_path, name);
            out.nodes.push(AstNode {
                node_id: node_id.clone(),
                label: name.clone(),
                node_type: "Function".to_string(),
                description: format!(
                    "Solidity {} `{}` в {}",
                    kind, name, rel_path
                ),
                file_path: rel_path.to_string(),
                line_start: i + 1,
                line_end: i + 1,
            });
            out.edges.push(AstEdge {
                source_node_id: file_node_id.to_string(),
                target_node_id: node_id,
                label: "DEFINES".to_string(),
                weight: 1.0,
                context: line.trim().to_string(),
                provenance: "EXTRACTED".to_string(),
            });
        }

        // 4. mapping(... <vis> name;) → Mapping-узел (один на имя на файл)
        let mut seen_mappings: HashSet<String> = HashSet::new();
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.sol_mapping_re.captures(line) {
                if let Some(name) = cap.get(1) {
                    if seen_mappings.insert(name.as_str().to_string()) {
                        let node_id = format!("mapping:{}:{}", rel_path, name.as_str());
                        out.nodes.push(AstNode {
                            node_id: node_id.clone(),
                            label: name.as_str().to_string(),
                            node_type: "Mapping".to_string(),
                            description: format!(
                                "Solidity mapping `{}` в {}:{}",
                                name.as_str(),
                                rel_path,
                                i + 1
                            ),
                            file_path: rel_path.to_string(),
                            line_start: i + 1,
                            line_end: i + 1,
                        });
                        out.edges.push(AstEdge {
                            source_node_id: file_node_id.to_string(),
                            target_node_id: node_id,
                            label: "DEFINES".to_string(),
                            weight: 1.0,
                            context: line.trim().to_string(),
                            provenance: "EXTRACTED".to_string(),
                        });
                    }
                }
            }
        }

        // 5. using X for Y → USES_SCHEMA (X — обычно библиотека)
        for cap in self.sol_using_re.captures_iter(content) {
            if let (Some(lib), Some(_ty)) = (cap.get(1), cap.get(2)) {
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", lib.as_str()),
                    label: "USES_SCHEMA".to_string(),
                    weight: 1.0,
                    context: format!(
                        "using {} for {}",
                        lib.as_str(),
                        cap.get(2).map(|m| m.as_str()).unwrap_or("")
                    ),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }
    }

    /// Ф52.3: Zig — `pub fn`/`export fn`/`fn`/`inline fn` → Function,
    /// `const X = struct/enum/union/opaque` → Struct, `@import("std")` →
    /// pkg:zig:std / `@import("mod.zig")` → file:, `test "name"` → Function с
    /// label `test_{name}` (префикс test_ выводит из dead-code, entrypoint_reason),
    /// top-level `pub const/var` (не структуры) → Variable.
    fn parse_zig(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();
        let mut defined_names: HashSet<String> = HashSet::new();

        // 1. @import
        for cap in self.zig_import_re.captures_iter(content) {
            if let Some(m) = cap.get(1) {
                let target = if m.as_str() == "std" {
                    "pkg:zig:std".to_string()
                } else if m.as_str().ends_with(".zig") {
                    format!("file:{}", m.as_str())
                } else {
                    format!("module:{}", m.as_str())
                };
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: target,
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("@import(\"{}\")", m.as_str()),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 2. top-level определения: fn, const/var-структуры, pub const/var
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.zig_fn_re.captures(line) {
                if let Some(name) = cap.get(1) {
                    let node_id = format!("fn:{}:{}", rel_path, name.as_str());
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: name.as_str().to_string(),
                        node_type: "Function".to_string(),
                        description: format!(
                            "Zig функция `{}` ({}) в {}",
                            name.as_str(),
                            line.trim().trim_end_matches('{').trim(),
                            rel_path
                        ),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                    defined_names.insert(name.as_str().to_string());
                    continue;
                }
            }
            if let Some(cap) = self.zig_test_re.captures(line) {
                if let Some(name) = cap.get(1) {
                    // label test_{name}: существующее правило entrypoint_reason
                    // (starts_with "test_") выводит тесты из dead-code
                    let label = format!("test_{}", name.as_str());
                    let node_id = format!("fn:{}:{}", rel_path, label);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label,
                        node_type: "Function".to_string(),
                        description: format!(
                            "Zig тест \"{}\" в {}",
                            name.as_str(),
                            rel_path
                        ),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                    continue;
                }
            }
            if let Some(cap) = self.zig_type_re.captures(line) {
                if let Some(name) = cap.get(1) {
                    let node_id = format!("struct:{}:{}", rel_path, name.as_str());
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: name.as_str().to_string(),
                        node_type: "Struct".to_string(),
                        description: format!(
                            "Zig {} `{}` в {}",
                            cap.get(2).map(|m| m.as_str()).unwrap_or("type"),
                            name.as_str(),
                            rel_path
                        ),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                    defined_names.insert(name.as_str().to_string());
                    continue;
                }
            }
            // top-level переменные — только pub (52.3)
            if let Some(cap) = self.zig_pub_var_re.captures(line) {
                if let Some(name) = cap.get(1) {
                    if defined_names.insert(name.as_str().to_string()) {
                        let node_id = format!("variable:{}:{}", rel_path, name.as_str());
                        out.nodes.push(AstNode {
                            node_id: node_id.clone(),
                            label: name.as_str().to_string(),
                            node_type: "Variable".to_string(),
                            description: format!(
                                "Zig pub const/var `{}` в {}",
                                name.as_str(),
                                rel_path
                            ),
                            file_path: rel_path.to_string(),
                            line_start: i + 1,
                            line_end: i + 1,
                        });
                        out.edges.push(AstEdge {
                            source_node_id: file_node_id.to_string(),
                            target_node_id: node_id,
                            label: "DEFINES".to_string(),
                            weight: 1.0,
                            context: line.trim().to_string(),
                            provenance: "EXTRACTED".to_string(),
                        });
                    }
                }
            }
        }
    }

    /// Ф49.1: Protocol Buffers — package → Module, service → Service,
    /// rpc → Function (DEFINES service→rpc), message → Struct, enum → Enum,
    /// import → IMPORTS, типы полей → USES_SCHEMA (объявленные в файле — к
    /// struct/enum-узлу, прочие — ExternalModule); option-строки — в
    /// description package-узла.
    fn parse_proto(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // package + option-строки (в description package-узла)
        let mut options: Vec<String> = Vec::new();
        for line in lines.iter() {
            let t = line.trim();
            if t.starts_with("option ") && t.ends_with(';') {
                options.push(t.to_string());
            }
        }
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.proto_package_re.captures(line) {
                if let Some(pkg) = cap.get(1) {
                    let node_id = format!("module:{}:{}", rel_path, pkg.as_str());
                    let mut description = format!("Protobuf пакет `{}` в {}", pkg.as_str(), rel_path);
                    if !options.is_empty() {
                        description.push_str("; ");
                        description.push_str(&options.join("; "));
                    }
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: pkg.as_str().to_string(),
                        node_type: "Module".to_string(),
                        description,
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        // import
        for cap in self.proto_import_re.captures_iter(content) {
            if let Some(m) = cap.get(1) {
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", m.as_str()),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("import \"{}\"", m.as_str()),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // service/rpc/message/enum — одним проходом по строкам
        let mut last_service: Option<String> = None;
        let mut declared: HashMap<String, String> = HashMap::new(); // имя → node_id
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.proto_service_re.captures(line) {
                if let Some(name) = cap.get(1) {
                    let node_id = format!("service:{}:{}", rel_path, name.as_str());
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: name.as_str().to_string(),
                        node_type: "Service".to_string(),
                        description: format!(
                            "Protobuf сервис `{}` в {}",
                            name.as_str(),
                            rel_path
                        ),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                    last_service = Some(node_id.clone());
                    declared.insert(name.as_str().to_string(), node_id);
                    continue;
                }
            }
            if let Some(cap) = self.proto_rpc_re.captures(line) {
                if let Some(name) = cap.get(1) {
                    let node_id = format!("fn:{}:{}", rel_path, name.as_str());
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: name.as_str().to_string(),
                        node_type: "Function".to_string(),
                        description: format!(
                            "Protobuf rpc `{}` в {}",
                            name.as_str(),
                            rel_path
                        ),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                    if let Some(svc) = &last_service {
                        out.edges.push(AstEdge {
                            source_node_id: svc.clone(),
                            target_node_id: node_id.clone(),
                            label: "DEFINES".to_string(),
                            weight: 1.0,
                            context: format!("rpc {}", name.as_str()),
                            provenance: "EXTRACTED".to_string(),
                        });
                    }
                    continue;
                }
            }
            if let Some(cap) = self.proto_message_re.captures(line) {
                if let Some(name) = cap.get(1) {
                    let node_id = format!("struct:{}:{}", rel_path, name.as_str());
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: name.as_str().to_string(),
                        node_type: "Struct".to_string(),
                        description: format!(
                            "Protobuf message `{}` в {}",
                            name.as_str(),
                            rel_path
                        ),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                    declared.insert(name.as_str().to_string(), node_id);
                    continue;
                }
            }
            if let Some(cap) = self.proto_enum_re.captures(line) {
                if let Some(name) = cap.get(1) {
                    let node_id = format!("enum:{}:{}", rel_path, name.as_str());
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: name.as_str().to_string(),
                        node_type: "Enum".to_string(),
                        description: format!(
                            "Protobuf enum `{}` в {}",
                            name.as_str(),
                            rel_path
                        ),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                    declared.insert(name.as_str().to_string(), node_id);
                }
            }
        }

        // Типы полей сообщений → USES_SCHEMA (объявленные здесь — к узлу,
        // прочие — ExternalModule). Второй проход: объявленные уже собраны.
        let mut seen: HashSet<String> = HashSet::new();
        for cap in self.proto_field_re.captures_iter(content) {
            if let Some(ty) = cap.get(1) {
                let ty = ty.as_str();
                if ty.starts_with('.') || !seen.insert(ty.to_string()) {
                    continue;
                }
                let target = declared.get(ty).cloned().unwrap_or(format!("module:{}", ty));
                // источник — последний message, объявленный до использования
                let src = declared
                    .values()
                    .next()
                    .cloned()
                    .unwrap_or(file_node_id.to_string());
                out.edges.push(AstEdge {
                    source_node_id: src,
                    target_node_id: target,
                    label: "USES_SCHEMA".to_string(),
                    weight: 1.0,
                    context: format!("field type {}", ty),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }
    }

    /// Ф49.2: GraphQL — type/input → Struct (input помечен), interface →
    /// Interface, enum → Enum, schema{} → Schema; поля Query/Mutation/
    /// Subscription → Function (метка в description); `extend type` — узел не
    /// создаём (мерж в базовый type).
    fn parse_graphql(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. schema { query: Q ... }
        for (i, line) in lines.iter().enumerate() {
            if self.gql_schema_re.is_match(line) {
                let node_id = format!("schema:{}", rel_path);
                out.nodes.push(AstNode {
                    node_id: node_id.clone(),
                    label: rel_path.to_string(),
                    node_type: "Schema".to_string(),
                    description: format!("GraphQL schema-определение в {}", rel_path),
                    file_path: rel_path.to_string(),
                    line_start: i + 1,
                    line_end: i + 1,
                });
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: node_id,
                    label: "DEFINES".to_string(),
                    weight: 1.0,
                    context: "schema {".to_string(),
                    provenance: "EXTRACTED".to_string(),
                });
                break;
            }
        }

        // 2. Поля Query/Mutation/Subscription → Function
        for cap in self.gql_query_block_re.captures_iter(content) {
            let Some(kind) = cap.get(1).map(|m| m.as_str().to_string()) else { continue };
            let Some(body) = cap.get(2) else { continue };
            let offset = content[..body.start()].matches('\n').count();
            for (k, line) in body.as_str().lines().enumerate() {
                if let Some(cap2) = self.gql_field_re.captures(line) {
                    if let Some(field) = cap2.get(1) {
                        let node_id = format!("fn:{}:{}", rel_path, field.as_str());
                        out.nodes.push(AstNode {
                            node_id: node_id.clone(),
                            label: field.as_str().to_string(),
                            node_type: "Function".to_string(),
                            description: format!(
                                "GraphQL {}-поле `{}` в {}",
                                kind,
                                field.as_str(),
                                rel_path
                            ),
                            file_path: rel_path.to_string(),
                            line_start: offset + k + 1,
                            line_end: offset + k + 1,
                        });
                        out.edges.push(AstEdge {
                            source_node_id: file_node_id.to_string(),
                            target_node_id: node_id,
                            label: "DEFINES".to_string(),
                            weight: 1.0,
                            context: format!("{}: {}", kind.to_lowercase(), field.as_str()),
                            provenance: "EXTRACTED".to_string(),
                        });
                    }
                }
            }
        }

        // 3. type/input/interface/enum — типы (кроме Query/Mutation/Subscription)
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.gql_type_re.captures(line) {
                if let Some(name) = cap.get(1) {
                    let name = name.as_str();
                    if matches!(name, "Query" | "Mutation" | "Subscription") {
                        continue; // поля уже Function-узлы (шаг 2)
                    }
                    let is_input = line.trim_start().starts_with("input");
                    let node_id = format!("struct:{}:{}", rel_path, name);
                    let mut description = format!("GraphQL type `{}` в {}", name, rel_path);
                    if is_input {
                        description.push_str(" (input)");
                    }
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: name.to_string(),
                        node_type: "Struct".to_string(),
                        description,
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                    continue;
                }
            }
            if let Some(cap) = self.gql_interface_re.captures(line) {
                if let Some(name) = cap.get(1) {
                    let node_id = format!("interface:{}:{}", rel_path, name.as_str());
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: name.as_str().to_string(),
                        node_type: "Interface".to_string(),
                        description: format!(
                            "GraphQL interface `{}` в {}",
                            name.as_str(),
                            rel_path
                        ),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                    continue;
                }
            }
            if let Some(cap) = self.gql_enum_re.captures(line) {
                if let Some(name) = cap.get(1) {
                    let node_id = format!("enum:{}:{}", rel_path, name.as_str());
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: name.as_str().to_string(),
                        node_type: "Enum".to_string(),
                        description: format!(
                            "GraphQL enum `{}` в {}",
                            name.as_str(),
                            rel_path
                        ),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }

    /// Ф49.3: OpenAPI/Swagger (json через serde_json, yaml — serde_yaml_ng, оба
    /// в serde_json::Value) — пути×методы → Endpoint + парный `route:`-узел
    /// (тот же node_id, что у серверных роутов Ф40.2 — мост (а) получается
    /// автоматически), components/schemas → Struct, $ref → USES_SCHEMA,
    /// parameters/requestBody → Field. info.title/version — в description
    /// File-узла. Возвращает true, если это был OpenAPI-документ.
    fn parse_openapi(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) -> bool {
        let value: serde_json::Value = if let Ok(v) = serde_json::from_str::<serde_json::Value>(content)
        {
            v
        } else if let Ok(v) = serde_yaml_ng::from_str::<serde_json::Value>(content) {
            v
        } else {
            return false;
        };

        // Классификатор (50.5): характерный корневой ключ
        let is_swagger = value.get("openapi").is_some() || value.get("swagger").is_some();
        if !is_swagger {
            return false;
        }

        // info.title/version → description File-узла (узел создан parse_file)
        let mut info_note = String::new();
        if let Some(info) = value.get("info") {
            let title = info.get("title").and_then(|t| t.as_str()).unwrap_or("");
            let version = info.get("version").and_then(|v| v.as_str()).unwrap_or("");
            if !title.is_empty() || !version.is_empty() {
                info_note = format!("OpenAPI: {} {}", title, version);
                for node in out.nodes.iter_mut() {
                    if node.node_id == file_node_id {
                        node.description.push_str("; ");
                        node.description.push_str(&info_note);
                        break;
                    }
                }
            }
        }

        // paths → Endpoint + route:
        const HTTP_METHODS: &[&str] = &["get", "post", "put", "delete", "patch", "head", "options"];
        let mut schema_nodes: HashMap<String, String> = HashMap::new();
        if let Some(paths) = value.get("paths").and_then(|p| p.as_object()) {
            for (path, item) in paths {
                for method in HTTP_METHODS {
                    let Some(op) = item.get(*method) else { continue };
                    let method_up = method.to_uppercase();
                    let endpoint_id = format!("endpoint:{}:{} {}", rel_path, method_up, path);
                    let route_id = format!("route:{} {}", method_up, path);
                    let op_desc = op
                        .get("summary")
                        .and_then(|s| s.as_str())
                        .unwrap_or_default();
                    out.nodes.push(AstNode {
                        node_id: endpoint_id.clone(),
                        label: format!("{} {}", method_up, path),
                        node_type: "Endpoint".to_string(),
                        description: format!(
                            "OpenAPI эндпоинт `{} {}`{} в {}",
                            method_up,
                            path,
                            if op_desc.is_empty() {
                                String::new()
                            } else {
                                format!(" — {}", op_desc)
                            },
                            rel_path
                        ),
                        file_path: rel_path.to_string(),
                        line_start: 1,
                        line_end: 1,
                    });
                    out.nodes.push(AstNode {
                        node_id: route_id.clone(),
                        label: format!("{} {}", method_up, path),
                        node_type: "Route".to_string(),
                        description: format!(
                            "HTTP маршрут `{} {}` (контракт {}){}",
                            method_up,
                            path,
                            rel_path,
                            if info_note.is_empty() {
                                String::new()
                            } else {
                                format!("; {}", info_note)
                            }
                        ),
                        file_path: rel_path.to_string(),
                        line_start: 1,
                        line_end: 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: endpoint_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: format!("{} {}", method_up, path),
                        provenance: "EXTRACTED".to_string(),
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: route_id.clone(),
                        label: "ROUTE".to_string(),
                        weight: 1.0,
                        context: format!("openapi {} {}", method_up, path),
                        provenance: "INFERRED".to_string(),
                    });
                    out.edges.push(AstEdge {
                        source_node_id: route_id,
                        target_node_id: endpoint_id.clone(),
                        label: "SERVES_ENDPOINT".to_string(),
                        weight: 1.0,
                        context: format!("openapi {} {}", method_up, path),
                        provenance: "EXTRACTED".to_string(),
                    });

                    // parameters/requestBody → Field
                    if let Some(params) = op.get("parameters").and_then(|p| p.as_array()) {
                        for p in params {
                            let Some(name) = p.get("name").and_then(|n| n.as_str()) else {
                                continue;
                            };
                            let field_id = format!("field:{}:{}:{}", rel_path, path, name);
                            out.nodes.push(AstNode {
                                node_id: field_id.clone(),
                                label: name.to_string(),
                                node_type: "Field".to_string(),
                                description: format!(
                                    "Параметр `{}` ({} {}){} в {}",
                                    name,
                                    method_up,
                                    path,
                                    p.get("in")
                                        .and_then(|i| i.as_str())
                                        .map(|i| format!(" in {}", i))
                                        .unwrap_or_default(),
                                    rel_path
                                ),
                                file_path: rel_path.to_string(),
                                line_start: 1,
                                line_end: 1,
                            });
                            out.edges.push(AstEdge {
                                source_node_id: endpoint_id.clone(),
                                target_node_id: field_id,
                                label: "DEFINES".to_string(),
                                weight: 1.0,
                                context: format!("parameter {}", name),
                                provenance: "EXTRACTED".to_string(),
                            });
                        }
                    }
                    if let Some(body) = op.get("requestBody") {
                        let field_id = format!("field:{}:{}:body", rel_path, path);
                        out.nodes.push(AstNode {
                            node_id: field_id.clone(),
                            label: "body".to_string(),
                            node_type: "Field".to_string(),
                            description: format!(
                                "Request body `{}` в {}",
                                path, rel_path
                            ),
                            file_path: rel_path.to_string(),
                            line_start: 1,
                            line_end: 1,
                        });
                        out.edges.push(AstEdge {
                            source_node_id: endpoint_id.clone(),
                            target_node_id: field_id.clone(),
                            label: "DEFINES".to_string(),
                            weight: 1.0,
                            context: format!("requestBody {}", path),
                            provenance: "EXTRACTED".to_string(),
                        });
                        // $ref тела → USES_SCHEMA
                        if let Some(schema_name) = find_ref_name(body) {
                            let target = schema_nodes.get(&schema_name).cloned().unwrap_or(
                                format!("struct:{}:{}", rel_path, schema_name),
                            );
                            out.edges.push(AstEdge {
                                source_node_id: field_id,
                                target_node_id: target,
                                label: "USES_SCHEMA".to_string(),
                                weight: 1.0,
                                context: format!("$ref {}", schema_name),
                                provenance: "EXTRACTED".to_string(),
                            });
                        }
                    }
                }
            }
        }

        // components/schemas → Struct
        if let Some(schemas) = value
            .get("components")
            .and_then(|c| c.get("schemas"))
            .and_then(|s| s.as_object())
        {
            for (name, schema) in schemas {
                let node_id = format!("struct:{}:{}", rel_path, name);
                out.nodes.push(AstNode {
                    node_id: node_id.clone(),
                    label: name.clone(),
                    node_type: "Struct".to_string(),
                    description: format!("OpenAPI schema `{}` в {}", name, rel_path),
                    file_path: rel_path.to_string(),
                    line_start: 1,
                    line_end: 1,
                });
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: node_id.clone(),
                    label: "DEFINES".to_string(),
                    weight: 1.0,
                    context: format!("schema {}", name),
                    provenance: "EXTRACTED".to_string(),
                });
                schema_nodes.insert(name.clone(), node_id.clone());
                // свойства схемы с $ref → USES_SCHEMA
                if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
                    for (prop_name, prop) in props {
                        if let Some(ref_name) = find_ref_name(prop) {
                            let target = schema_nodes
                                .get(&ref_name)
                                .cloned()
                                .unwrap_or(format!("struct:{}:{}", rel_path, ref_name));
                            out.edges.push(AstEdge {
                                source_node_id: node_id.clone(),
                                target_node_id: target,
                                label: "USES_SCHEMA".to_string(),
                                weight: 1.0,
                                context: format!("property {} $ref {}", prop_name, ref_name),
                                provenance: "EXTRACTED".to_string(),
                            });
                        }
                    }
                }
            }
        }

        true
    }

    /// Ф50.1: Terraform/OpenTofu (регэксп+блочный сканер, без hcl-rs,
    /// ADR-AST-1) — resource/data → Resource/DataSource (провайдер из префикса
    /// типа в description), module → Module + DEPENDS_ON (локальный source →
    /// file:, внешний — module:), variable/output → Variable/Output,
    /// depends_on = [a.b] → PendingRef (резолв по type.name по всему скану).
    fn parse_terraform(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();
        let mut defined_resources: Vec<(String, String)> = Vec::new(); // "type.name" → node_id

        for (i, line) in lines.iter().enumerate() {
            let Some(cap) = self.tf_block_re.captures(line) else {
                continue;
            };
            let Some(kind) = cap.get(1) else { continue };
            let arg1 = cap.get(2).map(|m| m.as_str()).unwrap_or("");
            let arg2 = cap.get(3).map(|m| m.as_str()).unwrap_or("");
            let (node_id, node_type, label, description) = match kind.as_str() {
                "resource" => {
                    let provider = arg1.split('_').next().unwrap_or("").to_string();
                    (
                        format!("resource:{}:{}.{}", rel_path, arg1, arg2),
                        "Resource",
                        format!("{}.{}", arg1, arg2),
                        format!(
                            "Terraform resource `{}.{}`{} в {}",
                            arg1,
                            arg2,
                            if provider.is_empty() {
                                String::new()
                            } else {
                                format!(" (провайдер: {})", provider)
                            },
                            rel_path
                        ),
                    )
                }
                "data" => (
                    format!("resource:{}:{}.{}", rel_path, arg1, arg2),
                    "DataSource",
                    format!("{}.{}", arg1, arg2),
                    format!("Terraform data `{}` в {}", arg1, rel_path),
                ),
                "module" => (
                    format!("module:{}:{}", rel_path, arg1),
                    "Module",
                    arg1.to_string(),
                    format!("Terraform module `{}` в {}", arg1, rel_path),
                ),
                "variable" => (
                    format!("variable:{}:{}", rel_path, arg1),
                    "Variable",
                    arg1.to_string(),
                    format!("Terraform variable `{}` в {}", arg1, rel_path),
                ),
                "output" => (
                    format!("output:{}:{}", rel_path, arg1),
                    "Output",
                    arg1.to_string(),
                    format!("Terraform output `{}` в {}", arg1, rel_path),
                ),
                _ => continue,
            };

            out.nodes.push(AstNode {
                node_id: node_id.clone(),
                label: label.clone(),
                node_type: node_type.to_string(),
                description,
                file_path: rel_path.to_string(),
                line_start: i + 1,
                line_end: i + 1,
            });
            out.edges.push(AstEdge {
                source_node_id: file_node_id.to_string(),
                target_node_id: node_id.clone(),
                label: "DEFINES".to_string(),
                weight: 1.0,
                context: line.trim().to_string(),
                provenance: "EXTRACTED".to_string(),
            });
            if matches!(kind.as_str(), "resource" | "data") {
                defined_resources.push((label.clone(), node_id.clone()));
            }

            // module "n" { source = ... } — DEPENDS_ON к file:/module:
            if kind.as_str() == "module" {
                if let Some(src) = self.tf_source_re.captures(
                    &content[cap.get(0).map(|m| m.start()).unwrap_or(0)..],
                ) {
                    let s = src.get(1).map(|m| m.as_str()).unwrap_or("");
                    let target = if s.starts_with("./") || s.starts_with("../") {
                        format!(
                            "file:{}",
                            s.trim_start_matches("./")
                                .trim_end_matches('/')
                                .trim_start_matches("../")
                        )
                    } else {
                        // внешний source (registry/ git) — ExternalModule
                        format!("module:{}", s)
                    };
                    out.edges.push(AstEdge {
                        source_node_id: node_id.clone(),
                        target_node_id: target,
                        label: "DEPENDS_ON".to_string(),
                        weight: 1.0,
                        context: format!("source = {}", s),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        // depends_on = [a.b, ...] — PendingRef, резолв по type.name по скану
        for cap in self.tf_depends_on_re.captures_iter(content) {
            let body = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            for ref_item in body.split(',') {
                let r = ref_item.trim().trim_matches('"');
                if r.is_empty() || !r.contains('.') {
                    continue;
                }
                // источник — ресурс, содержащий этот depends_on (последний
                // определённый до него упрощённо не ищем: вешаем на файл,
                // если точный владелец не определён)
                let owner = defined_resources
                    .iter()
                    .rev()
                    .find(|(name, _)| content.rfind(&format!("{} ", name)).is_some())
                    .map(|(_, id)| id.clone());
                let Some(src) = owner.or_else(|| Some(file_node_id.to_string())) else {
                    continue;
                };
                let target_suffix = format!(":{}", r);
                out.pending_refs.push(PendingRef {
                    source_node_id: src,
                    target_prefix: "resource:".to_string(),
                    target_suffix,
                    label: "DEPENDS_ON".to_string(),
                    context: format!("depends_on = {}", r),
                });
            }
        }
    }

    /// Ф50.2: Dockerfile — FROM → BaseImage (image:), ARG/ENV → EnvVar
    /// (DEFINES), COPY/ADD локальных путей → DEPENDS_ON к file:, внешние URL —
    /// module:; WORKDIR/EXPOSE/CMD/ENTRYPOINT — в description File-узла;
    /// многоступенчатые сборки — узлы стейджей не создаём.
    fn parse_dockerfile(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let mut notes: Vec<String> = Vec::new();
        let mut seen_images: HashSet<String> = HashSet::new();

        for cap in self.docker_from_re.captures_iter(content) {
            if let Some(img) = cap.get(1) {
                let img = img.as_str();
                if seen_images.insert(img.to_string()) {
                    out.nodes.push(AstNode {
                        node_id: format!("image:{}", img),
                        label: img.to_string(),
                        node_type: "BaseImage".to_string(),
                        description: format!("Базовый образ `{}` (FROM в {})", img, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: 1,
                        line_end: 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: format!("image:{}", img),
                        label: "DEPENDS_ON".to_string(),
                        weight: 1.0,
                        context: format!("FROM {}", img),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        let mut seen_env: HashSet<String> = HashSet::new();
        for cap in self.docker_env_re.captures_iter(content) {
            if let Some(name) = cap.get(1) {
                if seen_env.insert(name.as_str().to_string()) {
                    let node_id = format!("envvar:{}:{}", rel_path, name.as_str());
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: name.as_str().to_string(),
                        node_type: "EnvVar".to_string(),
                        description: format!("Docker переменная `{}` в {}", name.as_str(), rel_path),
                        file_path: rel_path.to_string(),
                        line_start: 1,
                        line_end: 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: format!("ARG/ENV {}", name.as_str()),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        for cap in self.docker_copy_re.captures_iter(content) {
            let Some(rest) = cap.get(1) else { continue };
            for token in rest.as_str().split_whitespace() {
                if token.starts_with('/') && !token.starts_with("//") || !token.contains("://") && token.contains('.') || !token.contains("://") && token.contains('/')
                {
                    // локальный путь (не URL, не флаг)
                    if token.contains("://") || token.starts_with("--") {
                        continue;
                    }
                    let target = format!(
                        "file:{}",
                        token.trim_end_matches('/')
                    );
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: target,
                        label: "DEPENDS_ON".to_string(),
                        weight: 1.0,
                        context: format!("COPY/ADD {}", token),
                        provenance: "EXTRACTED".to_string(),
                    });
                    break; // первый источник (после флагов) — путь в репо
                }
            }
        }

        for line in content.lines() {
            let t = line.trim();
            for kw in ["WORKDIR", "EXPOSE", "CMD", "ENTRYPOINT"] {
                if t.starts_with(kw) {
                    notes.push(t.to_string());
                }
            }
        }
        if !notes.is_empty() {
            for node in out.nodes.iter_mut() {
                if node.node_id == file_node_id {
                    node.description.push_str("; ");
                    node.description.push_str(&notes.join("; "));
                    break;
                }
            }
        }
    }

    /// Разбить YAML на документы (`---`) и распарсить каждый в serde_json::Value.
    fn split_yaml_docs(content: &str) -> Vec<serde_json::Value> {
        content
            .split("\n---")
            .filter_map(|doc| serde_yaml_ng::from_str::<serde_json::Value>(doc).ok())
            .collect()
    }

    /// Ф50.3: docker-compose — services.<name> → Service (svc:), image →
    /// BaseImage + DEPENDS_ON, build → DEPENDS_ON к Dockerfile, depends_on →
    /// DEPENDS_ON (резолв по имени сервиса файла), environment/env_file →
    /// EnvVar, ports → Port (DEFINES), volumes/networks → MOUNTS.
    /// Возвращает true, если файл распознан как compose.
    fn parse_compose(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) -> bool {
        let Ok(root) = serde_yaml_ng::from_str::<serde_json::Value>(content) else {
            return false;
        };
        // Классификатор (50.5): характерный корневой ключ
        let Some(services) = root.get("services").and_then(|s| s.as_object()) else {
            return false;
        };

        for (name, svc) in services {
            let svc_id = format!("svc:{}:{}", rel_path, name);
            out.nodes.push(AstNode {
                node_id: svc_id.clone(),
                label: name.clone(),
                node_type: "Service".to_string(),
                description: format!("Compose-сервис `{}` в {}", name, rel_path),
                file_path: rel_path.to_string(),
                line_start: 1,
                line_end: 1,
            });
            out.edges.push(AstEdge {
                source_node_id: file_node_id.to_string(),
                target_node_id: svc_id.clone(),
                label: "DEFINES".to_string(),
                weight: 1.0,
                context: format!("service {}", name),
                provenance: "EXTRACTED".to_string(),
            });

            // image → BaseImage
            if let Some(img) = svc.get("image").and_then(|i| i.as_str()) {
                out.nodes.push(AstNode {
                    node_id: format!("image:{}", img),
                    label: img.to_string(),
                    node_type: "BaseImage".to_string(),
                    description: format!("Образ `{}` (image в {})", img, rel_path),
                    file_path: rel_path.to_string(),
                    line_start: 1,
                    line_end: 1,
                });
                out.edges.push(AstEdge {
                    source_node_id: svc_id.clone(),
                    target_node_id: format!("image:{}", img),
                    label: "DEPENDS_ON".to_string(),
                    weight: 1.0,
                    context: format!("image {}", img),
                    provenance: "EXTRACTED".to_string(),
                });
            }

            // build → DEPENDS_ON к Dockerfile
            let build_target = match svc.get("build") {
                Some(serde_json::Value::String(ctx)) => {
                    Some(format!(
                        "file:{}/Dockerfile",
                        ctx.trim_end_matches('/').trim_start_matches("./")
                    ))
                }
                Some(serde_json::Value::Object(b)) => {
                    let ctx = b
                        .get("context")
                        .and_then(|c| c.as_str())
                        .unwrap_or(".")
                        .trim_end_matches('/')
                        .trim_start_matches("./")
                        .to_string();
                    let df = b
                        .get("dockerfile")
                        .and_then(|d| d.as_str())
                        .unwrap_or("Dockerfile");
                    Some(format!("file:{}/{}", ctx, df))
                }
                _ => None,
            };
            if let Some(target) = build_target {
                out.edges.push(AstEdge {
                    source_node_id: svc_id.clone(),
                    target_node_id: target,
                    label: "DEPENDS_ON".to_string(),
                    weight: 1.0,
                    context: "build".to_string(),
                    provenance: "EXTRACTED".to_string(),
                });
            }

            // depends_on → DEPENDS_ON (по имени сервиса этого файла)
            match svc.get("depends_on") {
                Some(serde_json::Value::Array(deps)) => {
                    for d in deps {
                        if let Some(dep) = d.as_str() {
                            out.edges.push(AstEdge {
                                source_node_id: svc_id.clone(),
                                target_node_id: format!("svc:{}:{}", rel_path, dep),
                                label: "DEPENDS_ON".to_string(),
                                weight: 1.0,
                                context: format!("depends_on {}", dep),
                                provenance: "EXTRACTED".to_string(),
                            });
                        }
                    }
                }
                Some(serde_json::Value::Object(deps)) => {
                    for dep in deps.keys() {
                        out.edges.push(AstEdge {
                            source_node_id: svc_id.clone(),
                            target_node_id: format!("svc:{}:{}", rel_path, dep),
                            label: "DEPENDS_ON".to_string(),
                            weight: 1.0,
                            context: format!("depends_on {}", dep),
                            provenance: "EXTRACTED".to_string(),
                        });
                    }
                }
                _ => {}
            }

            // environment → EnvVar
            let mut env_keys: Vec<String> = Vec::new();
            match svc.get("environment") {
                Some(serde_json::Value::Array(envs)) => {
                    for e in envs {
                        if let Some(e) = e.as_str() {
                            let key = e.split('=').next().unwrap_or(e).to_string();
                            env_keys.push(key);
                        }
                    }
                }
                Some(serde_json::Value::Object(envs)) => {
                    env_keys.extend(envs.keys().cloned());
                }
                _ => {}
            }
            if let Some(serde_json::Value::Array(files)) = svc.get("env_file") {
                for f in files {
                    if let Some(f) = f.as_str() {
                        env_keys.push(format!("[env_file: {}]", f));
                    }
                }
            }
            for key in env_keys {
                let node_id = format!("envvar:{}:{}:{}", rel_path, name, key);
                out.nodes.push(AstNode {
                    node_id: node_id.clone(),
                    label: key.clone(),
                    node_type: "EnvVar".to_string(),
                    description: format!(
                        "Переменная окружения `{}` сервиса `{}` в {}",
                        key, name, rel_path
                    ),
                    file_path: rel_path.to_string(),
                    line_start: 1,
                    line_end: 1,
                });
                out.edges.push(AstEdge {
                    source_node_id: svc_id.clone(),
                    target_node_id: node_id,
                    label: "DEFINES".to_string(),
                    weight: 1.0,
                    context: format!("env {}", key),
                    provenance: "EXTRACTED".to_string(),
                });
            }

            // ports → Port (уникальное значение), volumes/networks → MOUNTS
            if let Some(serde_json::Value::Array(ports)) = svc.get("ports") {
                let mut seen_ports: HashSet<String> = HashSet::new();
                for p in ports {
                    let p = match p {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Object(_) => {
                            let pub_port = p.get("published").and_then(|v| v.as_str()).unwrap_or("");
                            let target = p.get("target").and_then(|t| t.as_i64()).unwrap_or(0);
                            format!("{}:{}", pub_port, target)
                        }
                        _ => continue,
                    };
                    if seen_ports.insert(p.to_string()) {
                        let node_id = format!("port:{}:{}", rel_path, p);
                        out.nodes.push(AstNode {
                            node_id: node_id.clone(),
                            label: p.to_string(),
                            node_type: "Port".to_string(),
                            description: format!("Порт `{}` сервиса `{}`", p, name),
                            file_path: rel_path.to_string(),
                            line_start: 1,
                            line_end: 1,
                        });
                        out.edges.push(AstEdge {
                            source_node_id: svc_id.clone(),
                            target_node_id: node_id,
                            label: "DEFINES".to_string(),
                            weight: 1.0,
                            context: format!("port {}", p),
                            provenance: "EXTRACTED".to_string(),
                        });
                    }
                }
            }
            for (key, label) in [("volumes", "MOUNTS"), ("networks", "MOUNTS")] {
                if let Some(serde_json::Value::Array(items)) = svc.get(key) {
                    let mut seen_items: HashSet<String> = HashSet::new();
                    for v in items {
                        let value = match v {
                            serde_json::Value::String(s) => s.clone(),
                            serde_json::Value::Object(o) => o
                                .get("source")
                                .and_then(|s| s.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            _ => continue,
                        };
                        if value.is_empty() || !seen_items.insert(value.clone()) {
                            continue;
                        }
                        let prefix = if key == "volumes" { "volume" } else { "network" };
                        let node_id = format!("{}:{}", prefix, value);
                        out.nodes.push(AstNode {
                            node_id: node_id.clone(),
                            label: value.clone(),
                            node_type: if key == "volumes" {
                                "Volume"
                            } else {
                                "Network"
                            }
                            .to_string(),
                            description: format!(
                                "{} `{}` ({} в {})",
                                if key == "volumes" { "Том" } else { "Сеть" },
                                value,
                                name,
                                rel_path
                            ),
                            file_path: rel_path.to_string(),
                            line_start: 1,
                            line_end: 1,
                        });
                        out.edges.push(AstEdge {
                            source_node_id: svc_id.clone(),
                            target_node_id: node_id,
                            label: label.to_string(),
                            weight: 1.0,
                            context: format!("{} {}", key, value),
                            provenance: "EXTRACTED".to_string(),
                        });
                    }
                }
            }
        }
        true
    }

    /// Ф50.4: Kubernetes/Helm YAML — документы с `kind:` → K8sResource
    /// (k8s:, label = kind/name), image → BaseImage + DEPENDS_ON,
    /// configMapRef/secretRef/envFrom/serviceAccountName/ingress→service →
    /// PendingRef, selector ↔ labels → BINDS_SELECTOR (INFERRED, в пределах
    /// файла). Helm-шаблоны не рендерим (пометка templated). Возвращает true,
    /// если найден хотя бы один документ с kind:.
    fn parse_k8s(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) -> bool {
        let docs = Self::split_yaml_docs(content);
        let templated = content.contains("{{");
        let mut found = false;
        let mut doc_nodes: Vec<(String, String, serde_json::Value)> = Vec::new(); // id, "kind/name", doc

        for doc in &docs {
            let Some(kind) = doc.get("kind").and_then(|k| k.as_str()) else {
                continue;
            };
            found = true;
            let name = doc
                .get("metadata")
                .and_then(|m| m.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("unnamed");
            let ns = doc
                .get("metadata")
                .and_then(|m| m.get("namespace"))
                .and_then(|n| n.as_str())
                .unwrap_or("default");
            let node_id = format!("k8s:{}:{}/{}", rel_path, kind, name);
            let label = format!("{}/{}", kind, name);
            out.nodes.push(AstNode {
                node_id: node_id.clone(),
                label: label.clone(),
                node_type: "K8sResource".to_string(),
                description: format!(
                    "K8s {} `{}` (namespace: {}{}) в {}",
                    kind,
                    name,
                    ns,
                    if templated { ", templated" } else { "" },
                    rel_path
                ),
                file_path: rel_path.to_string(),
                line_start: 1,
                line_end: 1,
            });
            out.edges.push(AstEdge {
                source_node_id: file_node_id.to_string(),
                target_node_id: node_id.clone(),
                label: "DEFINES".to_string(),
                weight: 1.0,
                context: format!("kind: {}", kind),
                provenance: "EXTRACTED".to_string(),
            });
            doc_nodes.push((node_id.clone(), label, doc.clone()));

            // image → BaseImage + DEPENDS_ON
            let images = find_yaml_strings(doc, "image");
            for img in images.iter().take(3) {
                out.nodes.push(AstNode {
                    node_id: format!("image:{}", img),
                    label: img.clone(),
                    node_type: "BaseImage".to_string(),
                    description: format!("Образ `{}` (image в {})", img, rel_path),
                    file_path: rel_path.to_string(),
                    line_start: 1,
                    line_end: 1,
                });
                out.edges.push(AstEdge {
                    source_node_id: node_id.clone(),
                    target_node_id: format!("image:{}", img),
                    label: "DEPENDS_ON".to_string(),
                    weight: 1.0,
                    context: format!("image {}", img),
                    provenance: "EXTRACTED".to_string(),
                });
            }

            // configMapRef/secretRef/envFrom/serviceAccountName → PendingRef
            for ref_name in find_yaml_strings(doc, "configMapRef").iter().map(|s| s.to_string()) {
                out.pending_refs.push(PendingRef {
                    source_node_id: node_id.clone(),
                    target_prefix: "k8s:".to_string(),
                    target_suffix: format!("ConfigMap/{}", ref_name),
                    label: "DEPENDS_ON".to_string(),
                    context: format!("configMapRef {}", ref_name),
                });
            }
            for ref_name in find_yaml_strings(doc, "secretRef").iter().map(|s| s.to_string()) {
                out.pending_refs.push(PendingRef {
                    source_node_id: node_id.clone(),
                    target_prefix: "k8s:".to_string(),
                    target_suffix: format!("Secret/{}", ref_name),
                    label: "DEPENDS_ON".to_string(),
                    context: format!("secretRef {}", ref_name),
                });
            }
            for sa in find_yaml_strings(doc, "serviceAccountName") {
                out.pending_refs.push(PendingRef {
                    source_node_id: node_id.clone(),
                    target_prefix: "k8s:".to_string(),
                    target_suffix: format!("ServiceAccount/{}", sa),
                    label: "DEPENDS_ON".to_string(),
                    context: format!("serviceAccountName {}", sa),
                });
            }
            // Ingress → Service (backend.service.name)
            if kind == "Ingress" {
                for svc_name in find_yaml_strings(doc, "name").iter().take(5) {
                    // только имена, попадающие в правила (rule → backend.service.name)
                    if content.contains("backend") && content.contains("service") {
                        out.pending_refs.push(PendingRef {
                            source_node_id: node_id.clone(),
                            target_prefix: "k8s:".to_string(),
                            target_suffix: format!("Service/{}", svc_name),
                            label: "DEPENDS_ON".to_string(),
                            context: format!("ingress → service {}", svc_name),
                        });
                    }
                }
            }
        }

        // selector ↔ labels внутри файла (BINDS_SELECTOR, INFERRED)
        let label_sets: Vec<(String, serde_json::Value)> = doc_nodes
            .iter()
            .filter_map(|(id, _, doc)| {
                doc.pointer("/metadata/labels").map(|l| (id.clone(), l.clone()))
            })
            .collect();
        for (id, _, doc) in &doc_nodes {
            if let Some(selector) = doc.pointer("/spec/selector/matchLabels") {
                for (sel_id, labels) in &label_sets {
                    if sel_id == id {
                        continue;
                    }
                    let matches = selector.as_object().is_some_and(|sel| {
                        sel.iter().all(|(k, v)| {
                            labels.get(k).and_then(|l| l.as_str()) == v.as_str()
                        })
                    });
                    if matches {
                        out.edges.push(AstEdge {
                            source_node_id: id.clone(),
                            target_node_id: sel_id.clone(),
                            label: "BINDS_SELECTOR".to_string(),
                            weight: 1.0,
                            context: "selector ↔ labels".to_string(),
                            provenance: "INFERRED".to_string(),
                        });
                    }
                }
            }
        }

        found
    }

    /// Ф51.1: манифесты пакетов — диспетчер по имени файла. Рёбра
    /// file →(DEPENDS_ON)→ `pkg:<ecosystem>:<name>` (ExternalPackage); версия —
    /// в context ребра; weight: 1.0 точный пин, 0.5 диапазон/без версии,
    /// 0.3 dev/peer/build-зависимости; path/git-зависимости Cargo — к file:/module:.
    fn parse_manifest(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let name = rel_path.rsplit('/').next().unwrap_or(rel_path);
        match name.to_lowercase().as_str() {
            "cargo.toml" => self.parse_manifest_cargo(rel_path, file_node_id, content, out),
            "package.json" => self.parse_manifest_package_json(rel_path, file_node_id, content, out),
            "pyproject.toml" => self.parse_manifest_pyproject(rel_path, file_node_id, content, out),
            "requirements.txt" => {
                self.parse_manifest_requirements(rel_path, file_node_id, content, out)
            }
            "go.mod" => self.parse_manifest_gomod(rel_path, file_node_id, content, out),
            "pubspec.yaml" | "pubspec.yml" => {
                self.parse_manifest_pubspec(rel_path, file_node_id, content, out)
            }
            "composer.json" => self.parse_manifest_composer(rel_path, file_node_id, content, out),
            _ => {}
        }
    }

    /// Вес зависимости (51.2): 1.0 точный пин, 0.5 диапазон/без версии.
    fn dep_weight(req: &str) -> f64 {
        let r = req.trim();
        if r.is_empty() {
            return 0.5;
        }
        if r.starts_with('^')
            || r.starts_with('~')
            || r == "*"
            || r.starts_with('>')
            || r.starts_with('<')
        {
            0.5
        } else {
            1.0
        }
    }

    fn push_dep(
        &self,
        out: &mut AstScanResult,
        file_node_id: &str,
        ecosystem: &str,
        name: &str,
        req: &str,
        weight: f64,
    ) {
        let pkg_id = format!("pkg:{}:{}", ecosystem, name);
        out.nodes.push(AstNode {
            node_id: pkg_id.clone(),
            label: name.to_string(),
            node_type: "ExternalPackage".to_string(),
            description: format!(
                "Внешний пакет `{} {}` ({}), потребители: {}",
                name,
                if req.is_empty() { "*" } else { req },
                ecosystem,
                file_node_id
            ),
            file_path: String::new(),
            line_start: 0,
            line_end: 0,
        });
        out.edges.push(AstEdge {
            source_node_id: file_node_id.to_string(),
            target_node_id: pkg_id,
            label: "DEPENDS_ON".to_string(),
            weight,
            context: format!("{} {}", name, req),
            provenance: "EXTRACTED".to_string(),
        });
    }

    fn parse_manifest_cargo(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let Ok(v) = toml::from_str::<serde_json::Value>(content) else {
            return;
        };
        let _ = rel_path;
        for (section, mult) in [
            ("dependencies", 1.0),
            ("dev-dependencies", 0.3),
            ("build-dependencies", 0.3),
        ] {
            let Some(deps) = v.get(section).and_then(|d| d.as_object()) else {
                continue;
            };
            for (name, spec) in deps {
                match spec {
                    serde_json::Value::String(req) => {
                        let w = if mult < 1.0 { 0.3 } else { Self::dep_weight(req) };
                        self.push_dep(out, file_node_id, "rust", name, req, w);
                    }
                    serde_json::Value::Object(t) => {
                        if let Some(path) = t.get("path").and_then(|p| p.as_str()) {
                            // path-зависимость — внутренний крейт рабочей области
                            let target = format!(
                                "file:{}/Cargo.toml",
                                path.trim_end_matches('/').trim_start_matches("./")
                            );
                            out.edges.push(AstEdge {
                                source_node_id: file_node_id.to_string(),
                                target_node_id: target,
                                label: "DEPENDS_ON".to_string(),
                                weight: 1.0,
                                context: format!("{} path = {}", name, path),
                                provenance: "EXTRACTED".to_string(),
                            });
                        } else if let Some(git) = t.get("git").and_then(|g| g.as_str()) {
                            out.edges.push(AstEdge {
                                source_node_id: file_node_id.to_string(),
                                target_node_id: format!("module:{}", git),
                                label: "DEPENDS_ON".to_string(),
                                weight: 0.5 * mult,
                                context: format!("{} git = {}", name, git),
                                provenance: "EXTRACTED".to_string(),
                            });
                        } else {
                            let req = t.get("version").and_then(|v| v.as_str()).unwrap_or("*");
                            let w = if mult < 1.0 { 0.3 } else { Self::dep_weight(req) };
                            self.push_dep(out, file_node_id, "rust", name, req, w);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn parse_manifest_package_json(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(content) else {
            return;
        };
        for (section, mult) in [
            ("dependencies", 1.0),
            ("devDependencies", 0.3),
            ("peerDependencies", 0.3),
        ] {
            let Some(deps) = v.get(section).and_then(|d| d.as_object()) else {
                continue;
            };
            for (name, spec) in deps {
                let Some(req) = spec.as_str() else { continue };
                let w = if mult < 1.0 { 0.3 } else { Self::dep_weight(req) };
                self.push_dep(out, file_node_id, "npm", name, req, w);
            }
        }
        // scripts → Script-узлы первого уровня (поиск `script:build`)
        if let Some(scripts) = v.get("scripts").and_then(|s| s.as_object()) {
            for (sname, cmd) in scripts {
                let node_id = format!("script:{}:{}", rel_path, sname);
                out.nodes.push(AstNode {
                    node_id: node_id.clone(),
                    label: sname.clone(),
                    node_type: "Script".to_string(),
                    description: format!(
                        "npm script `{}`: {} ({})",
                        sname,
                        cmd.as_str().unwrap_or(""),
                        rel_path
                    ),
                    file_path: rel_path.to_string(),
                    line_start: 1,
                    line_end: 1,
                });
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: node_id,
                    label: "DEFINES".to_string(),
                    weight: 1.0,
                    context: format!("script {}", sname),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }
    }

    fn parse_manifest_pyproject(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let Ok(v) = toml::from_str::<serde_json::Value>(content) else {
            return;
        };
        let _ = rel_path;
        // PEP 621: [project].dependencies — строки "name>=1.0"
        if let Some(deps) = v
            .get("project")
            .and_then(|p| p.get("dependencies"))
            .and_then(|d| d.as_array())
        {
            for d in deps {
                let Some(d) = d.as_str() else { continue };
                let name: String = d
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                    .collect();
                if name.is_empty() {
                    continue;
                }
                let req = d[name.len()..].trim();
                let w = if d.contains("==") { 1.0 } else { 0.5 };
                self.push_dep(out, file_node_id, "pypi", &name, req, w);
            }
        }
        // Poetry: [tool.poetry.dependencies]
        if let Some(deps) = v
            .pointer("/tool/poetry/dependencies")
            .and_then(|d| d.as_object())
        {
            for (name, spec) in deps {
                let Some(req) = spec.as_str() else { continue };
                if name == "python" {
                    continue;
                }
                self.push_dep(out, file_node_id, "pypi", name, req, Self::dep_weight(req));
            }
        }
        // [dependency-groups] (PEP 735) — dev-вес
        if let Some(groups) = v.get("dependency-groups").and_then(|d| d.as_object()) {
            for (_, items) in groups {
                let Some(items) = items.as_array() else { continue };
                for d in items {
                    let Some(d) = d.as_str() else { continue };
                    let name: String = d
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                        .collect();
                    if !name.is_empty() {
                        self.push_dep(out, file_node_id, "pypi", &name, d, 0.3);
                    }
                }
            }
        }
    }

    fn parse_manifest_requirements(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let _ = rel_path;
        for line in content.lines() {
            let t = line.trim();
            if t.is_empty()
                || t.starts_with('#')
                || t.starts_with("-r")
                || t.starts_with("-e")
                || t.starts_with("--")
            {
                continue;
            }
            let name: String = t
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
                .collect();
            if name.is_empty() {
                continue;
            }
            let req = t[name.len()..].trim();
            // строки без версии — unpinned (51.1)
            let (req_norm, w) = if req.is_empty() {
                ("unpinned".to_string(), 0.5)
            } else if req.contains("==") {
                (req.to_string(), 1.0)
            } else {
                (req.to_string(), 0.5)
            };
            self.push_dep(out, file_node_id, "pypi", &name, &req_norm, w);
        }
    }

    fn parse_manifest_gomod(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let _ = rel_path;
        let mut in_require = false;
        for line in content.lines() {
            let t = line.trim();
            if t.starts_with("require (") {
                in_require = true;
                continue;
            }
            if in_require && t == ")" {
                in_require = false;
                continue;
            }
            let item = if in_require {
                t
            } else if let Some(rest) = t.strip_prefix("require ") {
                rest.trim()
            } else {
                continue;
            };
            let mut parts = item.split_whitespace();
            let Some(path) = parts.next() else { continue };
            let Some(ver) = parts.next() else { continue };
            if ver.starts_with("//") {
                continue;
            }
            let w = if item.contains("// indirect") {
                0.3
            } else {
                Self::dep_weight(ver)
            };
            self.push_dep(out, file_node_id, "go", path, ver, w);
        }
    }

    fn parse_manifest_pubspec(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let Ok(v) = serde_yaml_ng::from_str::<serde_json::Value>(content) else {
            return;
        };
        let _ = rel_path;
        for (section, mult) in [("dependencies", 1.0), ("dev_dependencies", 0.3)] {
            let Some(deps) = v.get(section).and_then(|d| d.as_object()) else {
                continue;
            };
            for (name, spec) in deps {
                let Some(req) = spec.as_str() else { continue };
                if name == "flutter" || name == "sdk" {
                    continue;
                }
                self.push_dep(
                    out,
                    file_node_id,
                    "pub",
                    name,
                    req,
                    if mult < 1.0 { 0.3 } else { Self::dep_weight(req) },
                );
            }
        }
    }

    fn parse_manifest_composer(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(content) else {
            return;
        };
        let _ = rel_path;
        for (section, mult) in [("require", 1.0), ("require-dev", 0.3)] {
            let Some(deps) = v.get(section).and_then(|d| d.as_object()) else {
                continue;
            };
            for (name, spec) in deps {
                let Some(req) = spec.as_str() else { continue };
                if name == "php" || name.starts_with("ext-") {
                    continue;
                }
                self.push_dep(
                    out,
                    file_node_id,
                    "composer",
                    name,
                    req,
                    if mult < 1.0 { 0.3 } else { Self::dep_weight(req) },
                );
            }
        }
    }

    fn parse_scala(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. Imports
        for cap in self.scala_import_re.captures_iter(content) {
            if let Some(m) = cap.get(1) {
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", m.as_str()),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("import {}", m.as_str()),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 2. Типы: class/object/trait (+case-модификаторы)
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.scala_type_re.captures(line) {
                let kind = cap.get(1).map(|m| m.as_str()).unwrap_or("class");
                if let Some(name_match) = cap.get(2) {
                    let type_name = name_match.as_str();
                    let (node_id, node_type, description) = if kind == "trait" {
                        (
                            format!("trait:{}:{}", rel_path, type_name),
                            "Trait".to_string(),
                            format!("Scala трейт `{}` в {}", type_name, rel_path),
                        )
                    } else {
                        (
                            format!("class:{}:{}", rel_path, type_name),
                            "Class".to_string(),
                            format!("Scala класс/объект `{}` в {}", type_name, rel_path),
                        )
                    };
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: type_name.to_string(),
                        node_type,
                        description,
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    // extends Base with T1, T2: первая часть — наследование,
                    // остальные — примешанные трейты
                    if let Some(bases_match) = cap.get(3) {
                        let cleaned = strip_paren_groups(bases_match.as_str());
                        let mut parts = cleaned.split(" with ");
                        if let Some(first) = parts.next() {
                            let base_trimmed = first.trim();
                            let base = match base_trimmed.find('[') {
                                Some(p) => &base_trimmed[..p],
                                None => base_trimmed,
                            };
                            if !base.is_empty() {
                                out.edges.push(AstEdge {
                                    source_node_id: node_id.clone(),
                                    target_node_id: format!("class:{}", base),
                                    label: "INHERITS".to_string(),
                                    weight: 1.0,
                                    context: format!("extends {}", base),
                                    provenance: "EXTRACTED".to_string(),
                                });
                            }
                        }
                        for with_part in parts {
                            for token in with_part.split(',') {
                                let token_trimmed = token.trim();
                                let trait_name = match token_trimmed.find('[') {
                                    Some(p) => &token_trimmed[..p],
                                    None => token_trimmed,
                                };
                                if trait_name.is_empty() {
                                    continue;
                                }
                                out.edges.push(AstEdge {
                                    source_node_id: node_id.clone(),
                                    target_node_id: format!("trait:{}", trait_name),
                                    label: "IMPLEMENTS".to_string(),
                                    weight: 1.0,
                                    context: format!("with {}", trait_name),
                                    provenance: "EXTRACTED".to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }

        // 3. Функции
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.scala_fn_re.captures(line) {
                if let Some(fn_match) = cap.get(1) {
                    let fn_name = fn_match.as_str();
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("Scala функция `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }

    fn parse_lua(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. require (включая `local x = require("mod")`)
        for cap in self.lua_require_re.captures_iter(content) {
            if let Some(m) = cap.get(1) {
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", m.as_str()),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("require '{}'", m.as_str()),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 2. Функции: function name / M.name / M:name / local function
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.lua_fn_re.captures(line) {
                if let Some(fn_match) = cap.get(1) {
                    let fn_name = fn_match.as_str();
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("Lua функция `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }

    fn parse_elixir(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. alias / import / use / require
        for cap in self.ex_alias_re.captures_iter(content) {
            if let Some(m) = cap.get(1) {
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", m.as_str()),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("alias {}", m.as_str()),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 2. defmodule и defprotocol
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.ex_module_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let module_name = name_match.as_str();
                    let node_id = format!("module:{}:{}", rel_path, module_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: module_name.to_string(),
                        node_type: "Module".to_string(),
                        description: format!("Elixir модуль `{}` в {}", module_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            } else if let Some(cap) = self.ex_protocol_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let proto_name = name_match.as_str();
                    let node_id = format!("interface:{}:{}", rel_path, proto_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: proto_name.to_string(),
                        node_type: "Interface".to_string(),
                        description: format!("Elixir протокол `{}` в {}", proto_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        // 3. def / defp / defmacro / defmacrop (defdelegate/defstruct/... не матчатся)
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.ex_fn_re.captures(line) {
                if let Some(fn_match) = cap.get(1) {
                    let fn_name = fn_match.as_str();
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("Elixir функция `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }

    fn parse_shell(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. source / . (переменные вида `source "$f"` не резолвим)
        for cap in self.sh_source_re.captures_iter(content) {
            if let Some(m) = cap.get(1) {
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", m.as_str()),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("source {}", m.as_str()),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 2. Функции: POSIX `name() {` и keyword-форма `function name`
        for (i, line) in lines.iter().enumerate() {
            let cap = self
                .sh_fn_paren_re
                .captures(line)
                .or_else(|| self.sh_fn_kw_re.captures(line));
            if let Some(cap) = cap {
                if let Some(fn_match) = cap.get(1) {
                    let fn_name = fn_match.as_str();
                    if matches!(
                        fn_name,
                        "if" | "for"
                            | "while"
                            | "case"
                            | "until"
                            | "return"
                            | "time"
                            | "select"
                    ) {
                        continue;
                    }
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("Shell функция `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }
}
impl Parser {
    /// Диспетчер: выбрать парсер по тегу реестра (Ф47.1).
    fn execute(
        &self,
        ex: &AstCodeExtractor,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        match self {
            Parser::Rust => ex.parse_rust(rel_path, file_node_id, content, out),
            Parser::Python => ex.parse_python(rel_path, file_node_id, content, 0, out),
            Parser::TsJs => ex.parse_ts_js(rel_path, file_node_id, content, 0, out),
            Parser::Go => ex.parse_go(rel_path, file_node_id, content, out),
            Parser::Sql => ex.parse_sql(rel_path, file_node_id, content, out),
            Parser::Php => ex.parse_php(rel_path, file_node_id, content, out),
            Parser::Dart => ex.parse_dart(rel_path, file_node_id, content, out),
            Parser::Java => ex.parse_java(rel_path, file_node_id, content, out),
            Parser::CSharp => ex.parse_csharp(rel_path, file_node_id, content, out),
            Parser::CCpp => ex.parse_c_cpp(rel_path, file_node_id, content, out),
            Parser::Kotlin => ex.parse_kotlin(rel_path, file_node_id, content, out),
            Parser::Swift => ex.parse_swift(rel_path, file_node_id, content, out),
            Parser::Ruby => ex.parse_ruby(rel_path, file_node_id, content, out),
            Parser::Sfc => ex.parse_sfc(rel_path, file_node_id, content, out),
            Parser::Scala => ex.parse_scala(rel_path, file_node_id, content, out),
            Parser::Lua => ex.parse_lua(rel_path, file_node_id, content, out),
            Parser::Elixir => ex.parse_elixir(rel_path, file_node_id, content, out),
            Parser::Shell => ex.parse_shell(rel_path, file_node_id, content, out),
            Parser::Html => ex.parse_html(rel_path, file_node_id, content, out),
            Parser::Css => ex.parse_css(rel_path, file_node_id, content, 0, out),
            Parser::Notebook => ex.parse_notebook(rel_path, file_node_id, content, out),
            Parser::Solidity => ex.parse_solidity(rel_path, file_node_id, content, out),
            Parser::Zig => ex.parse_zig(rel_path, file_node_id, content, out),
            Parser::Proto => ex.parse_proto(rel_path, file_node_id, content, out),
            Parser::Graphql => ex.parse_graphql(rel_path, file_node_id, content, out),
            Parser::OpenApi => {
                ex.parse_openapi(rel_path, file_node_id, content, out);
            }
            Parser::Terraform => ex.parse_terraform(rel_path, file_node_id, content, out),
            Parser::Dockerfile => ex.parse_dockerfile(rel_path, file_node_id, content, out),
            Parser::Compose => {
                ex.parse_compose(rel_path, file_node_id, content, out);
            }
            Parser::K8s => {
                ex.parse_k8s(rel_path, file_node_id, content, out);
            }
            Parser::Manifest => {
                ex.parse_manifest(rel_path, file_node_id, content, out);
            }
        }
    }
}

/// Ф49.4б: собрать клиентские HTTP-вызовы с path-литералом (fetch/axios в
/// ts/js, requests в py); только in-app пути (`/...`).
fn collect_http_calls(rel_path: &str, content: &str, re: &Regex, out: &mut AstScanResult) {
    for cap in re.captures_iter(content) {
        let method = cap
            .get(1)
            .map(|m| m.as_str().to_uppercase())
            .unwrap_or_else(|| "GET".to_string());
        let Some(path) = cap.get(2) else { continue };
        let p = path.as_str().trim();
        if !p.starts_with('/') || p.starts_with("//") {
            continue;
        }
        out.http_calls.push(HttpCall {
            rel_path: rel_path.to_string(),
            method,
            path: p.to_string(),
        });
    }
}

/// Ф49.3: имя схемы из `$ref` (`#/components/schemas/User` → `User`).
fn find_ref_name(v: &serde_json::Value) -> Option<String> {
    let rf = v.get("$ref").and_then(|r| r.as_str())?;
    rf.rsplit('/').next().map(|s| s.to_string())
}

/// Ф50.4: все строковые значения по ключу в глубине YAML-документа
/// (image/configMapRef/serviceAccountName — на любой глубине podspec'а).
fn find_yaml_strings(v: &serde_json::Value, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(v: &serde_json::Value, key: &str, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, val) in m {
                    if k == key {
                        if let Some(s) = val.as_str() {
                            out.push(s.to_string());
                        } else if let Some(name) = val.get("name").and_then(|n| n.as_str()) {
                            // configMapRef: { name: x }
                            out.push(name.to_string());
                        }
                    }
                    walk(val, key, out);
                }
            }
            serde_json::Value::Array(items) => {
                for it in items {
                    walk(it, key, out);
                }
            }
            _ => {}
        }
    }
    walk(v, key, &mut out);
    out
}

/// Нормализация пути для сопоставления контракта и кода (49.4а):
/// `{id}` / `<id>` / `:id` → `:param`.
fn normalize_api_path(p: &str) -> String {
    let mut out = String::new();
    let mut chars = p.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' | '<' => {
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if n == '}' || n == '>' {
                        break;
                    }
                }
                out.push(':');
                out.push_str("param");
            }
            ':' if !out.ends_with('/') && !out.ends_with(':') => {
                // `:id` — параметр до следующего `/`
                out.push_str(":param");
                while let Some(&n) = chars.peek() {
                    if n == '/' {
                        break;
                    }
                    chars.next();
                }
            }
            _ => out.push(c),
        }
    }
    out.trim_end_matches('/')
        .to_lowercase()
}

/// Ф49.4 (PLAN_v1.7): INFERRED-мосты контракт↔код по точным совпадениям:
/// (б) fetch/axios/requests с path-литералом → `SERVES_ENDPOINT` к route:-узлу
/// контракта; (в) идентификаторы gql-строк → `QUERIES_FIELD` к Function-полю;
/// (г) proto rpc ↔ хендлер по нормализованному имени → `PROVIDES_RPC`
/// (первый детерминированный матч). Возвращает число добавленных рёбер.
pub fn resolve_bridges(scan: &mut AstScanResult) -> usize {
    let mut added = 0usize;
    let mut seen: HashSet<(String, String, String)> = HashSet::new();

    // (б) клиентские вызовы → route:-узлы контракта
    let route_nodes: Vec<(String, String)> = scan
        .nodes
        .iter()
        .filter(|n| n.node_type == "Route")
        .filter_map(|n| {
            // node_id = "route:GET /api/users" -> путь после метода
            let rest = n.node_id.strip_prefix("route:")?;
            let path = rest.split_once(' ')?.1;
            Some((n.node_id.clone(), normalize_api_path(path)))
        })
        .collect();
    for call in &scan.http_calls {
        let norm = normalize_api_path(&call.path);
        let target = route_nodes.iter().find(|(nid, npath)| {
            npath == &norm && {
                let method = nid
                    .strip_prefix("route:")
                    .and_then(|rest| rest.split(' ').next())
                    .unwrap_or("");
                method.eq_ignore_ascii_case(&call.method)
            }
        });
        if let Some((nid, _)) = target {
            let src = format!("file:{}", call.rel_path);
            if seen.insert((src.clone(), nid.clone(), "SERVES_ENDPOINT".to_string())) {
                scan.edges.push(AstEdge {
                    source_node_id: src,
                    target_node_id: nid.clone(),
                    label: "SERVES_ENDPOINT".to_string(),
                    weight: 1.0,
                    context: format!("{} {}", call.method, call.path),
                    provenance: "INFERRED".to_string(),
                });
                added += 1;
            }
        }
    }

    // (в) gql-поля → Function-узлы GraphQL
    let gql_fns: Vec<(String, String)> = scan
        .nodes
        .iter()
        .filter(|n| n.node_type == "Function" && n.description.contains("GraphQL"))
        .map(|n| (n.node_id.clone(), n.label.clone()))
        .collect();
    for g in &scan.gql_fields {
        if let Some((nid, _)) = gql_fns.iter().find(|(_, label)| label == &g.field) {
            let src = format!("file:{}", g.rel_path);
            if seen.insert((src.clone(), nid.clone(), "QUERIES_FIELD".to_string())) {
                scan.edges.push(AstEdge {
                    source_node_id: src,
                    target_node_id: nid.clone(),
                    label: "QUERIES_FIELD".to_string(),
                    weight: 1.0,
                    context: format!("gql {}", g.field),
                    provenance: "INFERRED".to_string(),
                });
                added += 1;
            }
        }
    }

    // Ф50: PendingRef → DEPENDS_ON (tf depends_on по type.name, k8s refs по
    // label kind/name); неразрешённые отбрасываются
    for r in &scan.pending_refs {
        if let Some(target) = scan.nodes.iter().find(|n| {
            n.node_id.starts_with(&r.target_prefix) && n.node_id.ends_with(&r.target_suffix)
        }) {
            if seen.insert((
                r.source_node_id.clone(),
                target.node_id.clone(),
                r.label.clone(),
            )) {
                scan.edges.push(AstEdge {
                    source_node_id: r.source_node_id.clone(),
                    target_node_id: target.node_id.clone(),
                    label: r.label.clone(),
                    weight: 1.0,
                    context: r.context.clone(),
                    provenance: "EXTRACTED".to_string(),
                });
                added += 1;
            }
        }
    }

    // (г) proto rpc ↔ хендлер: snake_case == camelCase, хендлер не сам rpc
    let to_snake = |s: &str| -> String {
        let mut out = String::new();
        for (i, c) in s.chars().enumerate() {
            if c.is_uppercase() {
                if i > 0 {
                    out.push('_');
                }
                out.extend(c.to_lowercase());
            } else {
                out.push(c);
            }
        }
        out
    };
    let rpcs: Vec<(String, String)> = scan
        .nodes
        .iter()
        .filter(|n| n.node_type == "Function" && n.description.contains("Protobuf rpc"))
        .map(|n| (n.node_id.clone(), n.label.clone()))
        .collect();
    for (rpc_id, rpc_label) in &rpcs {
        let want = to_snake(rpc_label);
        if let Some(handler) = scan.nodes.iter().find(|n| {
            n.node_type == "Function"
                && !n.description.contains("Protobuf rpc")
                && n.node_id != rpc_id.as_str()
                && to_snake(&n.label) == want
        }) {
            let src = handler.node_id.clone();
            if seen.insert((src.clone(), rpc_id.clone(), "PROVIDES_RPC".to_string())) {
                scan.edges.push(AstEdge {
                    source_node_id: src,
                    target_node_id: rpc_id.clone(),
                    label: "PROVIDES_RPC".to_string(),
                    weight: 1.0,
                    context: format!("rpc {}", rpc_label),
                    provenance: "INFERRED".to_string(),
                });
                added += 1;
            }
        }
    }

    added
}

/// Убирает содержимое круглых скобок (`Base(x, y), IBar` → `Base, IBar`) —
/// чтобы запятые в аргументах конструктора не ломали разбор списка баз.
fn strip_paren_groups(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for ch in s.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

/// Ф39.1: разобрать use-строку Rust → (module_path, item, alias).
/// `crate::a::b::Name as Alias` → ("crate::a::b", Some("Name"), Some("Alias")).
fn split_rust_use(s: &str) -> Option<(String, Option<String>, Option<String>)> {
    let s = s.trim();
    if s.is_empty() || s.contains('{') || s.contains('*') {
        return None; // group/glob imports не резолвим lite-проходом
    }
    let (path, alias) = match s.split_once(" as ") {
        Some((p, a)) => (p.trim(), Some(a.trim().to_string())),
        None => (s, None),
    };
    let parts: Vec<&str> = path.split("::").filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    if parts.len() == 1 {
        return Some((parts[0].to_string(), None, alias));
    }
    let item = parts.last().unwrap().to_string();
    let module = parts[..parts.len() - 1].join("::");
    Some((module, Some(item), alias))
}

/// Ф39.1: кандидаты-файлы, которые может обозначать модуль импорта.
/// Rust: `crate::a::b` → src/a.rs, src/a/b.rs, src/a/b/mod.rs, a.rs…;
/// `super::x`/`self::x` — относительно файла.
/// Python: `.mod`/`pkg.mod` → mod.py, mod/__init__.py.
fn module_to_paths(module: &str, from_file: &str) -> Vec<String> {
    fn parent(p: &str) -> String {
        std::path::Path::new(p)
            .parent()
            .map(|x| x.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    let mut out = Vec::new();
    let m = module.trim();
    if m.is_empty() {
        return out;
    }
    if m.contains("::") {
        // Rust
        let ups = m.matches("super::").count();
        let rel = m.trim_start_matches("crate::");
        let rel = rel.trim_start_matches("self::");
        let mut rel = rel.to_string();
        for _ in 0..ups {
            rel = rel.trim_start_matches("super::").to_string();
        }
        let parts: Vec<&str> = rel.split("::").filter(|p| !p.is_empty()).collect();
        let prefix = if m.starts_with("crate::") {
            "src/".to_string()
        } else if ups > 0 {
            let mut d = parent(from_file);
            for _ in 0..ups {
                d = parent(&d);
            }
            if d.is_empty() {
                d
            } else {
                format!("{d}/")
            }
        } else {
            String::new()
        };
        for k in (1..=parts.len()).rev() {
            let b = parts[..k].join("/");
            out.push(format!("{prefix}{b}.rs"));
            out.push(format!("{prefix}{b}/mod.rs"));
        }
        return out;
    }
    if m.starts_with('.') || m.contains('.') {
        // Python
        let dots = m.chars().take_while(|&c| c == '.').count();
        let rest = &m[dots..];
        let mut base = parent(from_file);
        for _ in 1..dots {
            base = parent(&base);
        }
        let prefix = if base.is_empty() {
            String::new()
        } else {
            format!("{base}/")
        };
        if rest.is_empty() {
            out.push(format!("{prefix}__init__.py"));
        } else {
            let rel = rest.replace('.', "/");
            out.push(format!("{prefix}{rel}.py"));
            out.push(format!("{prefix}{rel}/__init__.py"));
        }
    }
    out
}

/// Ф39.1: собрать места вызовов `name(` — без `.`-методов (receiver-типы
/// неизвестны) и макросов `name!(`; ключевые слова скипаются.
fn collect_call_sites(
    rel_path: &str,
    lines: &[&str],
    skip: &[&str],
    re: &Regex,
    line_offset: usize,
    out: &mut AstScanResult,
) {
    // строки-определения: `fn helper(` / `def load(` — это не вызовы
    const DEF_STARTERS: &[&str] = &[
        "fn ",
        "pub fn ",
        "pub(crate) fn ",
        "def ",
        "async def ",
        "class ",
        "trait ",
        "struct ",
        "enum ",
        "impl ",
        "mod ",
        "type ",
    ];
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if DEF_STARTERS.iter().any(|d| trimmed.starts_with(d)) {
            continue;
        }
        for cap in re.captures_iter(line) {
            let Some(m) = cap.get(1) else { continue };
            let name = m.as_str();
            if skip.contains(&name) {
                continue;
            }
            let start = m.start();
            let bytes = line.as_bytes();
            if start > 0 && (bytes[start - 1] == b'.' || bytes[start - 1] == b'!') {
                continue; // AMBIGUOUS: метод с неизвестным receiver / макрос
            }
            out.call_sites.push(CallSite {
                rel_path: rel_path.to_string(),
                line: i + 1 + line_offset,
                name: name.to_string(),
            });
        }
    }
}

/// Ф39.1: лёгкий semantic pass (Rust + Python) — резолв простых вызовов:
/// same-file и по импортам (алиасы учитываются, receiver-типы — нет).
/// Нерезолвленное остаётся без ребра (AMBIGUOUS — не выдумываем).
/// Возвращает число добавленных CALLS-рёбер (provenance = RESOLVED).
pub fn resolve_calls(scan: &mut AstScanResult) -> usize {
    // Ф47.4: единый словарь символов с dead-code/callpath (crate::graph::callpath)
    use crate::graph::callpath::SYMBOL_TYPES;

    // 1. Таблица символов: label → индексы узлов
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, n) in scan.nodes.iter().enumerate() {
        if SYMBOL_TYPES.contains(&n.node_type.as_str()) {
            by_name.entry(n.label.clone()).or_default().push(i);
        }
    }

    // 2. Импорты и диапазоны символов по файлам
    let mut imports_by_file: HashMap<&str, Vec<&ImportRef>> = HashMap::new();
    for imp in &scan.imports {
        imports_by_file
            .entry(imp.rel_path.as_str())
            .or_default()
            .push(imp);
    }
    let mut syms_by_file: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, n) in scan.nodes.iter().enumerate() {
        if SYMBOL_TYPES.contains(&n.node_type.as_str()) {
            syms_by_file
                .entry(n.file_path.as_str())
                .or_default()
                .push(i);
        }
    }
    for idxs in syms_by_file.values_mut() {
        idxs.sort_by_key(|&i| scan.nodes[i].line_start);
    }

    // 3. Резолв каждого места вызова
    let mut added = 0usize;
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for site in &scan.call_sites {
        // вызывающий — последний символ файла, чей line_start <= строке вызова
        let Some(ci) = syms_by_file.get(site.rel_path.as_str()).and_then(|idxs| {
            let mut caller = None;
            for &i in idxs {
                if scan.nodes[i].line_start <= site.line {
                    caller = Some(i);
                } else {
                    break;
                }
            }
            caller
        }) else {
            continue;
        };

        // алиас → реальное имя элемента; запоминаем модуль алиаса:
        // вызов через алиас обязан уйти в файл этого модуля, не в same-file
        let mut name = site.name.clone();
        let mut alias_module: Option<String> = None;
        if let Some(imps) = imports_by_file.get(site.rel_path.as_str()) {
            for imp in imps {
                if imp.alias.as_deref() == Some(site.name.as_str()) {
                    if let Some(item) = &imp.item {
                        name = item.clone();
                        alias_module = Some(imp.module.clone());
                    }
                }
            }
        }
        let Some(cands) = by_name.get(&name) else {
            continue;
        };

        let import_paths: Vec<String> = imports_by_file
            .get(site.rel_path.as_str())
            .map(|v| {
                v.iter()
                    .flat_map(|imp| module_to_paths(&imp.module, &imp.rel_path))
                    .collect()
            })
            .unwrap_or_default();

        let mut target: Option<(usize, String)> = None;
        if let Some(am) = &alias_module {
            let paths = module_to_paths(am, site.rel_path.as_str());
            for &ti in cands {
                let tn = &scan.nodes[ti];
                if tn.node_id != scan.nodes[ci].node_id && paths.iter().any(|p| p == &tn.file_path)
                {
                    target = Some((ti, format!("typepass:import-alias:{}", tn.file_path)));
                    break;
                }
            }
        }
        if target.is_none() {
            for &ti in cands {
                let tn = &scan.nodes[ti];
                if tn.node_id == scan.nodes[ci].node_id {
                    continue; // рекурсия не даёт ребра в самого себя
                }
                if tn.file_path == site.rel_path {
                    target = Some((ti, "typepass:same-file".to_string()));
                    break;
                }
                if import_paths.iter().any(|p| p == &tn.file_path) {
                    target = Some((ti, format!("typepass:import:{}", tn.file_path)));
                    break;
                }
            }
        }

        if let Some((ti, ctx)) = target {
            let src = scan.nodes[ci].node_id.clone();
            let dst = scan.nodes[ti].node_id.clone();
            if seen.insert((src.clone(), dst.clone())) {
                scan.edges.push(AstEdge {
                    source_node_id: src,
                    target_node_id: dst,
                    label: "CALLS".to_string(),
                    weight: 1.0,
                    context: ctx,
                    provenance: "RESOLVED".to_string(),
                });
                added += 1;
            }
        }
    }
    added
}
