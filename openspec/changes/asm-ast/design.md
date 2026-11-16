# Design: Asm AST (`asm-ast`)

Продолжение нумерации фаз PLAN_v2.0 (занято до 77). Паттерн: regex-волна
(образец — Ф48–Ф52) → tree-sitter-маппер (образец — Ф70.3–70.6). Все node_id и
описания зеркалят конвенции regex-бэкенда v1.x — стабильность привязки памяти
и Ralph-findings к node_id.

## 1. Диалекты и детекция

| Диалект | Расширения | Реестр | Примечание |
|---|---|---|---|
| GAS AT&T (x86-64 / AArch64) | `.s`, `.S` | `asm-gas` | вывод gcc/clang, ядра, libc |
| GAS Intel (`.intel_syntax noprefix`) | `.s`, `.S` | `asm-gas` | тот же экстрактор, диалект из контента |
| NASM | `.asm` | `asm-intel` | `section .text`, `%macro`, `global` |
| MASM (ml/ml64) | `.asm` | `asm-intel` | `PROC/ENDP`, `SEGMENT`, `.MODEL`, `EXTERN` |

`.s` и `.S` неразличимы матчером — `LangSpec::for_path` нижнит регистр
расширения (Ф47.1); препроцессорный `.S` обрабатывается тем же экстрактором
(директивы `#include`/`#if` C-препроцессора вырезаются из рассмотрения как
не-символы).

Расширение не определяет диалект — **сниффинг по контенту** (первые ~256
строк, функция `asm_dialect(content, default_by_ext) -> GasAtt | GasIntel |
Nasm | Masm`):

- `%define` / `%macro` / `section .text` / `global ` / `extern ` → NASM;
- ` PROC` / `ENDP` / `SEGMENT` / `.MODEL` / `END` → MASM;
- `.intel_syntax` → GAS Intel;
- иначе: `.s`/`.S` → GAS AT&T, `.asm` → NASM (default).

Детекция детерминированная и дешёвая; результат — только выбор таблицы
паттернов экстрактора (на tree-sitter-волну не влияет: грамматика одна).

## 2. Модель узлов

| node_type | Что даёт | node_id |
|---|---|---|
| `File` | файл-узел (зеркало прочих языков); секции `.text`/`.data`/`.bss` и `end`-точка входа — в description | `file:{rel}` |
| `Function` | метка в кодовой секции: `name:` (GAS/NASM), `name PROC … name ENDP` (MASM); entry-метки (`.globl name` / `global name` / `PUBLIC name`) помечаются в description | `fn:{rel}:{name}` |
| `Macro` | `.macro name … .endm` (GAS), `%macro name args … %endmacro` (NASM), `name MACRO … ENDM` (MASM) | `macro:{rel}:{name}` |
| `Data` | метки в `.data`/`.bss`/`section .data`, директивы `.equ name, v` / `name EQU v` / `%define name v` / `name DQ/DW/DD ...` | `data:{rel}:{name}` |

Словарь `node_types` asm-записей реестра: `["File", "Function", "Macro",
"Data"]`. `Function` — осознанно тот же тип, что у прочих языков: кросс-файловый
резолв `CALLS` через type-pass и `project_call_path` работают без специфики.

### Skip-листы (анти-шум, прецедент `RUST_CALL_SKIP`)

Локальные и компиляторные метки не становятся узлами и не дают `call_sites`:

- GAS: `.L*` (`.L1`, `.LBB0_3`), `.Lfunc_end*`, числовые метки `1:`–`9:`;
- точки входа thunk/plt: `__x86.get_pc_thunk.*`, `*.cold`, `*.plt`, `*.got` —
  по суффиксу;
- NASM/MASM: локальные метки `.loop`/`@@`/`@f`, `LOCAL`-директивы MASM;
- директивы-как-метки (`.text:`) отсекаются префиксом точки.

Список — `pub const ASM_LABEL_SKIP: &[&str]` в `ast.rs`, расширяется по мере
реальных кейсов (grep-уемо, тестируемо).

## 3. Рёбра

| label | Источник → Цель | Что даёт |
|---|---|---|
| `DEFINES` | `file:{rel}` → `fn:`/`macro:`/`data:` | все определённые символы (зеркало прочих языков) |
| `IMPORTS` | `file:{rel}` → `module:{include}` | `#include "x.S"` / `%include "x.asm"` / `INCLUDE x` (файловые включения) |
| `CALLS` | `fn:{rel}:{caller}` → `fn:*:{callee}` | из call_sites через существующий type-pass; цели-не-найденные автосоздаются как `Function`-заглушки (как у прочих языков) |

Не-цели (см. proposal): переходы `jmp`/`jcc`/`b label` — не рёбра; `extern`/
`EXTRN`-декларации — не `IMPORTS`-рёбра (это ссылки на символы, а не файловые
включения; вызов extern-символа дойдёт через `CALLS` call-site'ом).

## 4. call_sites

Инструкция вызова с разрешимым статически именем → `CallSite { rel_path,
line, name, qualifier: None }`:

- x86: `call name`, `callq name`, `jalr name`; косвенные (`call *%rax`,
  `call rax`, `call [rbx]`, `call qword ptr [x]`) — пропуск.
- AArch64: `bl name`, `jal name`; `b label` — переход, не вызов, пропуск.

Дедуп `sort + dedup` — как в ts-мапперах (71.1). Словарь skip-слов для
регистров/ключевых слов не нужен: паттерн требует операнд-идентификатор, не
регистр (синтаксис различим: `%rax`/`rax`-регистры фильтруются списком
регистров x86-64/AArch64 — компактный `ASM_REG_SKIP`).

## 5. Regex-экстрактор (Фаза 78)

`Parser::Asm` в `AstCodeExtractor::parse_file`: сниффинг диалекта → выбор
таблицы паттернов → один линейный проход (строка за строкой, как прочие
regex-парсеры v1.x):

1. секции/директивы → состояние (текущая секция: code / data) — влияет на
   тип узла метки (`Function` vs `Data`);
2. `DEFINES`-метки + `PROC/ENDP`-блоки (MASM: имя функции берётся из `PROC`,
   `line_end` — из парного `ENDP`);
3. макросы (`Macro`), `.equ`/`EQU`/`%define` (`Data`);
4. `include`-директивы → `IMPORTS` + `ImportRef` (module = нормализованный
   путь включения без кавычек);
5. `call`/`bl`/`jal` → call_sites (skip-листы §2, §4);
6. entry-метки (`.globl`/`global`/`PUBLIC`) → дополнение description узла
   `Function` пометкой «(entry)».

`max_bytes`: `DEFAULT_CAP_BYTES` (прецедент ADR-AST-4: листинги/дизасм-дампы
могут быть мегабайтными; сверх капа файл трекается по хэшу, не парсится).

### Записи реестра

```rust
LangSpec {
    name: "asm-gas",
    matcher: Matcher::Ext(&["s"]),            // матчер нижнит регистр: .S тоже
    node_types: &["File", "Function", "Macro", "Data"],
    edge_labels: &["IMPORTS", "DEFINES", "CALLS"],
    max_bytes: DEFAULT_CAP_BYTES,
    parser: Parser::Asm,
    fixture: r#"…минимальный GAS-файл: .globl, метка, call, .equ…"#,
    sample_rel: "",
},
LangSpec {
    name: "asm-intel",
    matcher: Matcher::Ext(&["asm"]),
    node_types: &["File", "Function", "Macro", "Data"],
    edge_labels: &["IMPORTS", "DEFINES", "CALLS"],
    max_bytes: DEFAULT_CAP_BYTES,
    parser: Parser::Asm,                      // диалект — сниффингом контента
    fixture: r#"…минимальный NASM-файл: section/global/метка/call…"#,
    sample_rel: "",
},
```

Инварианты Ф47.5 (реестр↔парсер↔словари) и Ф47.2 (обходчик↔вотчер) включаются
автоматически самим фактом записи + фикстуры; отдельных тестов не требуется,
нужны только специфичные проверки ниже.

## 6. Tree-sitter-волна (Фаза 78b)

- Cargo: `tree-sitter-asm = { version = "0.24", optional = true }` (crates.io,
  RubixDev — generic asm grammar, используется nvim-treesitter как `asm`) в
  фичу `treesitter`.
- `lang_for_path`: `"s" | "S" → "asm"`, `"asm" → "asm"`; `has_mapper("asm")`.
- `ts::language_for`: `"asm" => tree_sitter_asm::LANGUAGE.into()`.
- `ts::map_asm` — CST→AST маппинг, зеркалящий узлы/рёбра regex-пути (node_id
  обязаны совпадать — стабильность привязки памяти).
- **Спайк T-78b.1 — выполнен** (фактические node kinds грамматики 0.24,
  зафиксированы на asm-gas эталоне): корень `program`; строки — `meta`
  (директивы: `meta_ident` + операнды `ident`/`reg`/`word`/`string`) |
  `label` (`ident` + анонимный `:`) | `instruction` (опкод `word` + операнды
  `ident`/`reg`/`word`/`string`) | `ERROR`. Метки (`label.ident`) → имя;
  `.globl`/`.global`/`.text`/`.data`/`.section`/`.equ`/`.macro`/`.endm`/
  `.include` — ветвление по тексту `meta_ident`; хвост `.equ NAME, v`
  грамматика не принимает (`,`-часть — ERROR-узел, ~1–2% файла — в пределах
  fallback-порога 70.9, имя берётся из named_child(1)).
- Инкрементальный LRU-парсер (Ф70.8) и ERROR-fallback 10% (Ф70.9) —
  переиспользуются диспетчером без изменений; kill-switch `OB2H_TREESITTER=0`
  глобален.
- NASM/MASM остаются на regex: маппер включается только для диалекта GAS
  (сниффинг выполняется до выбора пути диспетчером: расширение `asm` с
  контентом NASM/MASM → regex; `.s`/`.S` → tree-sitter с фолбеком).
- Валидация против тех же golden-эталонов: symbol F1 ≥ 0.95, call F1 ≥ 0.90
  (гейты 70.11/71.7).

## 7. Тесты

1. **Реестр**: инвариант `test_registry_consistency` проходит автоматически
   (фикстуры в `LANGS`); отдельный кейс — `sample.S` (верхний регистр)
   матчится на `asm-gas`.
2. **Диалекты**: юнит-тесты сниффинга — 4 мини-файла (по одному на диалект) в
   `tests/test_ast_langs.rs` рядом с прочими языками: узлы/рёбра в словаре
   (`assert_has_node`/`assert_has_edge`), MASM `PROC/ENDP` даёт `line_end`,
   `.L*`-метки отсутствуют, `call *%rax` не даёт call_site.
3. **Golden**: `tests/fixtures/ast_golden/{asm-gas,asm-nasm,asm-masm}/`
   (`source.s`/`source.asm` + `expected.json`): ≥4 символов (fn/macro/data) и
   ≥2 вызовов, `call_extraction: "current"` — вызовы обязательны уже на
   regex-волне.
4. **Кросс-файловый**: C-файл с extern-декларацией asm-метки + `call` →
   `CALLS`-цепочка C → asm в `project_call_path` (по образцу
   `test_incremental_crossfile.rs`).
5. **Skip-листы**: таблица шумовых меток → 0 узлов, 0 call_sites.

## 8. Риски и альтернативы

| Риск | Митигация |
|---|---|
| Схема CST `tree-sitter-asm` не задокументирована | спайк T-78b.1 до маппера; regex-волна не зависит от грамматики |
| Дизасм-дампы/листинги раздуют граф | `DEFAULT_CAP_BYTES` + skip-листы; generated-маски — отложено |
| Смешение NASM/MASM в одном `.asm` | сниффинг берёт первый уверенный признак; конфликты → default NASM; реальные кейсы редко смешаны |
| `.S`-препроцессорные директивы (`#if`) ломают regex-паттерны | строки, начинающиеся с `#`, пропускаются до разбора (кроме `#include`) |
| Ложные `Function` от данных в `.rodata` с код-подобными именами | секционное состояние (§5.1): метки вне кодовых секций — только `Data` |

Отвергнутые альтернативы: отдельные грамматики NASM/MASM (нет на crates.io —
своё сопровождение грамматик не окупается); CFG-рёбра переходов (шум, см.
proposal); отдельный node_type `Label` вместо `Function` (ломает единый
type-pass резолв `CALLS`).
