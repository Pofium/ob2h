//! Ralph Knowledge Layer (Фазы 26–27, PLAN_v1.3; спека `Ralph Knowledge Layer (ob2h)_v1.2.3.md`).
//! Циклы разработки: runs/iterations/findings + AST-дельты, авто-вердикты только по
//! объективным сигналам (ADR-K4), staleness-pass по символам, контекст-пакет с
//! reuse-кандидатами из AST-графа.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::db::{utcnow, Database};
use crate::project::ProjectService;

static SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Serialize)]
pub struct IterationOutcome {
    pub iteration_id: String,
    pub project_id: String,
    pub verdict: String,
    pub verdict_source: String,
    pub ast_changes: usize,
    pub stale_marked: usize,
    pub findings: usize,
}

pub struct RalphService {
    db: Database,
    project: Arc<ProjectService>,
    data_dir: std::path::PathBuf,
}

fn new_id(prefix: &str) -> String {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut h = Sha256::new();
    h.update(utcnow().as_bytes());
    h.update(std::process::id().to_le_bytes());
    h.update(seq.to_le_bytes());
    format!("{prefix}_{}", &hex::encode(h.finalize())[..16])
}

fn auto_verdict(tests_summary: Option<&str>) -> (String, String) {
    // ADR-K4: вердикт только по объективному сигналу; self_assessment не читается.
    match tests_summary.and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()) {
        Some(v) => {
            let failed = v.get("failed").and_then(|x| x.as_i64()).unwrap_or(0);
            let passed = v.get("passed").and_then(|x| x.as_i64()).unwrap_or(0);
            if failed > 0 {
                ("failed".into(), "auto_tests".into())
            } else if passed > 0 {
                ("verified".into(), "auto_tests".into())
            } else {
                ("unconfirmed".into(), "auto_tests".into())
            }
        }
        None => ("unconfirmed".into(), "none".into()),
    }
}

fn symbols_of(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(json).unwrap_or_default()
}

impl RalphService {
    pub fn new(db: Database, project: Arc<ProjectService>, data_dir: std::path::PathBuf) -> Self {
        Self { db, project, data_dir }
    }

    /// ralph_start: один активный run на (project, feature).
    pub fn start(
        &self,
        project_id: &str,
        feature_slug: &str,
        goal: &str,
        autonomy: Option<&str>,
        max_per_task: Option<i64>,
        max_total: Option<i64>,
        budget_tokens: Option<i64>,
    ) -> anyhow::Result<String> {
        let active: Option<String> = self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT id FROM ralph_runs WHERE project_id = ?1 AND feature_slug = ?2 \
                 AND status NOT IN ('archived','stopped','failed')",
                params![project_id, feature_slug],
                |r| r.get(0),
            )
            .optional()
        })?;
        if active.is_some() {
            anyhow::bail!("активный run для ({project_id}, {feature_slug}) уже существует — сначала ralph_report/archive");
        }
        let id = new_id("rrun");
        let now = utcnow();
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO ralph_runs (id, project_id, feature_slug, goal, status, autonomy, \
                 max_iterations_per_task, max_total_iterations, budget_tokens, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, 'applying', ?5, ?6, ?7, ?8, ?9, ?9)",
                params![
                    id,
                    project_id,
                    feature_slug,
                    goal,
                    autonomy.unwrap_or("L1"),
                    max_per_task.unwrap_or(5),
                    max_total.unwrap_or(60),
                    budget_tokens,
                    now
                ],
            )?;
            Ok(())
        })?;
        Ok(id)
    }

    /// ralph_iteration: запись итерации + авто-вердикт + AST-рескан с дельтой + staleness-pass.
    #[allow(clippy::too_many_arguments)]
    pub fn iteration(
        &self,
        run_id: &str,
        task_id: &str,
        n: i64,
        hypothesis: Option<&str>,
        plan: Option<&str>,
        result: Option<&str>,
        tests_summary: Option<&str>,
        ladder_rung: Option<&str>,
        git_before: Option<&str>,
        git_after: Option<&str>,
        findings: Option<&serde_json::Value>,
    ) -> anyhow::Result<IterationOutcome> {
        let (project_id, feature_slug): (String, String) = self
            .db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT project_id, feature_slug FROM ralph_runs WHERE id = ?1",
                    params![run_id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()
            })?
            .ok_or_else(|| anyhow::anyhow!("run не найден: {run_id}"))?;

        let (verdict, verdict_source) = auto_verdict(tests_summary);
        let iteration_id = new_id("rit");
        let now = utcnow();
        let inserted = self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO ralph_iterations (id, run_id, task_id, n, git_before, git_after, \
                 hypothesis, plan, result, tests_summary, verdict, verdict_source, ladder_rung, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    iteration_id,
                    run_id,
                    task_id,
                    n,
                    git_before,
                    git_after,
                    hypothesis,
                    plan,
                    result,
                    tests_summary,
                    verdict,
                    verdict_source,
                    ladder_rung,
                    now
                ],
            )?;
            Ok(conn.execute(
                "UPDATE ralph_runs SET status = 'verifying', updated_at = ?2 WHERE id = ?1",
                params![run_id, utcnow()],
            )?)
        })?;
        if inserted == 0 {
            anyhow::bail!("run не найден");
        }

        // AST-рескан (ADR-K3): дельта по сигнатурам узлов, не git-дифф.
        let before = self.node_snapshot(&project_id)?;
        let _scan = self.project.scan_project(&project_id, None, true)?;
        let after = self.node_snapshot(&project_id)?;
        let (ast_changes, stale_marked) =
            self.record_ast_delta(&project_id, run_id, &iteration_id, &before, &after)?;

        // Findings итерации (Ponytail-маркеры kind=deferred и опыт)
        let mut findings_added = 0usize;
        if let Some(list) = findings.and_then(|v| v.as_array()) {
            for f in list {
                let kind = f.get("kind").and_then(|x| x.as_str()).unwrap_or("gotcha");
                let Some(content) = f.get("content").and_then(|x| x.as_str()) else {
                    continue;
                };
                let symbols = f.get("symbols").map(|s| s.to_string());
                let meta = f.get("meta").map(|m| m.to_string());
                if self
                    .add_finding(&project_id, Some(run_id), Some(&iteration_id), kind, content, symbols.as_deref(), meta.as_deref())
                    .is_ok()
                {
                    findings_added += 1;
                }
            }
        }

        Ok(IterationOutcome {
            iteration_id,
            project_id,
            verdict,
            verdict_source,
            ast_changes,
            stale_marked,
            findings: findings_added,
        })
    }

    fn node_snapshot(
        &self,
        project_id: &str,
    ) -> anyhow::Result<HashMap<String, (String, String, String)>> {
        // key: path|label|type → (node_id-сигнатура, path, label)
        let mut map = HashMap::new();
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT file_path, label, node_type, node_id FROM graph_nodes \
                 WHERE project_id = ?1 AND deleted_at IS NULL",
            )?;
            let rows = stmt.query_map(params![project_id], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?;
            for (path, label, ntype, sig) in rows.flatten() {
                map.insert(format!("{path}|{label}|{ntype}"), (sig, path, label));
            }
            Ok(())
        })?;
        Ok(map)
    }

    #[allow(clippy::too_many_arguments)]
    fn record_ast_delta(
        &self,
        project_id: &str,
        run_id: &str,
        iteration_id: &str,
        before: &HashMap<String, (String, String, String)>,
        after: &HashMap<String, (String, String, String)>,
    ) -> anyhow::Result<(usize, usize)> {
        let now = utcnow();
        let mut changed_labels: HashSet<String> = HashSet::new();
        let mut count = 0usize;
        let mut write_change = |conn: &rusqlite::Connection,
                                path: &str,
                                label: &str,
                                ntype: &str,
                                change: &str,
                                sig_before: Option<&str>,
                                sig_after: Option<&str>|
         -> rusqlite::Result<()> {
            let mut h = Sha256::new();
            h.update(format!("{label}|{ntype}|{path}").as_bytes());
            let node_key = hex::encode(h.finalize());
            conn.execute(
                "INSERT INTO ast_changes (project_id, run_id, iteration_id, path, node_key, \
                 label, node_type, change_type, sig_before, sig_after, loc_delta, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, ?11)",
                params![
                    project_id,
                    run_id,
                    iteration_id,
                    path,
                    node_key,
                    label,
                    ntype,
                    change,
                    sig_before,
                    sig_after,
                    now
                ],
            )?;
            count += 1;
            if change != "added" {
                changed_labels.insert(label.to_string());
            }
            Ok(())
        };

        self.db.with_conn(|conn| {
            for (key, (sig, path, label)) in after {
                match before.get(key) {
                    None => {
                        let ntype = key.rsplit('|').next().unwrap_or("other").to_string();
                        write_change(conn, path, label, &ntype, "added", None, Some(sig))?;
                    }
                    Some((old_sig, _, _)) if old_sig != sig => {
                        let ntype = key.rsplit('|').next().unwrap_or("other").to_string();
                        write_change(conn, path, label, &ntype, "modified", Some(old_sig), Some(sig))?;
                    }
                    _ => {}
                }
            }
            for (key, (sig, path, label)) in before {
                if !after.contains_key(key) {
                    let ntype = key.rsplit('|').next().unwrap_or("other").to_string();
                    write_change(conn, path, label, &ntype, "removed", Some(sig), None)?;
                }
            }
            Ok(())
        })?;

        // FR-K5: staleness-pass — модифицированные/удалённые символы инвалидируют findings.
        let mut stale_marked = 0;
        for label in &changed_labels {
            let like = format!("%{label}%");
            let rows: Vec<(String, String)> = self.db.with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, symbols FROM ralph_findings \
                     WHERE project_id = ?1 AND verdict != 'stale' AND symbols LIKE ?2",
                )?;
                let rows = stmt
                    .query_map(params![project_id, like], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                    })?
                    .flatten()
                    .collect();
                Ok(rows)
            })?;
            for (fid, symbols_json) in rows {
                let hit = symbols_of(&symbols_json)
                    .iter()
                    .any(|s| s.rsplit(':').next().map(|n| n == label).unwrap_or(false));
                if !hit {
                    continue;
                }
                self.db.with_conn(|conn| {
                    conn.execute(
                        "UPDATE ralph_findings SET verdict = 'stale', stale_at = ?2 WHERE id = ?1",
                        params![fid, utcnow()],
                    )?;
                    Ok(())
                })?;
                stale_marked += 1;
            }
        }
        Ok((count, stale_marked))
    }

    /// Корень проекта (для тестов и скилла).
    pub fn project_root(&self, project_id: &str) -> anyhow::Result<Option<String>> {
        self.project.get_project(project_id).map(|p| p.map(|p| p.root_path))
    }

    /// Finding цикла: hypothesis|gotcha|decision|constraint|deferred (Ponytail-маркер).
    pub fn add_finding(
        &self,
        project_id: &str,
        run_id: Option<&str>,
        iteration_id: Option<&str>,
        kind: &str,
        content: &str,
        symbols: Option<&str>,
        meta: Option<&str>,
    ) -> anyhow::Result<String> {
        const KINDS: [&str; 5] = ["hypothesis", "gotcha", "decision", "constraint", "deferred"];
        if !KINDS.contains(&kind) {
            anyhow::bail!("kind должен быть одним из {KINDS:?}");
        }
        let id = new_id("rfin");
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO ralph_findings (id, run_id, iteration_id, project_id, kind, content, \
                 symbols, verdict, meta, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, COALESCE(?7, '[]'), 'unconfirmed', ?8, ?9)",
                params![id, run_id, iteration_id, project_id, kind, content, symbols, meta, utcnow()],
            )?;
            Ok(())
        })?;
        Ok(id)
    }

    /// ralph_verdict: смена вердикта итерации или finding'а (человек/дрим/агент).
    pub fn set_verdict(
        &self,
        iteration_id: Option<&str>,
        finding_id: Option<&str>,
        verdict: &str,
        source: &str,
    ) -> anyhow::Result<String> {
        const ALLOWED: [&str; 5] = ["verified", "failed", "unconfirmed", "overturned", "stale"];
        if !ALLOWED.contains(&verdict) {
            anyhow::bail!("verdict должен быть одним из {ALLOWED:?}");
        }
        if let Some(fid) = finding_id {
            let n = self.db.with_conn(|conn| {
                conn.execute(
                    "UPDATE ralph_findings SET verdict = ?2, verdict_source = ?3 WHERE id = ?1",
                    params![fid, verdict, source],
                )
            })?;
            if n == 0 {
                anyhow::bail!("finding не найден: {fid}");
            }
            return Ok(format!("finding {fid} → {verdict}"));
        }
        let Some(iid) = iteration_id else {
            anyhow::bail!("нужен iteration_id или finding_id");
        };
        let n = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE ralph_iterations SET verdict = ?2, verdict_source = ?3 WHERE id = ?1",
                params![iid, verdict, source],
            )
        })?;
        if n == 0 {
            anyhow::bail!("итерация не найдена: {iid}");
        }
        Ok(format!("iteration {iid} → {verdict}"))
    }

    /// Ф27: контекст-пакет (спека фрагмент → негатив → reuse → инварианты → архитектура).
    pub fn context(
        &self,
        run_id: &str,
        task_id: &str,
        max_tokens: usize,
        mode: &str,
    ) -> anyhow::Result<String> {
        let (project_id, feature_slug): (String, String) = self
            .db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT project_id, feature_slug FROM ralph_runs WHERE id = ?1",
                    params![run_id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()
            })?
            .ok_or_else(|| anyhow::anyhow!("run не найден: {run_id}"))?;
        let budget = max_tokens.saturating_mul(4).min(64_000); // ~4 символа/токен
        let lite_budget = budget / 2;
        let budget = if mode == "lite" { lite_budget } else { budget };

        let root: String = self
            .db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT root_path FROM projects WHERE id = ?1",
                    params![project_id],
                    |r| r.get(0),
                )
                .optional()
            })?
            .unwrap_or_default();

        let read_head = |path: &std::path::Path, cap: usize| -> String {
            std::fs::read_to_string(path)
                .map(|t| t.chars().take(cap).collect())
                .unwrap_or_default()
        };

        // 1. Фрагмент спеки задачи (до 30% бюджета)
        let spec_cap = budget * 3 / 10;
        let mut spec = String::new();
        for name in ["proposal.md", "design.md", "tasks.md"] {
            spec.push_str(&read_head(
                &std::path::Path::new(&root)
                    .join("openspec/changes")
                    .join(&feature_slug)
                    .join(name),
                800,
            ));
        }

        // 2. Негативный опыт (до 25%)
        let neg_cap = budget / 4;
        let negative: Vec<String> = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT kind, verdict, content FROM ralph_findings \
                 WHERE project_id = ?1 AND verdict IN ('failed','overturned') \
                 ORDER BY created_at DESC LIMIT 10",
            )?;
            let rows = stmt.query_map(params![project_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            Ok(rows
                .flatten()
                .map(|(k, v, c)| {
                    let head: String = c.chars().take(200).collect();
                    format!("- [{v}, {k}] {head}")
                })
                .collect())
        })?;

        // 2.5 Reuse-кандидаты из AST-графа (до 15%, детерминированно)
        let reuse_cap = budget * 3 / 20;
        let reuse: Vec<String> = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT label, node_type, COALESCE(file_path,''), is_god_node, val \
                 FROM graph_nodes WHERE project_id = ?1 AND deleted_at IS NULL \
                 AND node_type IN ('Function','Method','Struct','Class','Interface','Trait') \
                 ORDER BY is_god_node DESC, val DESC, label LIMIT 8",
            )?;
            let rows = stmt.query_map(params![project_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })?;
            Ok(rows
                .flatten()
                .map(|(label, ntype, path, god, val)| {
                    format!(
                        "- {}:{} ({}){}",
                        &ntype[..1],
                        label,
                        path.as_deref().unwrap_or("-"),
                        if god == 1 { format!(" — god node, val={val}") } else { String::new() }
                    )
                })
                .collect())
        })?;

        // 3. Инварианты (до 10%)
        let inv_cap = budget / 10;
        let mut invariants = String::new();
        let specs_dir = std::path::Path::new(&root).join("openspec/specs");
        if let Ok(entries) = std::fs::read_dir(&specs_dir) {
            for e in entries.flatten().take(5) {
                if e.path().extension().map(|x| x == "md").unwrap_or(false) {
                    invariants.push_str(&read_head(&e.path(), 400));
                }
            }
        }

        // 4. Архитектурная зона (до 10%): god nodes
        let arch_cap = budget / 10;
        let arch: Vec<String> = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT label, COALESCE(file_path,'') FROM graph_nodes \
                 WHERE project_id = ?1 AND is_god_node = 1 AND deleted_at IS NULL \
                 ORDER BY val DESC LIMIT 3",
            )?;
            let rows = stmt.query_map(params![project_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?;
            Ok(rows
                .flatten()
                .map(|(l, p)| format!("- god node {} ({})", l, p.as_deref().unwrap_or("-")))
                .collect())
        })?;

        let take = |items: &[String], cap: usize| -> String {
            let mut used = 0;
            let mut out = String::new();
            for it in items {
                let len = it.chars().count() + 1;
                if used + len > cap {
                    break;
                }
                used += len;
                out.push_str(it);
                out.push('\n');
            }
            out
        };

        let mut out = String::new();
        out.push_str(&format!(
            "<ralph_context run=\"{run_id}\" task=\"{task_id}\" mode=\"{mode}\">\n"
        ));
        if !spec.is_empty() {
            out.push_str(&format!(
                "<spec_task>\n{}\n</spec_task>\n",
                spec.chars().take(spec_cap).collect::<String>()
            ));
        }
        let neg_block = take(&negative, neg_cap);
        if !neg_block.is_empty() {
            out.push_str(&format!("<negative_experience>\n{neg_block}</negative_experience>\n"));
        }
        let reuse_block = take(&reuse, reuse_cap);
        if !reuse_block.is_empty() {
            out.push_str(&format!("<reuse_candidates>\n{reuse_block}</reuse_candidates>\n"));
        }
        if !invariants.is_empty() && mode != "lite" {
            out.push_str(&format!(
                "<invariants>\n{}\n</invariants>\n",
                invariants.chars().take(inv_cap).collect::<String>()
            ));
        }
        let arch_block = take(&arch, arch_cap);
        if !arch_block.is_empty() && mode != "lite" {
            out.push_str(&format!("<architecture_zone>\n{arch_block}</architecture_zone>\n"));
        }
        out.push_str("</ralph_context>");

        // NFR-K5: пакет на диск для аудита
        let dir = self.data_dir.join("ralph/contexts").join(run_id);
        if std::fs::create_dir_all(&dir).is_ok() {
            let path = dir.join(format!("{task_id}-{}.md", utcnow().replace(':', "-")));
            let _ = std::fs::write(&path, &out);
            let mut h = Sha256::new();
            h.update(out.as_bytes());
            out.push_str(&format!(
                "\n<!-- context_ref: {} sha256:{} -->",
                path.display(),
                &hex::encode(h.finalize())[..16]
            ));
        }
        Ok(out)
    }

    /// ralph_report: сводка по run (или по всем) + debt-леджер + gain-метрики.
    pub fn report(&self, run_id: Option<&str>) -> anyhow::Result<String> {
        let (runs, iterations, findings, deferred, reuse_hits, total_iters): (
            i64,
            i64,
            i64,
            Vec<(String, Option<String>, String)>,
            i64,
            i64,
        ) = self.db.with_conn(|conn| {
            let runs: i64 = match run_id {
                Some(id) => conn.query_row(
                    "SELECT count(*) FROM ralph_runs WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )?,
                None => conn.query_row("SELECT count(*) FROM ralph_runs", [], |r| r.get(0))?,
            };
            let iter_where = match run_id {
                Some(_) => "WHERE run_id = ?1".to_string(),
                None => "WHERE ladder_rung LIKE 'reuse:%'".to_string(),
            };
            let iterations: i64 = conn.query_row(
                &match run_id {
                    Some(_) => "SELECT count(*) FROM ralph_iterations WHERE run_id = ?1".to_string(),
                    None => "SELECT count(*) FROM ralph_iterations".to_string(),
                },
                params![run_id],
                |r| r.get(0),
            )?;
            let reuse_hits: i64 = if run_id.is_some() {
                conn.query_row(
                    "SELECT count(*) FROM ralph_iterations WHERE run_id = ?1 AND ladder_rung LIKE 'reuse:%'",
                    params![run_id],
                    |r| r.get(0),
                )?
            } else {
                conn.query_row(
                    "SELECT count(*) FROM ralph_iterations WHERE ladder_rung LIKE 'reuse:%'",
                    [],
                    |r| r.get(0),
                )?
            };
            let _ = iter_where;
            let findings: i64 = conn.query_row("SELECT count(*) FROM ralph_findings", [], |r| r.get(0))?;
            let mut stmt = conn.prepare(
                "SELECT content, meta, kind FROM ralph_findings WHERE kind = 'deferred' LIMIT 20",
            )?;
            let deferred = rows_to_vec(&mut stmt)?;
            let total_iters = iterations;
            Ok((runs, iterations, findings, deferred, reuse_hits, total_iters))
        })?;

        let reuse_rate = if total_iters > 0 {
            format!("{:.0}%", reuse_hits * 100 / total_iters.max(1))
        } else {
            "n/a".into()
        };
        let mut out = format!(
            "ralph_report: runs={runs} iterations={iterations} findings={findings} reuse-hit={reuse_rate}"
        );
        if !deferred.is_empty() {
            out.push_str("\ndebt-леджер (deferred):");
            for (content, meta, kind) in deferred {
                let no_trigger = meta.as_deref().map(|m| m.contains("no_trigger")).unwrap_or(false);
                let head: String = content.chars().take(120).collect();
                out.push_str(&format!(
                    "\n- [{kind}]{} {head}",
                    if no_trigger { " [no-trigger!]" } else { "" }
                ));
            }
        }
        Ok(out)
    }

    /// ast_diff: symbol-level дифф между итерациями (по ast_changes).
    pub fn ast_diff(&self, project_id: &str, from: &str, to: Option<&str>) -> anyhow::Result<String> {
        let rows: Vec<(String, String, String, String)> = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT path, label, node_type, change_type FROM ast_changes \
                 WHERE project_id = ?1 AND iteration_id IN (SELECT id FROM ralph_iterations \
                 WHERE created_at >= COALESCE((SELECT created_at FROM ralph_iterations WHERE id = ?2), ?2)) \
                 AND (?3 IS NULL OR created_at <= COALESCE((SELECT created_at FROM ralph_iterations WHERE id = ?3), ?3)) \
                 ORDER BY created_at LIMIT 200",
            )?;
            let rows = stmt
                .query_map(params![project_id, from, to], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })?
                .flatten()
                .collect();
            Ok(rows)
        })?;
        if rows.is_empty() {
            return Ok("изменений символов в диапазоне нет".to_string());
        }
        let mut out = format!("ast_diff {from}..{}:", to.unwrap_or("now"));
        for (path, label, ntype, change) in rows {
            out.push_str(&format!("\n- [{change}] {ntype}:{label} ({path})"));
        }
        Ok(out)
    }

    /// ast_history: хронология символа по итерациям.
    pub fn ast_history(&self, project_id: &str, symbol: &str) -> anyhow::Result<String> {
        let like = format!("%{symbol}%");
        let rows: Vec<(String, String, String, String)> = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT change_type, path, node_type, created_at FROM ast_changes \
                 WHERE project_id = ?1 AND label LIKE ?2 ORDER BY created_at DESC LIMIT 50",
            )?;
            let rows = stmt
                .query_map(params![project_id, like], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })?
                .flatten()
                .collect();
            Ok(rows)
        })?;
        if rows.is_empty() {
            return Ok(format!("история символа {symbol} пуста"));
        }
        let mut out = format!("ast_history {symbol}:");
        for (change, path, ntype, at) in rows {
            out.push_str(&format!("\n- [{at}] {change} {ntype}:{symbol} ({path})"));
        }
        Ok(out)
    }
}

fn rows_to_vec(
    stmt: &mut rusqlite::Statement,
) -> rusqlite::Result<Vec<(String, Option<String>, String)>> {
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, String>(2)?))
    })?;
    Ok(rows.flatten().collect())
}
