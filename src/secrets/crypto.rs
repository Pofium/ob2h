//! Криптографические примитивы хранилища секретов (Argon2id + XChaCha20-Poly1305 + zeroize).

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use zeroize::Zeroizing;

pub const VERIFIER_AAD: &str = "__vault_verifier__";
pub const VERIFIER_MAGIC: &[u8] = b"ob2h-vault-v1-ok";

/// Параметры KDF Argon2id
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct KdfParams {
    /// Объем памяти в КиБ (по умолчанию 65536 КиБ = 64 МиБ)
    pub m_cost: u32,
    /// Количество итераций времени (по умолчанию 3)
    pub t_cost: u32,
    /// Степень параллелизма (по умолчанию 1)
    pub p_cost: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        Self {
            m_cost: 65536,
            t_cost: 3,
            p_cost: 1,
        }
    }
}

/// Вычисление 32-байтного мастер-ключа K из PIN-кода и соли с использованием Argon2id.
pub fn derive_key(pin: &str, salt: &[u8], params: &KdfParams) -> anyhow::Result<Zeroizing<[u8; 32]>> {
    let argon_params = Params::new(params.m_cost, params.t_cost, params.p_cost, Some(32))
        .map_err(|e| anyhow::anyhow!("Недопустимые параметры Argon2: {e}"))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon_params);
    let mut key = Zeroizing::new([0u8; 32]);
    argon2
        .hash_password_into(pin.as_bytes(), salt, key.as_mut())
        .map_err(|e| anyhow::anyhow!("Ошибка вычисления Argon2: {e}"))?;
    Ok(key)
}

/// Генерация криптографически стойкой 16-байтной соли.
pub fn generate_salt() -> anyhow::Result<[u8; 16]> {
    let mut salt = [0u8; 16];
    getrandom::getrandom(&mut salt)
        .map_err(|e| anyhow::anyhow!("Ошибка getrandom при генерации соли: {e}"))?;
    Ok(salt)
}

/// Шифрование значения секретной записи алгоритмом XChaCha20-Poly1305.
/// Nonce (24 байта) генерируется случайно и помещается в начало результата: `nonce || ciphertext || tag`.
/// `name` привязывается в качестве AAD (Additional Authenticated Data).
pub fn encrypt_value(key: &[u8; 32], name: &str, plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let mut nonce = [0u8; 24];
    getrandom::getrandom(&mut nonce)
        .map_err(|e| anyhow::anyhow!("Ошибка getrandom при генерации nonce: {e}"))?;
    let xnonce = XNonce::from_slice(&nonce);
    let payload = Payload {
        msg: plaintext,
        aad: name.as_bytes(),
    };
    let ct = cipher
        .encrypt(xnonce, payload)
        .map_err(|e| anyhow::anyhow!("Ошибка шифрования AEAD: {e}"))?;

    let mut result = Vec::with_capacity(24 + ct.len());
    result.extend_from_slice(&nonce);
    result.extend_from_slice(&ct);
    Ok(result)
}

/// Расшифровка значения секретной записи алгоритмом XChaCha20-Poly1305.
/// Извлекает 24-байтный nonce из начала и проверяет целостность с AAD `name`.
pub fn decrypt_value(
    key: &[u8; 32],
    name: &str,
    blob: &[u8],
) -> anyhow::Result<Zeroizing<Vec<u8>>> {
    if blob.len() < 24 + 16 {
        anyhow::bail!("Поврежденный ciphertext: длина {} < 40", blob.len());
    }
    let (nonce_bytes, ct_bytes) = blob.split_at(24);
    let cipher = XChaCha20Poly1305::new(key.into());
    let xnonce = XNonce::from_slice(nonce_bytes);
    let payload = Payload {
        msg: ct_bytes,
        aad: name.as_bytes(),
    };
    let pt = cipher
        .decrypt(xnonce, payload)
        .map_err(|_| anyhow::anyhow!("расшифровка не удалась (целостность или неверный ключ)"))?;
    Ok(Zeroizing::new(pt))
}

/// Создание верификатора для проверки правильности PIN-кода.
pub fn create_verifier(key: &[u8; 32]) -> anyhow::Result<Vec<u8>> {
    encrypt_value(key, VERIFIER_AAD, VERIFIER_MAGIC)
}

/// Проверка правильности ключа K по сохраненному верификатору.
pub fn check_verifier(key: &[u8; 32], verifier_blob: &[u8]) -> bool {
    match decrypt_value(key, VERIFIER_AAD, verifier_blob) {
        Ok(pt) => pt.as_slice() == VERIFIER_MAGIC,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crypto_roundtrip_and_aad() {
        let salt = generate_salt().expect("salt");
        let params = KdfParams {
            m_cost: 1024, // Для быстрого юнит-теста
            t_cost: 1,
            p_cost: 1,
        };
        let key = derive_key("secret_pin_123", &salt, &params).expect("derive key");

        let plaintext = b"super_secret_api_key_value_999";
        let encrypted = encrypt_value(&key, "openai_api_key", plaintext).expect("encrypt");
        assert!(encrypted.len() >= 24 + 16 + plaintext.len());

        // 1. Успешная расшифровка с правильным именем
        let decrypted = decrypt_value(&key, "openai_api_key", &encrypted).expect("decrypt");
        assert_eq!(decrypted.as_slice(), plaintext);

        // 2. Ошибка при неверном AAD (подмена имени секрета)
        let aad_err = decrypt_value(&key, "different_key_name", &encrypted);
        assert!(aad_err.is_err());

        // 3. Ошибка при неверном ключе K
        let wrong_key = derive_key("wrong_pin", &salt, &params).expect("wrong key");
        let wrong_key_err = decrypt_value(&wrong_key, "openai_api_key", &encrypted);
        assert!(wrong_key_err.is_err());

        // 4. Ошибка при повреждении даже 1 байта ciphertext
        let mut corrupted = encrypted.clone();
        corrupted[30] ^= 0x55;
        let corrupt_err = decrypt_value(&key, "openai_api_key", &corrupted);
        assert!(corrupt_err.is_err());
    }

    #[test]
    fn test_verifier_roundtrip() {
        let salt = generate_salt().expect("salt");
        let params = KdfParams {
            m_cost: 1024,
            t_cost: 1,
            p_cost: 1,
        };
        let key = derive_key("correct_pin", &salt, &params).expect("key");
        let verifier = create_verifier(&key).expect("verifier");

        assert!(check_verifier(&key, &verifier));

        let wrong_key = derive_key("bad_pin", &salt, &params).expect("wrong key");
        assert!(!check_verifier(&wrong_key, &verifier));
    }
}
