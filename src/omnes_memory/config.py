"""Конфигурация OmnesMemory (env с префиксом OMNES_, см. docs/ARCHITECTURE.md §6)."""

from __future__ import annotations

import functools
from pathlib import Path

from pydantic_settings import BaseSettings, SettingsConfigDict

# C:\...\omnesbot_for_hermes\src\omnes_memory\config.py -> корень проекта
_PROJECT_ROOT = Path(__file__).resolve().parents[2]


class Settings(BaseSettings):
    """Все настройки. Ключи и секреты — только из env."""

    model_config = SettingsConfigDict(
        env_prefix="OMNES_",
        env_file=".env",
        env_file_encoding="utf-8",
        extra="ignore",
    )

    # --- Хранилища ---
    data_dir: Path = _PROJECT_ROOT / "data"

    # --- LLM (dream / extract / reason / consolidate) ---
    llm_base_url: str = "https://api.deepseek.com/v1"
    llm_api_key: str = ""
    llm_model: str = "deepseek-v4-flash"
    llm_timeout: float = 120.0
    llm_max_retries: int = 3

    # --- Эмбеддинги ---
    embed_provider: str = "local"  # local | api
    # Дефолт — единственный мультиязычный (русский) в-process вариант fastembed.
    # Альтернатива без скачивания: api + LM Studio (embeddinggemma-300m-qat),
    # см. docs/ARCHITECTURE.md §6.
    embed_model: str = "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2"
    embed_base_url: str = ""  # для api-провайдера, напр. http://localhost:1234/v1
    embed_api_key: str = ""

    # --- Консолидация / контекст ---
    context_window: int = 65536
    max_completion_tokens: int = 8192

    # --- Дриминг ---
    autodream_enabled: bool = True
    autodream_interval_min: int = 5  # период проверки гейтов
    autodream_min_interval_h: int = 4  # не чаще раза в N часов
    autodream_min_events: int = 10  # минимум новых событий daily-лога
    dream_batch: int = 20
    # Извлечение сущностей из новых записей истории в граф во время дрима
    # (сессии попадают в общий граф знаний вместе с документами)
    dream_extract_enabled: bool = True

    # --- Ретеншн ---
    retention_days: int = 30

    # --- Служебное ---
    log_level: str = "INFO"
    max_tool_output_chars: int = 20000

    @property
    def db_path(self) -> Path:
        return self.data_dir / "omnes.db"

    @property
    def workspace_dir(self) -> Path:
        return self.data_dir / "workspace"

    @property
    def backups_dir(self) -> Path:
        return self.data_dir / "backups"

    @property
    def logs_dir(self) -> Path:
        return _PROJECT_ROOT / "logs"

    def ensure_dirs(self) -> None:
        for p in (self.data_dir, self.workspace_dir, self.workspace_dir / "memory",
                  self.workspace_dir / "daily", self.backups_dir, self.logs_dir):
            p.mkdir(parents=True, exist_ok=True)


@functools.lru_cache(maxsize=1)
def get_settings() -> Settings:
    return Settings()
