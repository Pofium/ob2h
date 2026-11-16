//! Сервис долгосрочной памяти: CRUD, гибридный поиск (FTS5 + Vector, RRF k=60), decay, purge.

pub mod service;
pub use service::{
    format_warming_suffix, warmup_eta_from, ContextOptions, MemoryHit, MemoryService, WarmupEta,
};
