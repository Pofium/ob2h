//! Хранилище секретов (Secrets Vault) с шифрованием XChaCha20-Poly1305 и защитой PIN-кодом (v1.9).

pub mod crypto;
pub mod ipc;
pub mod pin_input;
pub mod service;

/// Локальное окно ввода PIN (egui/eframe) — только при feature `gui`.
#[cfg(feature = "gui")]
pub mod pin_gui;

pub use crypto::KdfParams;
pub use ipc::{IpcClient, IpcServer};
pub use service::SecretsService;
