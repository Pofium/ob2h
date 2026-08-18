//! Постобработка извлечённых сущностей: фильтрация стоп-слов, инференс связей и дедупликация.

use std::collections::{HashMap, HashSet};
use super::prompt::{INFERENCE_PATTERNS, JUNK_LABELS};
use super::{Entity, ExtractionResult, Relation};

pub fn postprocess(mut result: ExtractionResult) -> ExtractionResult {
    filter_junk(&mut result);
    infer_relations(&mut result);
    dedup(&mut result);
    result
}

fn filter_junk(result: &mut ExtractionResult) {
    let junk_set: HashSet<&str> = JUNK_LABELS.iter().copied().collect();
    result.entities.retain(|e| {
        let label_lower = e.label.to_lowercase();
        !junk_set.contains(label_lower.as_str()) && e.label.chars().count() >= 3
    });

    let valid_labels: HashSet<String> = result.entities.iter().map(|e| e.label.clone()).collect();
    result.relations.retain(|r| {
        valid_labels.contains(&r.source) && valid_labels.contains(&r.target) && r.source != r.target
    });
}

fn infer_relations(result: &mut ExtractionResult) {
    let mut existing: HashSet<(String, String, String)> = result
        .relations
        .iter()
        .map(|r| (r.source.clone(), r.target.clone(), r.label.clone()))
        .collect();

    let labels: Vec<String> = result.entities.iter().map(|e| e.label.clone()).collect();
    let mut new_relations = Vec::new();

    for entity in &result.entities {
        let desc = entity.description.to_lowercase();
        if desc.is_empty() {
            continue;
        }

        for other in &labels {
            if other == &entity.label || !desc.contains(&other.to_lowercase()) {
                continue;
            }

            for (pattern, relation) in INFERENCE_PATTERNS {
                if desc.contains(pattern) {
                    let key = (entity.label.clone(), other.clone(), relation.to_string());
                    if !existing.contains(&key) {
                        existing.insert(key);
                        let ctx_snippet: String = entity.description.chars().take(200).collect();
                        new_relations.push(Relation {
                            source: entity.label.clone(),
                            target: other.clone(),
                            label: relation.to_string(),
                            contexts: vec![format!("инференс: {ctx_snippet}")],
                        });
                    }
                    break;
                }
            }
        }
    }

    result.relations.extend(new_relations);
}

fn dedup(result: &mut ExtractionResult) {
    let mut merged_entities: HashMap<(String, String), Entity> = HashMap::new();
    for e in result.entities.drain(..) {
        let key = (e.label.clone(), e.entity_type.clone());
        merged_entities
            .entry(key)
            .and_modify(|base| {
                if !e.description.is_empty() && !base.description.contains(&e.description) {
                    base.description = format!("{} {}", base.description, e.description)
                        .trim()
                        .chars()
                        .take(1000)
                        .collect();
                }
            })
            .or_insert(e);
    }
    result.entities = merged_entities.into_values().collect();

    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    let mut clean_relations = Vec::new();
    for r in result.relations.drain(..) {
        let key = (r.source.clone(), r.target.clone(), r.label.clone());
        if !seen.contains(&key) {
            seen.insert(key);
            clean_relations.push(r);
        }
    }
    result.relations = clean_relations;
}
