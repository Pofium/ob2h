//! Хранилище секретов (Secrets Vault) с шифрованием XChaCha20-Poly1305 и защитой PIN-кодом (v1.9).

pub mod crypto;
pub mod ipc;
pub mod service;

pub use crypto::KdfParams;
pub use ipc::{IpcClient, IpcServer};
pub use service::SecretsService;
