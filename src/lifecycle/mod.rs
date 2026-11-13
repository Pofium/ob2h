//! Жизненный цикл памяти (v1.9.9, Фаза 64): уроки (lessons) и аудит-леджер (audit_log).
//! Схема — миграция M11 (`src/db/schema.rs`).

pub mod audit;
pub mod lessons;

pub use lessons::{LessonRecord, LessonSaveOutcome, LessonSaveRequest, LessonsService};
