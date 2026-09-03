//! Синхронизация двух инстансов ob2h (PC ↔ VPS) файловыми бандлами (ADR-9).
//!
//! Бандл — gzip'd JSONL: строка-заголовок + строки таблиц memories/graph_nodes/
//! graph_edges (tombstones = те же строки с deleted_at). Импорт идемпотентен
//! (по bundle_id), конфликты решаются LWW по updated_at с tie-break по приоритету
//! origin из peers.json. Живую SQLite-БД файловой синхронизацией не передавать.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context as _};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use crate::backup::BackupManager;
use crate::config::Settings;
use crate::db::{utcnow, Database};
use crate::embedding::EmbeddingProvider;
use crate::vector::serialize as vec_serialize;

pub mod worker;
pub use worker::AutoSyncWorker;

// -- Конфиг пирингов (data/sync/peers.json) -----------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PeerConfig {
    /// ssh | manual (manual — перенос файлов руками/Syncthing/git)
    #[serde(default = "default_method")]
    pub method: String,
    pub host: Option<String>,
    #[serde(default)]
    pub ssh_port: Option<String>,
    /// Куда push'им свои бандлы (папка inbox на пире)
    pub push_to: Option<String>,
    /// Откуда pull'им чужие бандлы (папка outbox на пире)
    pub pull_from: Option<String>,
}

fn default_method() -> String {
    "ssh".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    /// Идентичность этой машины: "pc" | "vps" | …
    #[serde(default = "default_origin")]
    pub origin: String,
    /// Порядок tie-break LWW (раньше = выше приоритет).
    #[serde(default)]
    pub priority: Vec<String>,
    #[serde(default)]
    pub peers: HashMap<String, PeerConfig>,
    /// Обмен после каждого автодрима (push+pull всех ssh-пиров)
    #[serde(default)]
    pub after_dream: bool,
}

fn default_origin() -> String {
    "local".to_string()
}

impl NodeConfig {
    pub fn load(data_dir: &Path) -> anyhow::Result<NodeConfig> {
        let path = data_dir.join("sync").join("peers.json");
        if !path.is_file() {
            return Ok(NodeConfig {
                origin: default_origin(),
                priority: vec![],
                peers: HashMap::new(),
                after_dream: false,
            });
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("чтение {}", path.display()))?;
        serde_json::from_str(&raw).with_context(|| format!("парсинг {}", path.display()))
    }

    fn rank(&self, origin: &str) -> usize {
        // Пустой origin = «строка этого узла» — рангуем как собственный origin.
        let o = if origin.is_empty() { &self.origin } else { origin };
        self.priority.iter().position(|p| p == o).unwrap_or(usize::MAX)
    }

    fn effective_origin(&self, row_origin: &str) -> String {
        if row_origin.is_empty() {
            self.origin.clone()
        } else {
            row_origin.to_string()
        }
    }
}

// -- Статистика ----------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct ImportStats {
    pub bundle_id: String,
    pub memories_applied: usize,
    pub nodes_applied: usize,
    pub edges_applied: usize,
    pub conflicts_lost: usize,
    pub skipped_missing_ref: usize,
    pub already_applied: bool,
}

// -- SyncManager ----------------------------------------------------------------

pub struct SyncManager {
    settings: Settings,
    db: Database,
    embedder: std::sync::Arc<dyn EmbeddingProvider>,
    backup: std::sync::Arc<BackupManager>,
    node: NodeConfig,
}

impl SyncManager {
    /// Не падает на битом peers.json — логирует и работает с пустым конфигом
    /// (память/граф важнее синка).
    pub fn new(
        settings: Settings,
        db: Database,
        embedder: std::sync::Arc<dyn EmbeddingProvider>,
        backup: std::sync::Arc<BackupManager>,
    ) -> Self {
        let node = NodeConfig::load(&settings.data_dir).unwrap_or_else(|e| {
            warn!("sync: peers.json не прочитан ({e}) — синк отключён до исправления");
            NodeConfig {
                origin: default_origin(),
                priority: vec![],
                peers: HashMap::new(),
                after_dream: false,
            }
        });
        Self {
            settings,
            db,
            embedder,
            backup,
            node,
        }
    }

    pub fn node_config(&self) -> &NodeConfig {
        &self.node
    }

    pub fn sync_dir(&self) -> PathBuf {
        self.settings.data_dir.join("sync")
    }

    pub fn outbox(&self) -> PathBuf {
        self.sync_dir().join("outbox")
    }

    pub fn inbox(&self) -> PathBuf {
        self.sync_dir().join("inbox")
    }

    // -- Экспорт ---------------------------------------------------------------

    /// Выгрузить изменения с прошлого экспорта для пира в outbox/<bundle>.jsonl.gz.
    pub fn export(&self, peer: &str) -> anyhow::Result<PathBuf> {
        if self.node.peers.is_empty() && peer != "default" {
            // позволяем ручной экспорт без конфига пирингов (watermark "default")
        }
        std::fs::create_dir_all(self.outbox())?;

        let watermark: Option<String> = self.db.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT last_export_at FROM sync_state WHERE peer = ?1")?;
            let mut rows = stmt.query(params![peer])?;
            Ok(match rows.next()? {
                Some(row) => row.get::<_, Option<String>>(0)?,
                None => None,
            })
        })?;

        let mut rows: Vec<serde_json::Value> = Vec::new();
        let mut max_ts = watermark.clone().unwrap_or_default();

        // memories (embedding конвертируем в hex прямо в SQL — collect_rows не знает blob)
        self.collect_rows(
            "SELECT key, content, category, importance, source, meta, hex(embedding) AS embedding, created_at, updated_at, origin, deleted_at, project_id
             FROM memories WHERE ?1 IS NULL OR MAX(updated_at, COALESCE(deleted_at, '')) >= ?1",
            watermark.as_deref(),
            &mut |v| {
                v["type"] = json!("mem");
                // '' = «строка этого узла» — в бандле всегда конкретный origin
                v["origin"] = json!(self.node.effective_origin(
                    v["origin"].as_str().unwrap_or("")
                ));
                rows.push(v.clone());
            },
        )?;
        // graph_nodes
        self.collect_rows(
            "SELECT node_id, label, node_type, description, val, hex(embedding) AS embedding, created_at, updated_at, origin, deleted_at, project_id, provenance, confidence, file_path, line_start, line_end, is_god_node
             FROM graph_nodes WHERE ?1 IS NULL OR MAX(updated_at, COALESCE(deleted_at, '')) >= ?1",
            watermark.as_deref(),
            &mut |v| {
                v["type"] = json!("node");
                // '' = «строка этого узла» — в бандле всегда конкретный origin
                v["origin"] = json!(self.node.effective_origin(
                    v["origin"].as_str().unwrap_or("")
                ));
                rows.push(v.clone());
            },
        )?;
        // graph_edges (с текстовыми node_id вместо локальных INTEGER id)
        self.collect_rows(
            "SELECT s.node_id AS source_node, t.node_id AS target_node, e.label, e.weight, e.contexts, e.created_at, e.updated_at, e.origin, e.deleted_at, e.project_id, e.provenance, e.confidence
             FROM graph_edges e
             JOIN graph_nodes s ON s.id = e.source_id
             JOIN graph_nodes t ON t.id = e.target_id
             WHERE ?1 IS NULL OR MAX(e.updated_at, COALESCE(e.deleted_at, '')) >= ?1",
            watermark.as_deref(),
            &mut |v| {
                v["type"] = json!("edge");
                // '' = «строка этого узла» — в бандле всегда конкретный origin
                v["origin"] = json!(self.node.effective_origin(
                    v["origin"].as_str().unwrap_or("")
                ));
                rows.push(v.clone());
            },
        )?;
        // projects
        self.collect_rows(
            "SELECT id, name, root_path, description, tech_stack, created_at, updated_at, last_scanned_at
             FROM projects WHERE ?1 IS NULL OR updated_at >= ?1",
            watermark.as_deref(),
            &mut |v| {
                v["type"] = json!("proj");
                rows.push(v.clone());
            },
        )?;

        for r in &rows {
            for key in ["updated_at", "deleted_at"] {
                if let Some(ts) = r.get(key).and_then(|v| v.as_str()) {
                    if ts > max_ts.as_str() {
                        max_ts = ts.to_string();
                    }
                }
            }
        }

        let now = utcnow();
        // Миллисекунды: несколько экспортов в одну секунду не должны
        // получать один bundle_id (иначе повторный импорт = уже применён).
        let bundle_id = format!(
            "{}-{}",
            self.node.origin,
            chrono::Utc::now().timestamp_millis()
        );
        let header = json!({
            "type": "bundle",
            "bundle_id": bundle_id,
            "origin": self.node.origin,
            "peer": peer,
            "created_at": now,
            "from": watermark.clone().unwrap_or_default(),
            "to": max_ts,
            "counts": {
                "mem": rows.iter().filter(|r| r["type"] == "mem").count(),
                "node": rows.iter().filter(|r| r["type"] == "node").count(),
                "edge": rows.iter().filter(|r| r["type"] == "edge").count(),
            },
        });

        let path = self.outbox().join(format!("{bundle_id}.jsonl.gz"));
        let enc = GzEncoder::new(
            std::fs::File::create(&path)?,
            Compression::default(),
        );
        let mut w = std::io::BufWriter::new(enc);
        writeln!(w, "{}", header)?;
        for r in &rows {
            writeln!(w, "{}", r)?;
        }
        w.flush()?;
        drop(w);

        // Watermark двигаем только после успешной записи файла.
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO sync_state (peer, last_export_at) VALUES (?1, ?2)
                 ON CONFLICT(peer) DO UPDATE SET last_export_at = excluded.last_export_at",
                params![peer, max_ts],
            )?;
            Ok(())
        })?;

        info!(
            "sync export: {} строк (mem/node/edge = {}/{}/{}), бандл {}",
            rows.len(),
            header["counts"]["mem"], header["counts"]["node"], header["counts"]["edge"],
            path.display()
        );
        Ok(path)
    }

    fn collect_rows(
        &self,
        sql: &str,
        watermark: Option<&str>,
        emit: &mut dyn FnMut(&mut serde_json::Value),
    ) -> anyhow::Result<()> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(sql)?;
            let col_names: Vec<String> =
                stmt.column_names().iter().map(|s| s.to_string()).collect();
            let mut rows = stmt.query(params![watermark])?;
            while let Some(row) = rows.next()? {
                let mut obj = serde_json::Map::new();
                for (i, name) in col_names.iter().enumerate() {
                    let val: rusqlite::types::Value = row.get(i)?;
                    obj.insert(
                        name.clone(),
                        match val {
                            rusqlite::types::Value::Null => serde_json::Value::Null,
                            rusqlite::types::Value::Integer(n) => json!(n),
                            rusqlite::types::Value::Real(f) => json!(f),
                            rusqlite::types::Value::Text(s) => json!(s),
                            rusqlite::types::Value::Blob(_) => {
                                // blob-колонки конвертируются отдельно (embedding)
                                serde_json::Value::Null
                            }
                        },
                    );
                }
                emit(&mut serde_json::Value::Object(obj));
            }
            Ok(())
        })
    }

    // -- Импорт ----------------------------------------------------------------

    /// Применить бандл. Идемпотентно по bundle_id; перед новым бандлом — авто-бэкап.
    pub async fn import_file(&self, path: &Path) -> anyhow::Result<ImportStats> {
        let raw = std::fs::read(path)
            .with_context(|| format!("чтение {}", path.display()))?;
        let decoder = GzDecoder::new(&raw[..]);
        let mut text = String::new();
        let mut reader = std::io::BufReader::new(decoder);
        reader
            .read_to_string(&mut text)
            .with_context(|| format!("распаковка {}", path.display()))?;

        let mut lines = text.lines().filter(|l| !l.trim().is_empty());
        let header_line = lines.next().context("пустой бандл")?;
        let header: serde_json::Value =
            serde_json::from_str(header_line).context("битый заголовок бандла")?;
        if header["type"] != "bundle" {
            bail!("первая строка не заголовок bundle: {}", path.display());
        }
        let bundle_id = header["bundle_id"]
            .as_str()
            .context("bundle_id отсутствует")?
            .to_string();

        if self.bundle_applied(&bundle_id)? {
            return Ok(ImportStats {
                bundle_id,
                already_applied: true,
                ..Default::default()
            });
        }

        // Авто-бэкап перед первым применением незнакомого бандла.
        if let Err(e) = self.backup.create() {
            warn!("sync import: авто-бэкап не удался: {e}");
        }

        let mut stats = ImportStats {
            bundle_id,
            ..Default::default()
        };
        let lines: Vec<serde_json::Value> = lines
            .map(|l| serde_json::from_str(l).context("битая строка бандла"))
            .collect::<Result<_, anyhow::Error>>()?;

        // Недостающие эмбеддинги досчитываем локальной моделью (та же MiniLM).
        let mut reembed_mem: Vec<(String, String)> = Vec::new();
        let mut reembed_node: Vec<(String, String)> = Vec::new();
        for l in &lines {
            match l["type"].as_str().unwrap_or("") {
                "mem" if l["embedding"].is_null() && !l["deleted_at"].is_null() => {}
                "mem" if l["embedding"].is_null() => {
                    reembed_mem.push((
                        l["key"].as_str().unwrap_or_default().to_string(),
                        l["content"].as_str().unwrap_or_default().to_string(),
                    ));
                }
                "node" if l["embedding"].is_null() && l["deleted_at"].is_null() => {
                    reembed_node.push((
                        l["node_id"].as_str().unwrap_or_default().to_string(),
                        format!(
                            "{}: {}",
                            l["label"].as_str().unwrap_or_default(),
                            l["description"].as_str().unwrap_or_default()
                        ),
                    ));
                }
                _ => {}
            }
        }
        let mut mem_emb: HashMap<String, Vec<u8>> = HashMap::new();
        if !reembed_mem.is_empty() {
            let texts: Vec<String> = reembed_mem.iter().map(|(_, t)| t.clone()).collect();
            let vecs = self
                .embedder
                .embed(&texts)
                .await
                .unwrap_or_else(|_| vec![Vec::new(); texts.len()]);
            for ((key, _), v) in reembed_mem.iter().zip(vecs.iter()) {
                if !v.is_empty() {
                    mem_emb.insert(key.clone(), vec_serialize(v));
                }
            }
        }
        let mut node_emb: HashMap<String, Vec<u8>> = HashMap::new();
        if !reembed_node.is_empty() {
            let texts: Vec<String> = reembed_node.iter().map(|(_, t)| t.clone()).collect();
            let vecs = self
                .embedder
                .embed(&texts)
                .await
                .unwrap_or_else(|_| vec![Vec::new(); texts.len()]);
            for ((node_id, _), v) in reembed_node.iter().zip(vecs.iter()) {
                if !v.is_empty() {
                    node_emb.insert(node_id.clone(), vec_serialize(v));
                }
            }
        }

        // Проекты и узлы применяем первыми (память и рёбра ссылаются на них).
        let mut ordered: Vec<&serde_json::Value> = lines.iter().collect();
        ordered.sort_by_key(|l| match l["type"].as_str().unwrap_or("") {
            "proj" => 0,
            "node" => 1,
            "mem" => 2,
            "edge" => 3,
            _ => 4,
        });

        self.db.with_tx(|tx| {
            // Вся транзакция: либо бандл применился целиком, либо откат.
            for l in ordered {
                match l["type"].as_str().unwrap_or("") {
                    "proj" => Self::apply_project(tx, l)?,
                    "mem" => Self::apply_mem(tx, l, &self.node, &mem_emb, &mut stats)?,
                    "node" => Self::apply_node(tx, l, &self.node, &node_emb, &mut stats)?,
                    "edge" => Self::apply_edge(tx, l, &self.node, &mut stats)?,
                    other => warn!("sync import: неизвестный тип строки: {other}"),
                }
            }
            let applied: String = {
                let mut stmt =
                    tx.prepare("SELECT applied_bundles FROM sync_state WHERE peer = '__imports'")?;
                let existing: Option<String> = stmt.query_row([], |r| r.get(0)).ok();
                existing.unwrap_or_else(|| "[]".to_string())
            };
            let mut list: Vec<String> = serde_json::from_str(&applied).unwrap_or_default();
            list.push(stats.bundle_id.clone());
            // держим последние 200 — защита от роста kv-значения
            if list.len() > 200 {
                let drop_n = list.len() - 200;
                list.drain(..drop_n);
            }
            let list_json = serde_json::to_string(&list)?;
            tx.execute(
                "INSERT INTO sync_state (peer, last_import_at, applied_bundles)
                 VALUES ('__imports', ?1, ?2)
                 ON CONFLICT(peer) DO UPDATE SET last_import_at = excluded.last_import_at,
                   applied_bundles = excluded.applied_bundles",
                params![utcnow(), list_json],
            )?;
            Ok(())
        })?;

        info!(
            "sync import {}: mem={} node={} edge={} конфликты_проиграны={} пропуск_ссылок={}",
            stats.bundle_id, stats.memories_applied, stats.nodes_applied, stats.edges_applied,
            stats.conflicts_lost, stats.skipped_missing_ref
        );
        Ok(stats)
    }

    fn bundle_applied(&self, bundle_id: &str) -> anyhow::Result<bool> {
        self.db.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT applied_bundles FROM sync_state WHERE peer = '__imports'")?;
            let existing: Option<String> = stmt.query_row([], |r| r.get(0)).ok();
            let list: Vec<String> =
                serde_json::from_str(&existing.unwrap_or_else(|| "[]".to_string()))
                    .unwrap_or_default();
            Ok(list.iter().any(|b| b == bundle_id))
        })
    }

    /// LWW: побеждает входящая строка?
    /// updated_at больше → да; меньше → нет; равны — выше приоритет origin.
    fn incoming_wins(node: &NodeConfig, inc_upd: &str, inc_origin: &str, ex_upd: &str, ex_origin: &str) -> bool {
        if inc_upd != ex_upd {
            return inc_upd > ex_upd;
        }
        node.rank(inc_origin) <= node.rank(ex_origin)
    }

    fn apply_mem(
        tx: &rusqlite::Transaction,
        l: &serde_json::Value,
        node: &NodeConfig,
        reembed: &HashMap<String, Vec<u8>>,
        stats: &mut ImportStats,
    ) -> anyhow::Result<()> {
        let key = l["key"].as_str().unwrap_or_default();
        if key.is_empty() {
            bail!("строка mem без key");
        }
        let get_s = |k: &str| l[k].as_str().map(|s| s.to_string());
        let embedding: Option<Vec<u8>> = match l["embedding"].as_str() {
            Some(hexstr) => Some(hex::decode(hexstr)?),
            None => reembed.get(key).cloned(),
        };
        let inc_upd = get_s("updated_at").unwrap_or_default();
        let inc_origin = get_s("origin").unwrap_or_default();
        let deleted = l["deleted_at"].as_str();
        let content_in = get_s("content").unwrap_or_default();

        /// (updated_at, origin, content, deleted_at) существующей строки memories
        type MemRow = (String, String, String, Option<String>);
        let existing: Option<MemRow> = tx
            .query_row(
                "SELECT updated_at, origin, content, deleted_at FROM memories WHERE key = ?1",
                params![key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;

        if let Some((ex_upd, ex_origin, ex_content, ex_deleted)) = &existing {
            // Идентичная строка (переотправка границы watermark) — no-op без счётчиков.
            if *ex_upd == inc_upd
                && node.effective_origin(ex_origin) == inc_origin
                && *ex_content == content_in
                && ex_deleted.as_deref() == deleted
            {
                return Ok(());
            }
        }

        if let Some((ex_upd, ex_origin, _, _)) = existing {
            if !Self::incoming_wins(node, &inc_upd, &inc_origin, &ex_upd, &ex_origin) {
                stats.conflicts_lost += 1;
                return Ok(());
            }
            tx.execute(
                "UPDATE memories SET content=?1, category=?2, importance=?3, source=?4,
                 meta=?5, embedding=?6, updated_at=?7, origin=?8, deleted_at=?9,
                 project_id=COALESCE(?10, project_id) WHERE key=?11",
                params![
                    content_in,
                    get_s("category").unwrap_or_else(|| "general".into()),
                    l["importance"].as_f64().unwrap_or(0.5),
                    get_s("source").unwrap_or_else(|| "sync".into()),
                    get_s("meta"),
                    embedding,
                    inc_upd,
                    inc_origin,
                    deleted,
                    get_s("project_id"),
                    key,
                ],
            )?;
        } else {
            tx.execute(
                "INSERT INTO memories (key, content, category, importance, source, meta, embedding,
                 created_at, updated_at, origin, deleted_at, project_id)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    key,
                    content_in,
                    get_s("category").unwrap_or_else(|| "general".into()),
                    l["importance"].as_f64().unwrap_or(0.5),
                    get_s("source").unwrap_or_else(|| "sync".into()),
                    get_s("meta"),
                    embedding,
                    get_s("created_at").unwrap_or_else(|| inc_upd.clone()),
                    inc_upd,
                    inc_origin,
                    deleted,
                    get_s("project_id"),
                ],
            )?;
        }
        stats.memories_applied += 1;
        Ok(())
    }

    fn apply_node(
        tx: &rusqlite::Transaction,
        l: &serde_json::Value,
        node: &NodeConfig,
        reembed: &HashMap<String, Vec<u8>>,
        stats: &mut ImportStats,
    ) -> anyhow::Result<()> {
        let node_id = l["node_id"].as_str().unwrap_or_default();
        if node_id.is_empty() {
            bail!("строка node без node_id");
        }
        let get_s = |k: &str| l[k].as_str().map(|s| s.to_string());
        let embedding: Option<Vec<u8>> = match l["embedding"].as_str() {
            Some(hexstr) => Some(hex::decode(hexstr)?),
            None => reembed.get(node_id).cloned(),
        };
        let inc_upd = get_s("updated_at").unwrap_or_default();
        let inc_origin = get_s("origin").unwrap_or_default();
        let deleted = l["deleted_at"].as_str();
        let desc_in = get_s("description");
        let val_in = l["val"].as_i64().unwrap_or(1);
        let proj_id = get_s("project_id");
        let prov = get_s("provenance").unwrap_or_else(|| "manual".into());
        let conf = l["confidence"].as_f64().unwrap_or(1.0);
        let fpath = get_s("file_path");
        let lstart = l["line_start"].as_i64();
        let lend = l["line_end"].as_i64();
        let is_god = l["is_god_node"].as_i64().unwrap_or(0);

        /// (updated_at, origin, description, val, deleted_at) существующей ноды
        type NodeRow = (String, String, Option<String>, i64, Option<String>);
        let existing: Option<NodeRow> = tx
            .query_row(
                "SELECT updated_at, origin, description, val, deleted_at FROM graph_nodes WHERE node_id = ?1",
                params![node_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;

        if let Some((ex_upd, ex_origin, ex_desc, ex_val, ex_deleted)) = &existing {
            if *ex_upd == inc_upd
                && node.effective_origin(ex_origin) == inc_origin
                && *ex_desc == desc_in
                && *ex_val == val_in
                && ex_deleted.as_deref() == deleted
            {
                return Ok(()); // идентичная строка — no-op
            }
        }

        if let Some((ex_upd, ex_origin, _, _, _)) = existing {
            if !Self::incoming_wins(node, &inc_upd, &inc_origin, &ex_upd, &ex_origin) {
                stats.conflicts_lost += 1;
                return Ok(());
            }
            tx.execute(
                "UPDATE graph_nodes SET label=?1, node_type=?2, description=?3, val=?4,
                 embedding=?5, updated_at=?6, origin=?7, deleted_at=?8,
                 project_id=COALESCE(?9, project_id), provenance=?10, confidence=?11,
                 file_path=?12, line_start=?13, line_end=?14, is_god_node=?15 WHERE node_id=?16",
                params![
                    get_s("label").unwrap_or_default(),
                    get_s("node_type").unwrap_or_else(|| "Other".into()),
                    desc_in,
                    val_in,
                    embedding,
                    inc_upd,
                    inc_origin,
                    deleted,
                    proj_id,
                    prov,
                    conf,
                    fpath,
                    lstart,
                    lend,
                    is_god,
                    node_id,
                ],
            )?;
        } else {
            tx.execute(
                "INSERT INTO graph_nodes (node_id, label, node_type, description, val, embedding,
                 created_at, updated_at, origin, deleted_at, project_id, provenance, confidence,
                 file_path, line_start, line_end, is_god_node)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                params![
                    node_id,
                    get_s("label").unwrap_or_default(),
                    get_s("node_type").unwrap_or_else(|| "Other".into()),
                    desc_in,
                    val_in,
                    embedding,
                    get_s("created_at").unwrap_or_else(|| inc_upd.clone()),
                    inc_upd,
                    inc_origin,
                    deleted,
                    proj_id,
                    prov,
                    conf,
                    fpath,
                    lstart,
                    lend,
                    is_god,
                ],
            )?;
        }
        stats.nodes_applied += 1;
        Ok(())
    }

    fn apply_edge(
        tx: &rusqlite::Transaction,
        l: &serde_json::Value,
        node: &NodeConfig,
        stats: &mut ImportStats,
    ) -> anyhow::Result<()> {
        let src = l["source_node"].as_str().unwrap_or_default();
        let tgt = l["target_node"].as_str().unwrap_or_default();
        let label = l["label"].as_str().unwrap_or_default();
        if src.is_empty() || tgt.is_empty() || label.is_empty() {
            bail!("строка edge без source_node/target_node/label");
        }
        let resolve = |node_id: &str| -> anyhow::Result<Option<i64>> {
            let id: Option<i64> = tx
                .query_row(
                    "SELECT id FROM graph_nodes WHERE node_id = ?1",
                    params![node_id],
                    |r| r.get(0),
                )
                .map(Some)
                .or_else(|e| if e == rusqlite::Error::QueryReturnedNoRows { Ok(None) } else { Err(e) })?;
            Ok(id)
        };
        let (Some(src_id), Some(tgt_id)) = (resolve(src)?, resolve(tgt)?) else {
            stats.skipped_missing_ref += 1;
            return Ok(());
        };
        let get_s = |k: &str| l[k].as_str().map(|s| s.to_string());
        let inc_upd = get_s("updated_at").unwrap_or_default();
        let inc_origin = get_s("origin").unwrap_or_default();
        let deleted = l["deleted_at"].as_str();
        let proj_id = get_s("project_id");
        let prov = get_s("provenance").unwrap_or_else(|| "manual".into());
        let conf = l["confidence"].as_f64().unwrap_or(1.0);

        let existing: Option<(i64, String, String)> = tx
            .query_row(
                "SELECT id, updated_at, origin FROM graph_edges
                 WHERE source_id=?1 AND target_id=?2 AND label=?3",
                params![src_id, tgt_id, label],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;

        if let Some((edge_id, ex_upd, ex_origin)) = existing {
            if !Self::incoming_wins(node, &inc_upd, &inc_origin, &ex_upd, &ex_origin) {
                stats.conflicts_lost += 1;
                return Ok(());
            }
            tx.execute(
                "UPDATE graph_edges SET weight=?1, contexts=?2, updated_at=?3, origin=?4, deleted_at=?5,
                 project_id=COALESCE(?6, project_id), provenance=?7, confidence=?8
                 WHERE id=?9",
                params![
                    l["weight"].as_f64().unwrap_or(1.0),
                    get_s("contexts"),
                    inc_upd,
                    inc_origin,
                    deleted,
                    proj_id,
                    prov,
                    conf,
                    edge_id,
                ],
            )?;
        } else {
            tx.execute(
                "INSERT INTO graph_edges (source_id, target_id, label, weight, contexts,
                 created_at, updated_at, origin, deleted_at, project_id, provenance, confidence)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    src_id,
                    tgt_id,
                    label,
                    l["weight"].as_f64().unwrap_or(1.0),
                    get_s("contexts"),
                    get_s("created_at").unwrap_or_else(|| inc_upd.clone()),
                    inc_upd,
                    inc_origin,
                    deleted,
                    proj_id,
                    prov,
                    conf,
                ],
            )?;
        }
        stats.edges_applied += 1;
        Ok(())
    }

    fn apply_project(
        tx: &rusqlite::Transaction,
        l: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let id = l["id"].as_str().unwrap_or_default();
        if id.is_empty() {
            bail!("строка proj без id");
        }
        let get_s = |k: &str| l[k].as_str().map(|s| s.to_string());
        let name = get_s("name").unwrap_or_else(|| id.to_string());
        let root_path = get_s("root_path").unwrap_or_default();
        let desc = get_s("description");
        let tech_stack = get_s("tech_stack");
        let inc_upd = get_s("updated_at").unwrap_or_else(utcnow);
        let created_at = get_s("created_at").unwrap_or_else(|| inc_upd.clone());
        let last_scanned_at = get_s("last_scanned_at");

        tx.execute(
            r#"
            INSERT INTO projects (id, name, root_path, description, tech_stack, created_at, updated_at, last_scanned_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                description = COALESCE(excluded.description, projects.description),
                tech_stack = COALESCE(excluded.tech_stack, projects.tech_stack),
                updated_at = excluded.updated_at,
                last_scanned_at = COALESCE(excluded.last_scanned_at, projects.last_scanned_at)
            WHERE excluded.updated_at >= projects.updated_at
            "#,
            params![id, name, root_path, desc, tech_stack, created_at, inc_upd, last_scanned_at],
        )?;
        Ok(())
    }

    // -- Inbox / transport -----------------------------------------------------

    /// Применить все бандлы из inbox.
    pub async fn apply_inbox(&self) -> anyhow::Result<Vec<ImportStats>> {
        let inbox = self.inbox();
        std::fs::create_dir_all(&inbox)?;
        let mut out = Vec::new();
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&inbox)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|e| e == "gz").unwrap_or(false))
            .collect();
        entries.sort();
        for path in entries {
            match self.import_file(&path).await {
                Ok(stats) => out.push(stats),
                Err(e) => warn!("sync inbox: {} не применён: {e}", path.display()),
            }
        }
        Ok(out)
    }

    fn run_scp(&self, port: Option<&str>, args: &[&str]) -> anyhow::Result<()> {
        let mut cmd = Command::new("scp");
        if let Some(p) = port {
            cmd.arg("-P").arg(p);
        }
        cmd.args(args);
        let status = cmd.status().context("запуск scp (OpenSSH-клиент установлен?)")?;
        if !status.success() {
            bail!("scp завершился с {status}");
        }
        Ok(())
    }

    /// export + копирование бандла на пир (ssh).
    pub fn push(&self, peer_name: &str) -> anyhow::Result<PathBuf> {
        let peer = self
            .node
            .peers
            .get(peer_name)
            .with_context(|| format!("пир '{peer_name}' не найден в peers.json"))?;
        if peer.method != "ssh" {
            bail!("пир '{peer_name}' method={} — push вручную из outbox", peer.method);
        }
        let bundle = self.export(peer_name)?;
        let host = peer.host.as_deref().context("peers.json: host не задан")?;
        let remote = peer.push_to.as_deref().context("peers.json: push_to не задан")?;
        let remote_path = format!("{host}:{remote}/");
        self.run_scp(peer.ssh_port.as_deref(), &[bundle.to_str().unwrap_or_default(), &remote_path])?;
        info!("sync push: {} → {}", bundle.display(), remote_path);
        Ok(bundle)
    }

    /// Забрать бандлы пира (ssh) в inbox и применить.
    pub async fn pull(&self, peer_name: &str) -> anyhow::Result<Vec<ImportStats>> {
        let peer = self
            .node
            .peers
            .get(peer_name)
            .with_context(|| format!("пир '{peer_name}' не найден в peers.json"))?;
        if peer.method != "ssh" {
            bail!("пир '{peer_name}' method={} — положите бандлы в inbox вручную", peer.method);
        }
        let host = peer.host.as_deref().context("peers.json: host не задан")?;
        let remote = peer.pull_from.as_deref().context("peers.json: pull_from не задан")?;
        let inbox = self.inbox();
        std::fs::create_dir_all(&inbox)?;
        let remote_glob = format!("{host}:{remote}/*.jsonl.gz");
        let inbox_str = inbox.to_string_lossy().to_string();
        self.run_scp(peer.ssh_port.as_deref(), &[&remote_glob, &inbox_str])?;
        self.apply_inbox().await
    }

    /// after_dream: push+pull всех ssh-пиров (best-effort).
    pub async fn run_scheduled(&self) -> anyhow::Result<()> {
        if !self.node.after_dream {
            return Ok(());
        }
        let names: Vec<String> = self
            .node
            .peers
            .iter()
            .filter(|(_, p)| p.method == "ssh")
            .map(|(n, _)| n.clone())
            .collect();
        for name in names {
            if let Err(e) = self.push(&name) {
                warn!("after_dream push {name}: {e}");
            }
            if let Err(e) = self.pull(&name).await {
                warn!("after_dream pull {name}: {e}");
            }
        }
        Ok(())
    }

    /// Человекочитаемый статус.
    pub fn status(&self) -> String {
        let mut out = format!(
            "origin: {}\npriority: {}\nafter_dream: {}\npeers: {}\n",
            self.node.origin,
            if self.node.priority.is_empty() {
                "-".to_string()
            } else {
                self.node.priority.join(" > ")
            },
            self.node.after_dream,
            if self.node.peers.is_empty() {
                "нет (data/sync/peers.json)".to_string()
            } else {
                self.node
                    .peers
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            },
        );
        let state = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT peer, last_export_at, last_import_at FROM sync_state")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })?;
            let mut list = Vec::new();
            for r in rows.flatten() {
                list.push(r);
            }
            Ok(list)
        });
        if let Ok(list) = state {
            for (peer, exp, imp) in list {
                if peer.starts_with("__") {
                    out.push_str(&format!(
                        "\nимпорты: last={}",
                        imp.unwrap_or_else(|| "-".into())
                    ));
                } else {
                    out.push_str(&format!(
                        "\npeer {peer}: export_from={:?} ",
                        exp.unwrap_or_else(|| "-".into())
                    ));
                }
            }
        }
        let outbox_count = std::fs::read_dir(self.outbox())
            .map(|d| d.filter_map(|e| e.ok()).count())
            .unwrap_or(0);
        let inbox_count = std::fs::read_dir(self.inbox())
            .map(|d| d.filter_map(|e| e.ok()).count())
            .unwrap_or(0);
        out.push_str(&format!(
            "\noutbox: {outbox_count} бандл(ов), inbox: {inbox_count}"
        ));
        out
    }
}
