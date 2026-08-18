//! OneKE-пайплайн экстракции сущностей и отношений для графа знаний.

pub mod postprocess;
pub mod prompt;

use std::collections::HashMap;
use std::sync::Arc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::llm::{LLMClient, LLMClientExt};
pub use postprocess::postprocess;
pub use prompt::{EXTRACTION_SYSTEM_PROMPT, JUNK_LABELS, VALID_TYPES};

pub const CHUNK_MAX_CHARS: usize = 3000;
pub const CHUNK_OVERLAP: usize = 300;
pub const MIN_CHUNK_CHARS: usize = 80;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub label: String,
    #[serde(rename = "type", default = "default_type")]
    pub entity_type: String,
    #[serde(default)]
    pub description: String,
}

fn default_type() -> String {
    "Other".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    pub source: String,
    pub target: String,
    pub label: String,
    #[serde(default)]
    pub contexts: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtractionResult {
    pub entities: Vec<Entity>,
    pub relations: Vec<Relation>,
    pub chunks_processed: usize,
    pub chunks_skipped: usize,
}

#[derive(Deserialize)]
struct RawExtraction {
    #[serde(default)]
    entities: Vec<RawEntity>,
    #[serde(default)]
    relations: Vec<RawRelation>,
}

#[derive(Deserialize)]
struct RawEntity {
    id: Option<String>,
    label: Option<String>,
    #[serde(rename = "type")]
    entity_type: Option<String>,
    description: Option<String>,
}

#[derive(Deserialize)]
struct RawRelation {
    source: Option<String>,
    target: Option<String>,
    label: Option<String>,
    contexts: Option<Vec<String>>,
}

pub fn split_sentences(text: &str) -> Vec<String> {
    let re = Regex::new(r"(?:\.|\!|\?|…)\s+|\n+").unwrap();
    let mut result = Vec::new();
    let mut last = 0;
    for mat in re.find_iter(text) {
        let sent = text[last..mat.end()].trim();
        if !sent.is_empty() {
            result.push(sent.to_string());
        }
        last = mat.end();
    }
    if last < text.len() {
        let rest = text[last..].trim();
        if !rest.is_empty() {
            result.push(rest.to_string());
        }
    }
    result
}

pub fn split_into_chunks(
    text: &str,
    max_chars: usize,
    overlap: usize,
) -> Vec<String> {
    let sentences = split_sentences(text);
    if sentences.is_empty() {
        return Vec::new();
    }

    let mut prepared = Vec::new();
    for s in sentences {
        let mut rem = s.as_str();
        while rem.chars().count() > max_chars {
            let part: String = rem.chars().take(max_chars).collect();
            rem = &rem[part.len()..];
            prepared.push(part);
        }
        if !rem.is_empty() {
            prepared.push(rem.to_string());
        }
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for s in prepared {
        let candidate = if current.is_empty() {
            s.clone()
        } else {
            format!("{current} {s}")
        };

        if candidate.chars().count() > max_chars && !current.is_empty() {
            chunks.push(current.clone());
            let tail: String = current.chars().rev().take(overlap).collect::<Vec<_>>().into_iter().rev().collect();
            current = if overlap > 0 {
                format!("{tail} {s}")
            } else {
                s
            };
        } else {
            current = candidate;
        }
    }

    if !current.trim().is_empty() {
        chunks.push(current);
    }

    chunks
}

pub fn prefilter_chunk(chunk: &str) -> bool {
    let stripped = chunk.trim();
    if stripped.chars().count() < MIN_CHUNK_CHARS {
        return false;
    }
    let has_sentinel = stripped.contains('.') || stripped.contains('!') || stripped.contains('?') || stripped.contains('…');
    has_sentinel || stripped.chars().count() >= 300
}

pub struct Extractor {
    llm: Arc<dyn LLMClient>,
    max_chunks: usize,
}

impl Extractor {
    pub fn new(llm: Arc<dyn LLMClient>, max_chunks: usize) -> Self {
        Self { llm, max_chunks }
    }

    pub async fn extract(&self, text: &str) -> anyhow::Result<ExtractionResult> {
        let mut result = ExtractionResult::default();
        let chunks = split_into_chunks(text, CHUNK_MAX_CHARS, CHUNK_OVERLAP);

        for (i, chunk) in chunks.iter().enumerate() {
            if i >= self.max_chunks {
                warn!("Достигнут лимит чанков ({}), остаток пропущен", self.max_chunks);
                break;
            }

            if !prefilter_chunk(chunk) {
                result.chunks_skipped += 1;
                continue;
            }

            self.extract_chunk(chunk, &mut result).await;
            result.chunks_processed += 1;
        }

        Ok(postprocess(result))
    }

    async fn extract_chunk(&self, chunk: &str, result: &mut ExtractionResult) {
        let raw: RawExtraction = match self.llm.ask_json(chunk, Some(EXTRACTION_SYSTEM_PROMPT)).await {
            Ok(data) => data,
            Err(e) => {
                warn!("Chunk extraction failed: {e}");
                return;
            }
        };

        let mut by_id: HashMap<String, Entity> = HashMap::new();
        for raw_e in raw.entities {
            let label = raw_e.label.unwrap_or_default().trim().to_string();
            if label.is_empty() || label.chars().count() < 3 {
                continue;
            }
            let etype = raw_e.entity_type.unwrap_or_else(|| "Other".to_string());
            let desc: String = raw_e.description.unwrap_or_default().trim().chars().take(500).collect();

            let ent = Entity {
                label: label.clone(),
                entity_type: etype,
                description: desc,
            };

            let id = raw_e.id.unwrap_or_else(|| label.clone());
            by_id.insert(id, ent.clone());
            result.entities.push(ent);
        }

        for raw_r in raw.relations {
            let src_id = raw_r.source.unwrap_or_default();
            let tgt_id = raw_r.target.unwrap_or_default();
            let src = by_id.get(&src_id);
            let tgt = by_id.get(&tgt_id);

            if let (Some(s), Some(t)) = (src, tgt) {
                if s.label != t.label {
                    let rel_label = raw_r.label.unwrap_or_default().trim().to_lowercase().replace(' ', "_");
                    if !rel_label.is_empty() && rel_label.len() <= 64 {
                        let contexts = raw_r.contexts.unwrap_or_default()
                            .into_iter()
                            .map(|c| c.chars().take(300).collect())
                            .collect();

                        result.relations.push(Relation {
                            source: s.label.clone(),
                            target: t.label.clone(),
                            label: rel_label,
                            contexts,
                        });
                    }
                }
            }
        }
    }
}
