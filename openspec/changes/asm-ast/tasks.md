# Tasks: Asm AST (`asm-ast`)

Нумерация продолжает PLAN_v2.0 (Фаза 78 — regex-волна, Фаза 78b —
tree-sitter-волна). Паттерны: Ф48–Ф52 (regex-волны форматов), Ф70.3–70.6
(ts-мапперы), Ф69.3 (golden).

## Фаза 78 — Regex-волна: `asm-gas` + `asm-intel`

- [x] T-78.1 Сниффинг диалекта `asm_dialect(content, default_by_ext)` →
  `GasAtt | GasIntel | Nasm | Masm` (признаки design §1) + юнит-тесты на
  4 мини-файлах.
- [x] T-78.2 Записи `asm-gas` (`Ext(&["s"])`, покрывает `.S` нижнением
  регистра) и `asm-intel` (`Ext(&["asm"])`) в `LANGS` с фикстурами;
  `Parser::Asm` в enum; словари node_types/edge_labels по design §2–3;
  `max_bytes = DEFAULT_CAP_BYTES`.
- [x] T-78.3 Экстрактор `Parser::Asm`: секционное состояние; метки →
  `fn:{rel}:{name}` (код) / `data:{rel}:{name}` (данные, `.equ`/`EQU`/
  `%define`); MASM `PROC/ENDP` (line_end из `ENDP`); макросы трёх диалектов →
  `macro:{rel}:{name}`; entry-метки (`.globl`/`global`/`PUBLIC`) — пометка
  «(entry)» в description.
- [x] T-78.4 `IMPORTS` + `ImportRef` из `#include`/`%include`/`INCLUDE`
  (нормализация пути без кавычек); препроцессорные `#`-строки, кроме
  `#include`, пропускаются.
- [x] T-78.5 call_sites: `call`/`callq`/`bl`/`jal` (+`jalr`) — операнд хвостом
  строки, классификация `asm_call_target` (`ASM_REG_SKIP`, косвенные
  `*`/`[`/`%`, `@PLT`/`@GOT`-суффиксы) + `ASM_LABEL_SKIP`; дедуп sort+dedup;
  резолв `CALLS` через существующий type-pass (same-file; кросс-языковой мост
  C → asm — T-78.9).
- [x] T-78.6 Skip-листы `ASM_LABEL_SKIP` (`.L*`, числовые метки (классом
  символов), `*.cold`, `__x86.get_pc_thunk.*`, NASM `.loop`, MASM `@@` —
  префиксы `.`/`@`) + `ASM_LABEL_SKIP_SUFFIXES` + тест: шумовые метки дают
  0 узлов и 0 call_sites.
- [x] T-78.7 Тесты `tests/test_ast_langs.rs`: реестровый инвариант —
  автоматически; кейс `sample.S` → `asm-gas`; словарные ассерты узлов/рёбер;
  MASM line_end; отсутствие `.L*`-узлов; `call *%rax` без call_site.
- [x] T-78.8 Golden-эталоны `tests/fixtures/ast_golden/{asm-gas, asm-nasm,
  asm-masm}/` (`expected.json`: ≥4 символов fn/macro/data, ≥2 вызова,
  `call_extraction: "current"`) + вшитые (load_embedded, 8 языков).
- [x] T-78.9 Кросс-файловый тест C → asm: extern-декларация + `call` →
  `CALLS`-цепочка (type-pass; добавлена сбор call_sites для C/C++ и
  линкерный мост C → asm — единственный asm-кандидат по имени, RESOLVED;
  отступление от спеки: Function-заглушки не создаются — как у прочих
  языков).
- [x] T-78.10 Документация: словарь node_type и аддендум Ф78/78b в
  ARCHITECTURE; запись в журнале ревизий плана.

## Фаза 78b — Tree-sitter-волна: GAS через `tree-sitter-asm`

- [x] T-78b.1 Спайк грамматики: фактические node kinds CST зафиксированы в
  design.md §6 (program/meta/label/instruction/ERROR; хвост `.equ X, v` —
  ERROR ~1–2%, в пороге 70.9).
- [x] T-78b.2 `tree-sitter-asm = { version = "0.24", optional = true }` в
  фичу `treesitter`; `lang_for_path`: `s`/`asm` → `"asm"`;
  `has_mapper("asm")`; грамматика в `ts::language_for`.
- [x] T-78b.3 `ts::map_asm`: CST→AST маппер GAS, зеркалящий node_id
  regex-пути (паритет-тест); включение только для GAS-диалекта (сниффинг до
  диспетчера: NASM/MASM-контент → regex); инкрементальный LRU (Ф70.8) и
  ERROR-fallback 10% (Ф70.9) — переиспользованием диспетчера.
- [x] T-78b.4 Валидация против golden `asm-gas`: symbol F1 ≥ 0.95, call F1 ≥
  0.90 (гейты 70.11/71.7); kill-switch `OB2H_TREESITTER=0` возвращает regex
  для всех asm-файлов — тест.
- [x] T-78b.5 Телеметрия бэкенда `treesitter:asm` в скан-статусе (строка
  бэкенда диспетчера без правок); заметка в ARCHITECTURE о границах
  грамматики (generic asm, NASM/MASM — regex).
