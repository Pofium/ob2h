//! Фоновые AST-сканы по инициативе MCP-агента.
//! project_scan уходит в tokio-задачу и не упирается в таймаут MCP-клиента:
//! агент запускает скан и опрашивает результат через project_scan_status,
//! не прибегая к CLI/логам/чтению БД напрямую.

use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{info, warn};

use crate::db::utcnow;
use crate::project::ProjectService;

/// Итог завершённого скана (числа как в AstScanResult + векторизованные узлы).
#[derive(Debug, Clone, Serialize)]
pub struct ScanJobResult {
    pub files_scanned: usize,
    pub nodes: usize,
    pub edges: usize,
    pub lines_total: usize,
    pub embedded: usize,
}

#[derive(Debug, Clone, Serialize)]
pub enum ScanJobStatus {
    Running,
    Done(ScanJobResult),
    Failed(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanJob {
    pub project_id: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: ScanJobStatus,
}

/// Верхняя граница итераций векторизации после скана: embed_unembedded_nodes
/// берёт ≤500 узлов за вызов; 200 итераций покрывают 100k узлов и гарантируют
/// завершение фонового задания.
const EMBED_ROUNDS_MAX: usize = 200;

pub struct ScanJobManager {
    project: Arc<ProjectService>,
    conn: Arc<Mutex<Connection>>,
    /// project_id → последняя/текущая работа процесса.
    jobs: Mutex<HashMap<String, ScanJob>>,
}

impl ScanJobManager {
    pub fn new(project: Arc<ProjectService>, conn: Arc<Mutex<Connection>>) -> Self {
        Self {
            project,
            conn,
            jobs: Mutex::new(HashMap::new()),
        }
    }

    /// Запустить фоновый скан проекта. Если скан этого проекта уже идёт —
    /// возвращается его текущая работа без повторного запуска (идемпотентно).
    /// Ошибки валидации (проект/путь) возвращаются синхронно.
    pub fn start(
        self: &Arc<Self>,
        project_id: &str,
        custom_path: Option<&str>,
        incremental: bool,
    ) -> anyhow::Result<ScanJob> {
        // Валидация до фона: несуществующий проект/путь — немедленный [Error]
        let project = self
            .project
            .get_project(project_id)?
            .ok_or_else(|| anyhow::anyhow!("Проект с ID '{}' не найден", project_id))?;
        let scan_path = custom_path.unwrap_or(&project.root_path);
        if !std::path::Path::new(scan_path).exists() {
            return Err(anyhow::anyhow!(
                "Путь к проекту не существует: {}",
                scan_path
            ));
        }

        {
            let mut jobs = self.jobs.lock().unwrap();
            if let Some(job) = jobs.get(project_id) {
                if matches!(job.status, ScanJobStatus::Running) {
                    return Ok(job.clone());
                }
            }
            let job = ScanJob {
                project_id: project_id.to_string(),
                started_at: utcnow(),
                finished_at: None,
                status: ScanJobStatus::Running,
            };
            jobs.insert(project_id.to_string(), job.clone());

            let mgr = Arc::clone(self);
            let pid = project_id.to_string();
            let path = custom_path.map(|s| s.to_string());
            tokio::spawn(async move {
                mgr.run_job(&pid, path.as_deref(), incremental).await;
            });

            Ok(job)
        }
    }

    /// Тело фоновой работы: AST-скан → пересчёт God Nodes → векторизация.
    async fn run_job(&self, project_id: &str, custom_path: Option<&str>, incremental: bool) {
        let pid = project_id.to_string();

        let scan_res = {
            let svc = Arc::clone(&self.project);
            let p = pid.clone();
            let path = custom_path.map(|s| s.to_string());
            tokio::task::spawn_blocking(move || svc.scan_project(&p, path.as_deref(), incremental))
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("panic в AST-скане: {e}")))
        };

        let res = match scan_res {
            Ok(r) => r,
            Err(e) => {
                warn!("Фоновый AST-скан '{}' не удался: {e}", pid);
                self.finish(&pid, ScanJobStatus::Failed(e.to_string()));
                return;
            }
        };

        // Пересчёт God Nodes тем же соединением (ошибки не критичны)
        {
            let conn = Arc::clone(&self.conn);
            let p = pid.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let conn = conn.lock().unwrap();
                crate::graph::GraphAnalytics::update_god_nodes(&conn, &p)
            })
            .await;
        }

        // Векторизация циклами по 500 узлов, пока не иссякнет хвост
        let mut embedded = 0usize;
        for _ in 0..EMBED_ROUNDS_MAX {
            match self.project.embed_unembedded_nodes(&pid).await {
                Ok(0) => break,
                Ok(n) => embedded += n,
                Err(e) => {
                    warn!("Векторизация узлов '{}' остановилась: {e}", pid);
                    break;
                }
            }
        }

        info!(
            "Фоновый AST-скан '{}' завершён: файлов {}, узлов {}, связей {}, строк {}, векторизовано {}",
            pid,
            res.files_scanned,
            res.nodes.len(),
            res.edges.len(),
            res.lines_total,
            embedded
        );

        self.finish(
            &pid,
            ScanJobStatus::Done(ScanJobResult {
                files_scanned: res.files_scanned,
                nodes: res.nodes.len(),
                edges: res.edges.len(),
                lines_total: res.lines_total,
                embedded,
            }),
        );
    }

    fn finish(&self, project_id: &str, status: ScanJobStatus) {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(job) = jobs.get_mut(project_id) {
            job.finished_at = Some(utcnow());
            job.status = status;
        }
    }

    /// Подождать завершения текущей работы проекта до `timeout`.
    /// Вернуть её состояние (Running — если время вышло); None — работ не было.
    pub async fn wait_for(&self, project_id: &str, timeout: Duration) -> Option<ScanJob> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let state = self.job(project_id);
            let running = matches!(&state, Some(j) if matches!(j.status, ScanJobStatus::Running));
            if !running || tokio::time::Instant::now() >= deadline {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Последняя/текущая работа проекта этого процесса.
    pub fn job(&self, project_id: &str) -> Option<ScanJob> {
        self.jobs.lock().unwrap().get(project_id).cloned()
    }
}
