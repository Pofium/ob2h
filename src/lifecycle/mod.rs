//! Жизненный цикл памяти (v1.9.9, Фазы 64–65): уроки (lessons), слоты (slots)
//! и аудит-леджер (audit_log). Схема — миграция M11 (`src/db/schema.rs`).

pub mod audit;
pub mod lessons;
pub mod slots;
pub mod timeline;

pub use lessons::{LessonRecord, LessonSaveOutcome, LessonSaveRequest, LessonsService};
pub use slots::{SlotRecord, SlotsService};
