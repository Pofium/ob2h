//! Консолидатор сессий по бюджету токенов (порт Consolidator из OmnesBOT).

use std::sync::Arc;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::config::Settings;
use crate::llm::LLMClient;
use crate::workspace::Workspace;

pub const MAX_MESSAGES_PER_ROUND: usize = 60;
pub const MAX_ROUNDS: usize = 5;

pub const SYSTEM_PROMPT: &str = "\
Ты — архивариус памяти личного агента. Сожми приведённый фрагмент диалога \
в компактный итог для долгосрочной памяти: только факты, решения, пожелания \
пользователя и результаты. Формат — маркированный список на русском, \
каждый пункт самодостаточен. Без вступлений и заголовков.";

pub fn estimate_tokens(text: &str) -> usize {
    (text.chars().count() / 3).max(1)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Default)]
pub struct PendingSession {
    pub messages: Vec<ChatTurn>,
    pub total_estimated: usize,
}

impl PendingSession {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn append(&mut self, role: &str, content: &str) {
        self.messages.push(ChatTurn {
            role: role.to_string(),
            content: content.to_string(),
        });
        self.total_estimated += estimate_tokens(content);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidateResult {
    pub consolidated: bool,
    pub entries: usize,
    pub remaining_estimated: usize,
}

pub struct Consolidator {
    workspace: Arc<Workspace>,
    llm: Arc<dyn LLMClient>,
    settings: Settings,
}

impl Consolidator {
    pub fn new(workspace: Arc<Workspace>, llm: Arc<dyn LLMClient>, settings: Settings) -> Self {
        Self {
            workspace,
            llm,
            settings,
        }
    }

    pub fn budget(&self) -> usize {
        (self.settings.context_window
            .saturating_sub(self.settings.max_completion_tokens)
            .saturating_sub(1024))
            / 2
    }

    pub async fn maybe_consolidate(&self, session: &mut PendingSession) -> anyhow::Result<ConsolidateResult> {
        let mut rounds = 0;
        let mut consolidated_entries = 0;

        while session.total_estimated > self.budget() && rounds < MAX_ROUNDS {
            let batch = self.take_batch(session);
            if batch.is_empty() {
                break;
            }
            let summary = self.summarize(&batch).await;
            let _ = self.workspace.append_history(&summary)?;
            consolidated_entries += 1;
            rounds += 1;
        }

        let _ = self.workspace.compact_history(1000);

        Ok(ConsolidateResult {
            consolidated: consolidated_entries > 0,
            entries: consolidated_entries,
            remaining_estimated: session.total_estimated,
        })
    }

    fn take_batch(&self, session: &mut PendingSession) -> Vec<ChatTurn> {
        let limit = MAX_MESSAGES_PER_ROUND.min(session.messages.len());
        let mut batch: Vec<ChatTurn> = session.messages[..limit].to_vec();

        // Не рвать пару user->assistant
        while !batch.is_empty()
            && batch.last().map(|m| m.role.as_str()) == Some("user")
            && batch.len() < session.messages.len()
        {
            batch.pop();
        }

        if batch.is_empty() {
            batch = session.messages[..limit].to_vec();
        }

        let consumed = batch.len();
        session.messages = session.messages[consumed..].to_vec();
        session.total_estimated = session
            .messages
            .iter()
            .map(|m| estimate_tokens(&m.content))
            .sum();

        batch
    }

    async fn summarize(&self, batch: &[ChatTurn]) -> String {
        let dialogue = batch
            .iter()
            .map(|m| {
                let speaker = if m.role == "user" {
                    "Пользователь"
                } else {
                    "Агент"
                };
                format!("{speaker}: {}", m.content)
            })
            .collect::<Vec<_>>()
            .join("\n");

        match self.llm.ask(&dialogue, Some(SYSTEM_PROMPT)).await {
            Ok(ans) if !ans.trim().is_empty() => ans.trim().to_string(),
            Ok(_) => Self::raw_archive(batch),
            Err(e) => {
                warn!("LLM summarization failed: {e}; using raw archive fallback");
                Self::raw_archive(batch)
            }
        }
    }

    fn raw_archive(batch: &[ChatTurn]) -> String {
        batch
            .iter()
            .map(|m| {
                let preview: String = m.content.chars().take(500).collect();
                format!("[RAW] {}: {}", m.role, preview)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
