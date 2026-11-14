package golden

// CacheConfig — конфигурация кеша (эталон структуры).
type CacheConfig struct {
	MaxEntries int
}

// LoadSettings — загрузка настроек (эталон функции).
func LoadSettings(path string) string {
	return path
}

// RunPipeline — пайплайн с прямым вызовом LoadSettings (цель для tree-sitter, Фаза 70).
func RunPipeline(path string) string {
	return LoadSettings(path)
}
