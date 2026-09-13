//! Дриминг: фоновая консолидация памяти «во сне» (порт Dream из OmnesBOT).

pub mod autodream;
pub mod bench_gate;
pub mod consolidate;

use std::collections::HashMap;
use std::sync::Arc;
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use tracing::{error, warn};

use crate::config::Settings;
use crate::db::{utcnow, Database};
use crate::extractor::Extractor;
use crate::graph::GraphService;
use crate::llm::{LLMClient, LLMClientExt};
use crate::workspace::{GitStore, HistoryEntry, Workspace};

pub use autodream::AutoDreamWorker;

pub const MAX_ITERATIONS: usize = 10;

pub const PHASE1_SYSTEM: &str = "\
Ты — аналитик памяти личного агента Hermes. Проанализируй новые записи \
истории диалогов на фоне текущего состояния памяти. Найди: устойчивые факты \
о пользователе и его проектах; изменения, противоречащие памяти; что стоит \
добавить в MEMORY.md (факты) или USER.md (о владельце); что устарело и его \
пора поправить. Отвечай кратко по-русски списком. Это анализ — файлы правит \
следующая фаза.";

pub const PHASE2_SYSTEM: &str = "\
Ты — редактор памяти личного агента. Твоя задача — внести точечные правки в \
MD-файлы памяти по анализу. За один шаг — РОВНО ОДНО действие, верни СТРОГО JSON:
{\"action\": \"edit\", \"file\": \"memory|soul|user\", \"old\": \"точный существующий фрагмент\",
 \"new\": \"замена\"}
или {\"action\": \"read\", \"file\": \"memory|soul|user\"} — перечитать файл,
или {\"action\": \"done\", \"summary\": \"что изменено overall\"}.
Правки минимальные: не переписывай файлы целиком, old должен совпадать буквально. \
Если правило или факт уже хранится в долгосрочной памяти ob2h — НЕ дублируй его в \
builtin MEMORY.md, предлагай только новое или изменившееся (дедуп сторов). \
Если править нечего — сразу done.";

pub const REVISION_SYSTEM: &str = "\
Ты — ревизор памяти личного агента. Тебе дают записи с минимальным доверием \
и свежую историю диалогов. Оцени каждую запись: confirmed — по-прежнему верна; \
outdated — устарела (есть более свежая информация); contradicted — противоречит \
фактам из истории. Не выдумывай ключи. Верни СТРОГО JSON-массив.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamStats {
    pub run_id: i64,
    pub status: String,
    pub processed: usize,
    pub edits: usize,
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph_entities: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph_edges: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_revision: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ralph_revision: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bench_gate: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consolidation: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Deserialize)]
struct Phase2Action {
    action: Option<String>,
    file: Option<String>,
    old: Option<String>,
    new: Option<String>,
    #[allow(dead_code)]
    summary: Option<String>,
}

#[derive(Deserialize)]
struct RevisionVerdict {
    key: String,
    verdict: String,
}

#[derive(Deserialize)]
struct RalphRevisionVerdict {
    id: String,
    verdict: String,
}

pub struct Dream {
    workspace: Arc<Workspace>,
    gitstore: Arc<GitStore>,
    llm: Arc<dyn LLMClient>,
    settings: Settings,
    db: Database,
    graph: Option<Arc<GraphService>>,
    memory: Option<Arc<crate::memory::MemoryService>>,
    ralph: Option<Arc<crate::ralph::RalphService>>,
}

impl Dream {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workspace: Arc<Workspace>,
        gitstore: Arc<GitStore>,
        llm: Arc<dyn LLMClient>,
        settings: Settings,
        db: Database,
        graph: Option<Arc<GraphService>>,
        memory: Option<Arc<crate::memory::MemoryService>>,
        ralph: Option<Arc<crate::ralph::RalphService>>,
    ) -> Self {
        Self {
            workspace,
            gitstore,
            llm,
            settings,
            db,
            graph,
            memory,
            ralph,
        }
    }

    pub async fn run(&self, trigger: &str) -> anyhow::Result<DreamStats> {
        let started = utcnow();
        let run_id: i64 = self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO dream_runs (started_at, status, trigger) VALUES (?1, 'running', ?2)",
                params![started, trigger],
            )?;
            Ok(conn.last_insert_rowid())
        })?;

        match self.execute_dream(run_id).await {
            Ok(stats) => {
                self.finish_run(run_id, "ok", &stats)?;
                Ok(stats)
            }
            Err(e) => {
                error!("Dream run failed: {e}");
                let err_stats = DreamStats {
                    run_id,
                    status: "error".to_string(),
                    processed: 0,
                    edits: 0,
                    commit: None,
                    graph_entities: None,
                    graph_edges: None,
                    memory_revision: None,
                    ralph_revision: None,
                    bench_gate: None,
                    consolidation: None,
                    note: None,
                    error: Some(e.to_string()),
                };
                self.finish_run(run_id, "error", &err_stats)?;
                Ok(err_stats)
            }
        }
    }

    fn finish_run(&self, run_id: i64, status: &str, stats: &DreamStats) -> anyhow::Result<()> {
        let now = utcnow();
        let stats_json = serde_json::to_string(stats)?;
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE dream_runs SET finished_at = ?1, status = ?2, stats = ?3 WHERE id = ?4",
                params![now, status, stats_json, run_id],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    async fn execute_dream(&self, run_id: i64) -> anyhow::Result<DreamStats> {
        let dream_cursor = self.workspace.get_dream_cursor()?.unwrap_or(0);
        let new_records = self.workspace.read_history_from_cursor(dream_cursor, self.settings.dream_batch)?;

        if new_records.is_empty() {
            return Ok(DreamStats {
                run_id,
                status: "ok".to_string(),
                processed: 0,
                edits: 0,
                commit: None,
                graph_entities: None,
                graph_edges: None,
                memory_revision: None,
                ralph_revision: None,
                bench_gate: None,
                consolidation: None,
                note: Some("нет новых записей с прошлого дрима".to_string()),
                error: None,
            });
        }

        // Фаза 1: Анализ
        let analysis = self.phase1(&new_records).await?;

        // Фаза 2: Агентный цикл правок
        let edits = self.phase2(&analysis).await?;

        // Извлечение сессионных фактов в общий граф (Dream Extract)
        let (graph_entities, graph_edges) = self.extract_to_graph(&new_records).await?;

        // Ф23.2: ревизия памяти — trust-вердикты по записям с минимальным доверием
        let revisions = self.revise_memory().await.unwrap_or_else(|e| {
            warn!("Dream-ревизия памяти не удалась: {e}");
            Vec::new()
        });

        // Ф31: офлайн-консолидация — LLM-вердикты по merge-кандидатам + compaction
        let consolidation = match self.consolidate_memory().await {
            Ok(r) => {
                let nontrivial = !r["merges"].as_array().unwrap_or(&vec![]).is_empty()
                    || !r["edges"].as_array().unwrap_or(&vec![]).is_empty()
                    || !r["digests"].as_array().unwrap_or(&vec![]).is_empty()
                    || !r["keep_both"].as_array().unwrap_or(&vec![]).is_empty();
                if nontrivial { Some(r) } else { None }
            }
            Err(e) => {
                warn!("Консолидация памяти не удалась: {e}");
                None
            }
        };

        // Ф28.1 (FR-K7): ревизия stale-findings Ralph по свежему контексту
        let ralph_revision = self.revise_ralph_findings().await.unwrap_or_else(|e| {
            warn!("Dream-ревизия Ralph не удалась: {e}");
            Vec::new()
        });

        let new_cursor = new_records.iter().map(|r| r.cursor).max().unwrap_or(dream_cursor);
        self.workspace.set_dream_cursor(new_cursor)?;
        let _ = self.workspace.compact_history(1000);

        let now_str = Utc::now().format("%Y-%m-%d %H:%M").to_string();
        let mut commit_msg = if revisions.is_empty() {
            format!("dream: {now_str} (+{} правок)", edits.len())
        } else {
            format!(
                "dream: {now_str} (+{} правок, {} ревизий trust)",
                edits.len(),
                revisions.len()
            )
        };
        if !ralph_revision.is_empty() {
            commit_msg.push_str(&format!(" + {} ревизий ralph", ralph_revision.len()));
        }
        let commit = self.gitstore.auto_commit(&commit_msg);

        Ok(DreamStats {
            run_id,
            status: "ok".to_string(),
            processed: new_records.len(),
            edits: edits.len(),
            commit,
            graph_entities: Some(graph_entities),
            graph_edges: Some(graph_edges),
            memory_revision: if revisions.is_empty() { None } else { Some(revisions) },
            ralph_revision: if ralph_revision.is_empty() { None } else { Some(ralph_revision) },
            bench_gate: None,
            consolidation,
            note: None,
            error: None,
        })
    }

    async fn phase1(&self, records: &[HistoryEntry]) -> anyhow::Result<String> {
        let history = records
            .iter()
            .map(|r| {
                let preview: String = r.content.chars().take(800).collect();
                format!("[{}] {preview}", r.timestamp)
            })
            .collect::<Vec<_>>()
            .join("\n");

        let files = [
            format!("=== MEMORY.md ===\n{}", self.workspace.read_file("memory")?),
            format!("=== SOUL.md ===\n{}", self.workspace.read_file("soul")?),
            format!("=== USER.md ===\n{}", self.workspace.read_file("user")?),
        ]
        .join("\n\n");

        let prompt = format!("Новые записи истории:\n{history}\n\n{files}");
        let analysis = self.llm.ask(&prompt, Some(PHASE1_SYSTEM)).await?;
        Ok(analysis)
    }

    async fn phase2(&self, analysis: &str) -> anyhow::Result<Vec<serde_json::Value>> {
        let mut applied = Vec::new();
        let mut context_files: HashMap<String, String> = HashMap::new();
        for name in &["memory", "soul", "user"] {
            context_files.insert(name.to_string(), self.workspace.read_file(name)?);
        }

        let mut last_error = String::new();

        for _ in 0..MAX_ITERATIONS {
            let files_block = context_files
                .iter()
                .map(|(n, c)| format!("=== {n}.md ===\n{c}"))
                .collect::<Vec<_>>()
                .join("\n\n");

            let mut prompt = format!("Анализ:\n{analysis}\n\n{files_block}");
            if !last_error.is_empty() {
                prompt.push_str(&format!("\n\nОшибка прошлого шага (исправь): {last_error}"));
            }
            prompt.push_str("\n\nТвоё действие (JSON):");

            let action: Phase2Action = match self.llm.ask_json(&prompt, Some(PHASE2_SYSTEM)).await {
                Ok(act) => act,
                Err(e) => {
                    warn!("Фаза 2: LLM JSON ошибка: {e}");
                    break;
                }
            };

            let act = action.action.as_deref().unwrap_or("done");
            if act == "done" {
                break;
            }

            if act == "read" {
                if let Some(ref file) = action.file {
                    if context_files.contains_key(file) {
                        context_files.insert(file.clone(), self.workspace.read_file(file)?);
                        last_error.clear();
                        continue;
                    }
                }
            }

            if act == "edit" {
                if let Some(ref file) = action.file {
                    let old_str = action.old.unwrap_or_default();
                    let new_str = action.new.unwrap_or_default();

                    if !context_files.contains_key(file.as_str()) {
                        last_error = "file должен быть memory|soul|user".to_string();
                        continue;
                    }

                    if old_str.is_empty() || new_str.is_empty() || old_str == new_str {
                        last_error = "пустые или одинаковые old/new".to_string();
                        continue;
                    }

                    let content = self.workspace.read_file(&file)?;
                    if !content.contains(&old_str) {
                        last_error = format!("фрагмент не найден в {file}.md (проверь дословно)");
                        continue;
                    }

                    let updated = content.replacen(&old_str, &new_str, 1);
                    self.workspace.write_file(&file, &updated)?;
                    context_files.insert(file.clone(), updated);
                    applied.push(serde_json::json!({
                        "file": file,
                        "old": old_str.chars().take(100).collect::<String>(),
                        "new": new_str.chars().take(100).collect::<String>(),
                    }));
                    last_error.clear();
                    continue;
                }
            }

            last_error = format!("неизвестное действие {:?}", act);
        }

        Ok(applied)
    }

    async fn extract_to_graph(&self, records: &[HistoryEntry]) -> anyhow::Result<(usize, usize)> {
        if let Some(ref graph) = self.graph {
            if self.settings.dream_extract_enabled {
                let combined_text = records
                    .iter()
                    .map(|r| r.content.chars().take(1500).collect::<String>())
                    .collect::<Vec<_>>()
                    .join("\n");

                if combined_text.chars().count() >= 80 {
                    let extractor = Extractor::new(self.llm.clone(), 30);
                    if let Ok(extracted) = extractor.extract(&combined_text).await {
                        let (new_nodes, updated_nodes, new_edges) = graph.upsert_extraction(&extracted).await?;
                        return Ok((new_nodes + updated_nodes, new_edges));
                    }
                }
            }
        }
        Ok((0, 0))
    }

    /// Ф23.2: dream-ревизия памяти — LLM-вердикты по записям с минимальным trust.
    /// Никаких автоудалений: только bounded сдвиги trust (кламп [0,1]).
    async fn revise_memory(&self) -> anyhow::Result<Vec<serde_json::Value>> {
        if !self.settings.dream_memory_revision {
            return Ok(Vec::new());
        }
        let Some(ref memory) = self.memory else {
            return Ok(Vec::new());
        };
        let low = memory.lowest_trust(10)?;
        if low.is_empty() {
            return Ok(Vec::new());
        }

        let list = low
            .iter()
            .map(|(r, t)| {
                let preview: String = r.content.chars().take(200).collect();
                format!("- key={} trust={t:.2}: {preview}", r.key)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = format!(
            "Записи памяти с минимальным доверием:\n{list}\n\n\
             Для каждой записи верни JSON-массив объектов \
             {{\"key\": \"...\", \"verdict\": \"confirmed|outdated|contradicted\"}}."
        );
        let verdicts: Vec<RevisionVerdict> = self
            .llm
            .ask_json(&prompt, Some(REVISION_SYSTEM))
            .await
            .unwrap_or_default();

        let mut applied = Vec::new();
        for v in verdicts {
            match memory.revise_trust_by_key(&v.key, &v.verdict) {
                Ok(Some(trust)) => applied.push(
                    serde_json::json!({ "key": v.key, "verdict": v.verdict, "trust": trust }),
                ),
                Ok(None) => {}
                Err(e) => warn!("Ревизия {}: {e}", v.key),
            }
        }
        Ok(applied)
    }

    /// Ф30: дописать исход bench-гейта в stats последнего дрима —
    /// исход виден в dream_status/dream_log без изменения контракта.
    pub fn append_run_gate(&self, run_id: i64, gate: serde_json::Value) -> anyhow::Result<()> {
        self.db.with_conn(|conn| {
            let stats: Option<String> = conn
                .query_row(
                    "SELECT stats FROM dream_runs WHERE id = ?1",
                    params![run_id],
                    |r| r.get(0),
                )
                .ok();
            let mut val: serde_json::Value = stats
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(serde_json::json!({}));
            val["bench_gate"] = gate;
            conn.execute(
                "UPDATE dream_runs SET stats = ?1 WHERE id = ?2",
                params![val.to_string(), run_id],
            )?;
            Ok(())
        })
    }

    /// Ф28.1 (FR-K7): ревизия stale-findings Ralph — LLM по свежей истории решает,
    /// подтвердилось ли знание (verified, source=dream) или остаётся устаревшим.
    async fn revise_ralph_findings(&self) -> anyhow::Result<Vec<serde_json::Value>> {
        if !self.settings.dream_ralph_revision {
            return Ok(Vec::new());
        }
        let Some(ref ralph) = self.ralph else {
            return Ok(Vec::new());
        };
        let stale = ralph.stale_findings(10)?;
        if stale.is_empty() {
            return Ok(Vec::new());
        }

        let list = stale
            .iter()
            .map(|(id, project, content, symbols)| {
                let head: String = content.chars().take(200).collect();
                format!("- id={id} project={project} symbols={symbols}: {head}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = format!(
            "Знания, помеченные устаревшими после изменений кода:\n{list}\n\n\
             Свежая история диалога могла их подтвердить. Для каждой записи верни JSON-массив \
             {{\"id\": \"...\", \"verdict\": \"reverified|stays_stale\"}}: reverified — знание \
             снова верно; stays_stale — остаётся устаревшим."
        );
        let verdicts: Vec<RalphRevisionVerdict> = self
            .llm
            .ask_json(&prompt, Some(REVISION_SYSTEM))
            .await
            .unwrap_or_default();

        let mut applied = Vec::new();
        for v in verdicts {
            if v.verdict == "reverified" {
                if ralph
                    .set_verdict(Some(&v.id), None, "verified", "dream")
                    .is_ok()
                {
                    applied.push(serde_json::json!({ "finding": v.id, "verdict": "verified (dream)" }));
                }
            } else if v.verdict == "stays_stale" {
                applied.push(serde_json::json!({ "finding": v.id, "verdict": "stale (подтверждено дримом)" }));
            }
        }
        Ok(applied)
    }

    pub fn last_status(&self) -> anyhow::Result<Option<serde_json::Value>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, started_at, finished_at, status, trigger, stats FROM dream_runs ORDER BY id DESC LIMIT 1",
            )?;
            let mut rows = stmt.query([])?;
            if let Some(row) = rows.next()? {
                let id: i64 = row.get(0)?;
                let started_at: Option<String> = row.get(1)?;
                let finished_at: Option<String> = row.get(2)?;
                let status: Option<String> = row.get(3)?;
                let trigger: Option<String> = row.get(4)?;
                let stats: Option<String> = row.get(5)?;
                let stats_val: serde_json::Value = stats.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(serde_json::json!({}));

                Ok(Some(serde_json::json!({
                    "id": id,
                    "started_at": started_at,
                    "finished_at": finished_at,
                    "status": status,
                    "trigger": trigger,
                    "stats": stats_val,
                })))
            } else {
                Ok(None)
            }
        })
    }
}
