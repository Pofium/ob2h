//! Сервис долгосрочной памяти: CRUD, гибридный поиск (FTS5 + Vector, RRF k=60), decay, purge.

pub mod service;
pub use service::{MemoryHit, MemoryService};
