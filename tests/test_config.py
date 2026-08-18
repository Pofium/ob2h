"""Тесты конфигурации (фаза 1.1)."""

from omnes_memory.config import Settings, get_settings


def test_defaults():
    s = Settings(_env_file=None)
    assert s.llm_model == "deepseek-v4-flash"
    assert s.embed_provider == "local"
    assert s.context_window == 65536
    assert s.autodream_min_interval_h == 4
    assert s.autodream_min_events == 10
    assert s.db_path.name == "omnes.db"
    assert s.workspace_dir.name == "workspace"


def test_env_prefix_override(monkeypatch, tmp_path):
    monkeypatch.setenv("OMNES_DATA_DIR", str(tmp_path))
    monkeypatch.setenv("OMNES_LLM_MODEL", "test-model")
    monkeypatch.setenv("OMNES_AUTODREAM_ENABLED", "false")
    s = Settings(_env_file=None)
    assert s.data_dir == tmp_path
    assert s.llm_model == "test-model"
    assert s.autodream_enabled is False


def test_ensure_dirs(tmp_path):
    s = Settings(_env_file=None, data_dir=tmp_path / "dd")
    s.ensure_dirs()
    assert (tmp_path / "dd" / "workspace" / "memory").is_dir()
    assert (tmp_path / "dd" / "workspace" / "daily").is_dir()
    assert (tmp_path / "dd" / "backups").is_dir()


def test_get_settings_cached(monkeypatch):
    monkeypatch.setenv("OMNES_LOG_LEVEL", "DEBUG")
    assert get_settings() is get_settings()
    assert get_settings().log_level == "DEBUG"
