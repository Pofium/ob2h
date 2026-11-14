use std::collections::HashMap;

/// Конфигурация кеша токенов.
pub struct CacheConfig {
    pub max_entries: usize,
}

/// Хранилище токенов (эталон трейта).
pub trait TokenStore {
    fn fetch(&self, key: &str) -> Option<String>;
}

/// Сборка кеша по конфигурации.
pub fn build_cache(config: &CacheConfig) -> HashMap<String, String> {
    let mut map = HashMap::new();
    map.insert(String::from("k"), String::from("v"));
    map
}

/// Пайплайн: конфиг → кеш → выборка токена.
pub fn run_pipeline(store: &dyn TokenStore) -> usize {
    let config = CacheConfig { max_entries: 16 };
    let cache = build_cache(&config);
    let _ = store.fetch("token");
    cache.len()
}
