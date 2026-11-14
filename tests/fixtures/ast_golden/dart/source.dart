/// Токен-кеш (эталон класса).
class TokenCache {
  void refresh() {}
}

/// Загрузка настроек (эталон функции).
String loadSettings(String path) {
  return path;
}

/// Пайплайн с прямым вызовом loadSettings — цель для tree-sitter (Фаза 70.12).
String runPipeline(String path) {
  loadSettings(path);
  return path;
}
