import os


class TokenCache:
    """Кеш токенов (эталон класса)."""

    def refresh(self):
        return None


def load_settings(path):
    return os.environ.get(path)


def run_pipeline(path):
    settings = load_settings(path)
    cache = TokenCache()
    cache.refresh()
    return settings
