"""Тесты экстрактора: чанкинг, префильтр, пост-обработка (фаза 4.2, 4.3)."""

from fakes import FakeLLM
from omnes_memory.extractor import (
    JUNK_LABELS,
    Entity,
    ExtractionResult,
    Extractor,
    prefilter_chunk,
    split_into_chunks,
)

# --- чанкинг ---


def test_split_respects_max_chars_and_sentences():
    sentences = [f"Предложение номер {i} про котлы и цеха." for i in range(100)]
    text = " ".join(sentences)
    chunks = split_into_chunks(text, max_chars=500, overlap=50)
    assert all(len(c) <= 560 for c in chunks)  # 500 + перекрытие
    assert len(chunks) > 1
    # чанки начинаются с начала предложения (после хвоста перекрытия)
    assert "Предложение" in chunks[1]


def test_split_short_text_single_chunk():
    assert split_into_chunks("Один короткий текст.") == ["Один короткий текст."]


def test_split_hard_cuts_giant_sentence():
    giant = "слово " * 2000  # одно «предложение» без терминатора... добавим точку в конец
    chunks = split_into_chunks(giant + ".", max_chars=300, overlap=30)
    assert all(len(c) <= 340 for c in chunks)
    assert len(chunks) > 5


def test_prefilter_short_and_header_chunks():
    assert prefilter_chunk("коротко") is False
    header = "\n".join(f"Глава {i}" for i in range(10))  # без точек, < 300
    assert prefilter_chunk(header) is False
    valid = "Нормальный текст достаточной длины с точкой. " * 3  # > 80 символов
    assert prefilter_chunk(valid) is True
    long_header = "\n".join(f"Глава номер {i} содержание" for i in range(30))
    # длинное без терминаторов — всё равно контент (правило порта: режем только
    # короткие заголовочные оглавления)
    assert prefilter_chunk(long_header) is True


# --- пайплайн с FakeLLM ---


EXTRACTION_JSON = (
    '{"entities": [{"id": "e1", "label": "Иванов", "type": "Person",'
    ' "description": "Инженер, работает в ООО Ромашка"},'
    '{"id": "e2", "label": "ООО Ромашка", "type": "Organization",'
    ' "description": "Производственная компания"},'
    '{"id": "e3", "label": "который", "type": "Other", "description": "мусор"}],'
    '"relations": [{"source": "e1", "target": "e2", "label": "works_at",'
    ' "contexts": ["работает в ООО Ромашка"]},'
    '{"source": "e1", "target": "e999", "label": "bad", "contexts": []}]}'
)


def test_extract_full_pipeline():
    llm = FakeLLM(responses=[EXTRACTION_JSON])
    extractor = Extractor(llm)
    text = "Иванов — инженер. Он работает в ООО Ромашка. " * 10  # >80 символов
    result = extractor.extract(text)

    labels = {e.label for e in result.entities}
    assert labels == {"Иванов", "ООО Ромашка"}          # мусор отфильтрован
    rels = {(r.source, r.target, r.label) for r in result.relations}
    assert ("Иванов", "ООО Ромашка", "works_at") in rels
    # невалидное отношение на e999 отброшено
    assert all(r.target != "e999" for r in result.relations)


def test_infer_relations_from_description():
    llm = FakeLLM(responses=[
        '{"entities": [{"id": "e1", "label": "Пётр", "type": "Person",'
        ' "description": "отец Ивана, работает в школе"},'
        '{"id": "e2", "label": "Иван", "type": "Person", "description": "сын"}],'
        '"relations": []}',
    ])
    result = Extractor(llm).extract("Пётр — отец Ивана и работает в школе." + " текст " * 30)
    rels = {(r.source, r.target, r.label) for r in result.relations}
    assert ("Пётр", "Иван", "father_of") in rels


def test_dedup_merges_same_entity():
    extractor = Extractor(FakeLLM())
    r = ExtractionResult(
        entities=[
            Entity("Иванов", "Person", "инженер"),
            Entity("Иванов", "Person", "инженер"),  # дубль внутри результата
        ],
        relations=[],
    )
    extractor.postprocess(r)
    assert len(r.entities) == 1


def test_incremental_callback_fires():
    llm = FakeLLM(responses=[EXTRACTION_JSON] * 100)
    calls = []
    Extractor(llm).extract(
        "Иванов работает в ООО Ромашка. " * 200,
        on_batch=lambda r: calls.append(r.chunks_processed),
        batch_size=1,
    )
    assert calls and calls[0] == 1


def test_junk_list_no_valid_entities():
    assert "который" in JUNK_LABELS
    assert "Ромашка" not in JUNK_LABELS
