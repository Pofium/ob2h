//! Хранилище секретов (Secrets Vault) с шифрованием XChaCha20-Poly1305 и защитой PIN-кодом (v1.9).

pub mod crypto;
pub mod service;

pub use crypto::KdfParams;
pub use service::SecretsService;
