"""Тесты инжеста документов (фаза 4.1)."""

import pytest

from omnes_memory.ingest import read_document, read_text_file


def test_read_txt_utf8(tmp_path):
    p = tmp_path / "doc.txt"
    p.write_text("привет мир", encoding="utf-8")
    assert read_document(p)[0] == "привет мир"


def test_read_txt_cp1251_fallback(tmp_path):
    p = tmp_path / "old.txt"
    p.write_bytes("привет из 2007".encode("cp1251"))
    text, meta = read_document(p)
    assert text == "привет из 2007"
    assert meta["format"] == ".txt"


def test_read_md(tmp_path):
    p = tmp_path / "note.md"
    p.write_text("# Заголовок\nтекст", encoding="utf-8")
    text, _ = read_document(p)
    assert "Заголовок" in text


def test_missing_file(tmp_path):
    with pytest.raises(FileNotFoundError):
        read_document(tmp_path / "нет.txt")


def test_unsupported_format(tmp_path):
    p = tmp_path / "file.exe"
    p.write_bytes(b"MZ")
    with pytest.raises(ValueError, match="не поддерживается"):
        read_document(p)


def test_broken_utf8_replaced(tmp_path):
    p = tmp_path / "bin.txt"
    p.write_bytes(b"\xff\xfe\x00broken")
    # не падает, заменяет биты
    assert isinstance(read_text_file(p), str)
