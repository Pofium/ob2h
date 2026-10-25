Первый анализ плана v2  
Прежде чем критиковать план, проверю несколько фактов, на которых он опирается: состояние репозитория (v1.9 / 41 инструмент), актуальность Tree-sitter, fastembed/ort и sqlite-vec.# Рецензия на план OB2H v2.0 и что в него добавить

Коротко: план логично выстроен и выбирает правильные направления (Tree-sitter, батчинг эмбеддингов, PPR, консолидация знаний). Но у него четыре системные проблемы: **(1)** он построен на базе, которая ещё не стабилизирована; **(2)** оценки трудозатрат занижены в 2,5–3 раза; **(3)** целевые метрики («99 %», «10–15 мс», «100k узлов») не имеют измеренного baseline и частично технически необоснованны; **(4)** несколько строк таблицы «As-Is / To-Be» не имеют соответствующей фазы реализации. Ниже — по пунктам, с конкретными правками.

---

## 1. Расхождение между планом и фактическим состоянием репозитория

План называет предыдущим этапом v1.9.0 (Secrets Vault, 41 инструмент, SCHEMA_VERSION 10). Однако на GitHub последний опубликованный релиз — по-прежнему v1.8.0, и после него в main только 1 коммит. Контракт v1.5.0 описывал 36 инструментов.

**Что добавить:**
- Явно указать в преамбуле, что v1.9 — это неопубликованное состояние `main` (или ещё не слитая ветка), и зафиксировать коммит/тег, от которого отсчитывается v2.0.
- Добавить **Фазу 69 «Стабилизация базы»** перед Фазой 70. Проблемы из предыдущего анализа никуда не делись и в плане не упомянуты: дефолтный репозиторий автообновления `presnyakov/ob2h` вместо `Pofium/ob2h`, провалившиеся запуски release-workflow (упаковка `plugin/tests/trybuild`, `target`), расхождение README и `docs/SYNC.md` по политике конфликтов sync (field-merge vs LWW), бейдж версии в README. Строить мажорный релиз на пайплайне, который не собирает артефакты, — значит получить v2.0, которую нельзя установить через `ob2h update`.

---

## 2. Оценка трудозатрат: 12 дней нереалистичны

| Фаза | В плане | Реалистично | Почему |
|---|---|---|---|
| 70 Tree-sitter Core (4 языка) | 3 дня | 8–12 дней | Один язык с `impl`/макросами/`use`-резолвом и golden-тестами — это 2–3 дня минимум; плюс интеграция в `scan`, watcher, миграция ID узлов |
| 71 Deep Call Graph | 2 дня | 5–8 дней | Требует разрешения имён (см. п. 3), а не просто обхода `CallExpression` |
| 72 ONNX + worker pool | 2 дня | 4–6 дней | Сборка `ort` на трёх платформах, различия векторов Candle↔INT8, переиндексация |
| 73 U-PPR | 2,5 дня | 4–5 дней | Нужна оценка на golden-наборах, иначе веса подбираются вслепую |
| 74 Distillation | 2 дня | 5+ дней + период «dry-run» | Самая рискованная фаза, нельзя выпускать без режима предпросмотра |
| 75 Release/CI/доки | 1,5 дня | 3–4 дня | Матрица features × платформы удваивает число артефактов |
| **Итого** | **~12** | **~30–40** | |

**Рекомендация:** не растягивать один релиз, а разбить на три мажорных/минорных:
- **v2.0** — Фаза 69 + Tree-sitter Core + батчинг/worker pool (без ONNX) + PPR с бенчмарками.
- **v2.1** — Deep Call Graph + ONNX-бэкенд.
- **v2.2** — Knowledge Distillation (сначала только в режиме отчёта).

Так каждый релиз даёт измеримую пользу, а риск Фазы 74 не блокирует остальное.

---

## 3. Столп I (Tree-sitter): что исправить в формулировках и дизайне

### 3.1. Tree-sitter даёт синтаксис, не семантику
Пункты «разрешение перегрузок», «реализация трейтов», «expression-level call graph» подразумевают **name resolution и вывод типов**, которых в Tree-sitter нет. Для `a.b().c()` парсер даст узел `call_expression` с полем `function: field_expression`, но не скажет, какой именно метод `c` вызывается — это задача rust-analyzer / tsserver / pyright. Для Python и TS без типов связь принципиально эвристична.

**Что изменить:**
- Переформулировать цель Фазы 71 как «**кандидатные рёбра с confidence**» (хорошо, что `ast_edges.confidence` уже запланирован). Добавить колонку `provenance` со значениями `SYNTAX` (точно из дерева), `RESOLVED` (импорт + сигнатура совпали однозначно), `HEURISTIC` (по имени метода).
- Убрать «99 % точности» из таблицы As-Is/To-Be. Заменить на: «Precision/Recall по размеченному корпусу, измеряется в Фазе 70.7; целевые пороги устанавливаются после baseline». Текущая цифра «80–85 %» для regex тоже ниоткуда не взята — её нужно сначала измерить тем же корпусом.
- Для трейтов: `IMPLEMENTS` из `impl Trait for Struct` — это синтаксически точно, оставить. А «виртуальные вызовы для Blast Radius» (71.4) — пометить как HEURISTIC и отдельно сообщать в выдаче `project_impact`.

### 3.2. Использовать queries, а не ручной обход дерева
Вместо написания обходчика CST на каждый язык — использовать tree-sitter queries (`.scm`), в т.ч. готовые `tags.scm` из официальных грамматик, которые уже размечают definitions/references для функций, классов, методов. Это сокращает объём кода в 3–5 раз и делает добавление нового языка декларативным.

### 3.3. Версионная совместимость грамматик
Актуальная версия биндингов — tree-sitter 0.27.0 — Rust bindings to the Tree-sitter parsing library; грамматика Rust — 0.24.2: Mar 27, 2026. Грамматики и ядро обновляются асинхронно; крейт tree-sitter-language предоставляет тип LanguageFn, чтобы грамматики создавали Language без привязки к версии крейта tree-sitter. **Добавить в 70.1:** зафиксировать версии грамматик в `Cargo.lock`, использовать `tree-sitter-language` для развязки, добавить тест «все грамматики загружаются и ABI совместимо».

### 3.4. Пересмотреть feature flag `treesitter`
Аргумент «чистая сборка без C++» не вполне применим: `rusqlite` с `bundled` уже компилирует C-код SQLite через `cc`, т.е. C-компилятор и так требуется. Tree-sitter добавляет только ещё C-файлы грамматик, без внешних библиотек. **Предложение:** сделать Tree-sitter частью default-сборки, а feature flag оставить только для ONNX (у которого реально есть внешний рантайм). Это вдвое сокращает матрицу тестирования. Цена — размер бинарника (+1–3 МБ на грамматику), это стоит измерить и указать в плане.

### 3.5. Что ещё не хватает в Столпе I
- **Политика fallback:** когда именно включается regex? Предложение: если доля `ERROR`-узлов в дереве > N % или грамматика отсутствует — fallback с пометкой в диагностике файла.
- **Инкрементальный парсинг:** Tree-sitter умеет `Tree::edit` + повторный parse. Связать с watcher — при сохранении файла не перепарсивать с нуля. Сейчас в плане этого нет.
- **Миграция идентичности узлов:** смена парсера изменит границы (line ranges), сигнатуры и, вероятно, ID узлов. Нужно: bump `extractor_version`, стратегия «полный rescan + tombstone старых узлов», оценка времени на 100k LOC, уведомление пользователя при первом запуске v2.0.
- **Fuzz/property-тест:** «парсер и маппер никогда не паникуют на произвольном входе» (`cargo-fuzz` или простой proptest на мусорных файлах). Tree-sitter сам устойчив, но ваш код преобразования CST→`ast_nodes` — нет.
- **Корпус:** вендорить небольшие фикстуры с «неудобным» кодом (макросы, generics, многострочные сигнатуры, декораторы, async, re-exports, `#[cfg]`), а не только тестировать на `ob2h` и `omnes-aibot`.

---

## 4. Столп II (ONNX/FastEmbed): сначала измерить, потом подключать

### 4.1. Сначала батчинг и вынос из async — это может дать большую часть выигрыша
Цифра «50–100 мс холодный расчёт Candle» в плане не подтверждена замером. MiniLM-L6 на CPU для короткого текста обычно даёт единицы–десятки мс; реальный тормоз в текущем коде — поштучный `embed()` без батчинга и синхронный инференс внутри async-задачи Tokio. **Предложение:** поменять порядок внутри Фазы 72: сначала 72.2 (worker pool + батчи по 32/64 + `spawn_blocking`) и 72.4 (`ob2h bench embeddings`) **для Candle**, замерить, и только потом решать, нужен ли ONNX вообще. Candle поддерживает батчевый inference нативно.

### 4.2. Зависимость ort — всё ещё release candidate
fastembed 7.5.0 зависит от ort =2.0.0-rc.15 — с точной фиксацией версии. Сам `ort` — 2.0.0-rc.15 (2026-10-19), 2.0.0-rc.14 (2026-10-12), т.е. RC обновляются еженедельно и API может меняться. Это нормально за feature-флагом, но план должен это проговорить: «ONNX-бэкенд — экспериментальный до выхода ort 2.0 stable».

### 4.3. «Авто-выбор бэкенда при запуске» (72.3) нужно переосмыслить
`ort` обычно линкует или скачивает ONNX Runtime **на этапе сборки**, поэтому «если доступна библиотека ONNX» — это решение compile-time, а не runtime. Для runtime-детекции потребуется `load-dynamic` и поиск `.so/.dll` в системе — это отдельная работа и источник багов на Windows. Проще: два артефакта (`ob2h` и `ob2h-full`) и явная настройка `embedding.backend = candle|onnx` в конфиге.

### 4.4. Критично: Candle и INT8-ONNX дают **разные векторы**
Квантованная модель и FP32-модель на одном и том же тексте дают близкие, но не идентичные эмбеддинги. Если пользователь переключит бэкенд, старые векторы в БД будут несовместимы с новыми запросами — качество поиска просядет незаметно. **Добавить:** хранить `embedding_model_id + backend + quantization + dim` рядом с векторами (в `meta`-таблице и/или в каждой записи), при несовпадении — отказывать в смешивании и предлагать `ob2h reindex`. Это также нужно для будущей смены модели.

### 4.5. Безопасность при API-провайдере
План v1.9 включает Secrets Vault, а в кодовой базе есть `ApiEmbedding`. Нужно гарантировать, что содержимое `.env`, ключей, `secrets.*` и т.п. **не попадает ни в эмбеддинги, ни в AST-граф, ни в sync-bundle**. Добавить фильтр/редакцию до вызова embedding-провайдера и тест на это.

---

## 5. Строка «Vector Storage» не имеет фазы — и sqlite-vec не решает описанную проблему

В таблице заявлена цель «sqlite-vec / SIMD», но в Фазах 70–75 её нет. При этом важно: стабильный релиз sqlite-vec делает точный brute-force поиск, а актуальные версии — 0.1.9 и v0.1.10-alpha.4 Pre-release. То есть sqlite-vec ускоряет линейный скан за счёт SIMD, но **алгоритмически он остаётся O(N)** — проблему «>50k векторов» он решает лишь константой.

**Что добавить (Фаза 72b или отдельная 76):**
1. Сначала замер: сколько реально занимает скан 50k int8-векторов 384-dim в Rust с SIMD (например, через `simsimd` или ручной dot-product на `i8`) — скорее всего это единицы мс, и ANN не нужен до 200–500k.
2. Если нужен ANN — рассматривать in-process HNSW (`usearch`, `hnsw_rs`) с сохранением индекса файлом рядом с БД, не нарушая ADR «только SQLite» в части данных. sqlite-vec оставить опциональным, как сейчас.
3. Двухступенчатый поиск: грубый отбор по int8/binary → rerank top-200 по точным значениям.

---

## 6. Столп III (U-PPR): формула создаёт петлю обратной связи

Формула `W(e) = W0 × (1 + ln(1+access_count)) × e^(−λΔt) × trust_score` имеет три проблемы:

1. **«Богатые богатеют».** Узел попал в выдачу → `access_count++` → вес выше → чаще попадает в выдачу. Через месяц граф «залипнет» на нескольких узлах, а новые/редкие знания не будут всплывать. Нужны: верхний cap на usage-бонус, периодическая нормализация, доля «исследования» (exploration) в ранжировании и A/B на golden-наборе: «с usage-весом» vs «без».
2. **Временное затухание для кода бессмысленно.** Функция, не менявшаяся год, не «устарела» — она стабильна. `λ` должен быть разным для memory и code graph (для code — 0 или зависеть от `git log`, а не от времени доступа).
3. **`trust_score` в произведении обнуляет ребро** при trust=0 (что планирует Фаза 74). Отрезанный узел перестаёт быть достижим даже для вопроса «почему это устарело?». Лучше `(0.1 + 0.9 × trust)`.

**Также добавить:**
- **Качество seeds важнее PPR.** Ваш же code-bench показал, что 8 из 15 кейсов оставались без стартовых узлов. PPR на пустом personalization vector ничего не даст. В Фазу 73 нужен пункт 73.0: «метрика seed-recall на golden-наборе ≥ X % до включения PPR».
- **Подграф, а не весь граф.** «<20 мс на 100k узлов» достижимо только на извлечённом k-hop подграфе с лимитом узлов. Описать: k, лимит, способ выгрузки из SQLite в `GraphAdjacency`, кэширование между запросами.
- **Логирование доступа (73.2) = запись на каждое чтение.** При одном `Mutex<Connection>` это write-amplification и блокировки. Копить счётчики в памяти и сбрасывать батчем раз в N секунд / при shutdown.
- **Приватность access-логов:** они показывают, что спрашивал агент. Решить, попадают ли они в sync-bundle и backup, и указать в документации.
- **Multi-hop golden-набор** — без него, как показал ваш эксперимент с весами рёбер, калибровка не изменит выдачу.

---

## 7. Столп IV (Distillation): самая рискованная часть, нужны тормоза

- **«Выбрать более поздний факт как истинный»** — плохая эвристика. Поздний факт мог прийти от агента, который ошибся; ранний мог быть явно задан пользователем. Нужно учитывать источник (`user` > `agent`), importance, число подтверждений, тип (предпочтение vs факт о коде).
- **«Снижение trust до нуля» автоматически** — нельзя в первом релизе. Предлагаемый путь: v2.0 — только детекция + ребро `CONTRADICTS` + отчёт; v2.1 — предложение `SUPERSEDES` с подтверждением через `knowledge_reconcile`; v2.2 — автоприменение только при confidence выше порога и с обратимым журналом (аналогично уже существующему dream rollback).
- **Метрики детектора:** нужен размеченный набор пар «противоречие / не противоречие / уточнение». Без precision/recall детектора авторезолв будет уничтожать корректные уточнения («раньше порт 8080, теперь 8081 на staging» — это не противоречие).
- **Честность про «никаких облаков».** Фаза 74.2 вызывает `llm_client`, а LLM по умолчанию — внешний API (DeepSeek). Фраза «никаких облаков и сторонних серверов» в гарантиях v2.0 неверна. Нужно: либо честно написать «LLM-операции требуют API, если не настроен локальный endpoint», либо гарантировать работу Distillation без LLM (только эмбеддинг-близость + правила) в offline-режиме.
- **Стоимость:** ночной прогон попарных сравнений на 50k фактов — квадратичен. Ограничить кандидатов блокингом по категории/проекту и top-k по вектору.

---

## 8. Чего в плане нет совсем

| Пробел | Что добавить |
|---|---|
| **Миграция и откат** | `ob2h migrate --dry-run`, автоматический backup перед M10, задокументированный путь отката на v1.9 (downgrade схемы или восстановление из backup) |
| **Переиндексация** | Единая команда `ob2h reindex --ast / --embeddings` с прогрессом; оценка времени на 100k LOC; поведение при прерывании |
| **Бенчмарк-свод** | Один `ob2h bench all` (search golden, code-bench, ast accuracy, embeddings, PPR) с JSON-выводом и порогами в CI, чтобы регрессии ловились до релиза |
| **MCP-контракт** | `knowledge_reconcile` → 42 инструмента: зафиксировать детерминированный порядок `tools/list`, версию контракта, тесты совместимости с клиентами; указать поддерживаемую версию спецификации MCP |
| **Бюджет памяти** | Tree-sitter-грамматики + Candle + (ONNX) одновременно в RSS. Ленивая загрузка: грамматика по первому файлу языка, модель — по первому embed-запросу (сейчас `init_app` грузит модель даже для `stats`) |
| **ADR** | Проект ведёт ADR-1..15; каждому столпу нужен свой ADR (16–19) с альтернативами и причинами отказа (почему не LSP, почему не Neo4j, почему не usearch и т.д.) |
| **Платформы** | В 75.1 нет macOS, хотя README заявляет поддержку; нет Windows aarch64. Нужна явная таблица «платформа × feature-набор × тестируется ли в CI» |
| **Безопасность** | `cargo audit`/`cargo deny` в CI, checksums + подпись релизов, SBOM — особенно с добавлением C-кода грамматик и ONNX-рантайма |
| **Deprecation** | Политика для regex-парсеров топ-4 языков: когда удаляются, как сообщается пользователю, что происходит с файлами, где tree-sitter дал `ERROR` |
| **Sync и новые рёбра** | Как `IMPLEMENTS/SUPERSEDES/CONTRADICTS`, `access_count`, `confidence` мержатся в sync v2 между машинами? Если конфликт-политика LWW — счётчики доступа будут затираться |

---

## 9. Исправленный граф зависимостей

В исходной схеме Фаза 72 (ONNX) зависит от Фазы 70 (Tree-sitter) — реальной зависимости нет, их можно вести параллельно. Зато Фаза 73 зависит от 73.2 (логирование доступа), которое меняет MCP-инструменты, и от golden-наборов. Предлагаемая последовательность по принципу «ценность / риск»:

```text
Фаза 69  Стабилизация (update repo, release CI, sync docs, offline-режим)
   │
   ├──► Фаза 72a  Батчинг + worker pool + spawn_blocking для Candle + bench embeddings
   │        └──► Фаза 72b  ONNX (feature flag, экспериментально)           [v2.1]
   │
   ├──► Фаза 73.0 Golden-наборы: seed-recall, multi-hop, contradiction pairs
   │        └──► Фаза 73  U-PPR с cap/exploration, A/B на golden            [v2.0]
   │
   └──► Фаза 70  Tree-sitter Core (queries/tags.scm, fallback policy, fuzz) [v2.0]
            └──► Фаза 71  Candidate Call Graph с provenance/confidence     [v2.1]
                     └──► Фаза 74  Distillation: отчёт → подтверждение → авто [v2.2]
                              └──► Фаза 75  Release, ADR-16..19, migration guide
```

---

## 10. Итог: пять правок, без которых план стоит не утверждать

1. **Добавить Фазу 69** и зафиксировать реальную отправную точку (v1.8.0 опубликован; v1.9 — `main`).
2. **Заменить все целевые цифры** («99 %», «10–15 мс», «100k узлов») на «измеряется в бенчмарке X, порог устанавливается после baseline».
3. **Переформулировать Столп I** как синтаксически точный граф + кандидатные рёбра с `provenance`, а не «полноценное разрешение методов и трейтов».
4. **Отложить ONNX** до результатов батчинга Candle; добавить идентификатор модели/бэкенда к векторам.
5. **Сделать Distillation поэтапно обратимой** (отчёт → подтверждение → авто) и убрать из «гарантий» утверждение «никаких облаков», пока LLM по умолчанию — внешний API.

Если нужно, могу переписать документ в виде готового `PLAN_v2.md` с этими правками, таблицей фаз и чек-боксами в том же формате, что у вас.    


Второй анализ плана v2  
  
Сверю план v2.0 с актуальным состоянием репозитория и лучшими практиками 2025–2026, затем предложу конкретные дополнения.Просматриваю ключевые ADR и текущее состояние PPR/vec0, чтобы не дублировать уже сделанное.# Ревью плана OB2H v2.0: что добавить и улучшить

План по направлению **верный**: tree-sitter, ONNX/FastEmbed, U-PPR, SUPERSEDES — это ровно те рычаги, которые отделяют «хороший личный мозг» от **production Graph & Code Intelligence**. Ниже — критика, пробелы относительно текущего кода/ADR и конкретные дополнения, которые стоит вшить в черновик до утверждения.

---

## 1. Что в плане уже сильно

| Столп | Оценка | Почему |
|--------|--------|--------|
| Hybrid AST (TS + regex) | ★★★★★ | Сохраняет zero-dep default и ADR-1..8 |
| FastEmbed/ONNX feature-flag | ★★★★★ | Candle остаётся fallback — правильно |
| U-PPR с access/time/trust | ★★★★☆ | Совпадает с EngramRAG / HippoRAG-линией; у вас уже есть base PPR |
| SUPERSEDES / CONTRADICTS в Dream | ★★★★★ | Split-brain → 0% в литературе как раз через directed DAG |
| Совместимость M10 + аддитивный MCP | ★★★★★ | Не ломает агентов |

---

## 2. Критичные пробелы и риски плана

### 2.1. Оценка «~12 рабочих дней» занижена в 2–3×

Реалистичнее:

| Фаза | План | Реалистично | Почему |
|------|------|-------------|--------|
| 70 Tree-sitter Core | 3 д | **5–7 д** | C-FFI, static grammars, Windows MSVC/link, binary size, CI matrix |
| 71 Call Graph | 2 д | **4–6 д** | Cross-file resolve, trait/impl, method chains — самая сложная часть |
| 72 FastEmbed | 2 д | **3–4 д** | ort/fastembed packaging, model download, dim migration, A/B bench |
| 73 U-PPR | 2.5 д | **3–4 д** | У вас PPR уже есть; usage weights + integration + multi-hop golden |
| 74 Distillation | 2 д | **4–5 д** | LLM cost, false positives, rollback, trust calibration |
| 75 Release | 1.5 д | **2–3 д** | Docs, migration guide, dual-feature CI |

**Итого: ~21–29 чел·дней**, не 12. Лучше зафиксировать **v2.0 = 3–4 спринта** или разбить на **v2.0-alpha (AST+embed)** и **v2.0 (graph+dream)**.

### 2.2. «99% точность AST» — непроверяемый KPI

Нужны **измеримые** критерии:

- golden fixtures per language (symbols / calls / imports / inherits);
- precision/recall vs hand-labeled mini-repos (ob2h self + 1–2 public);
- parity hash: `project_scan` deterministic across runs;
- regression suite на multi-line fn, macros, generics (из вашего CHANGELOG).

Формулировка: **«≥95% symbol F1 и ≥90% CALLS F1 на golden top-4»**, не «99%».

### 2.3. U-PPR: не «с нуля», а эволюция существующего

В `src/graph/pagerank.rs` уже есть:

- power iteration, damping 0.5–0.85, degree-normalization, dangling teleport;
- `DEFAULT_PPR_WEIGHTS`, `access`-готовый path в memories;
- вердикт Ф54: на code golden **веса рёбер почти не влияют** — bottleneck в **seeds**, не в walk.

**Что добавить в фазу 73:**

1. **Multi-hop golden set** (цель только через 2–3 hop) — иначе U-PPR нечем калибровать (вы сами это писали в комментарии к коду).
2. Не только weight edges, а **seed reweighting** + **filter SUPERSEDED nodes out of walk**.
3. **CATD-lite** (EngramRAG): decay half-life зависит от topological load-bearing, не только `e^{-λΔt}` — иначе «scaffolding amnesia» (SOUL/USER факты вымываются).
4. Grace period `N_grace ≥ 4` access’ов для новых узлов (cold-start trust).
5. Бюджет latency: PPR subgraph cap (top-N nodes after FTS+vec seeds), иначе 100k узлов × 15 iter ≠ «&lt;20 ms».

Формула в плане хорошая — добавьте **cap и early exit**, иначе обещание «&lt;20 ms» сорвётся на больших графах.

### 2.4. Vector storage сформулирован слабо

Столп IV в таблице «As-Is»: *sqlite-vec / SIMD*. В фазах **отдельной фазы vec0 production path нет** — только embeddings.

У вас уже ADR-14/35.1: vec0 int8 rescore даёт выигрыш на vec-стадии, но **end-to-end p95** убивает full-scan `label/description`. План v1.9 якобы уже сделал FTS5 Graph — отлично; в v2.0 обязательно:

| # | Задача | Почему |
|---|--------|--------|
| V1 | Incremental vec0 upsert on node write | Full rebuild не масштабируется |
| V2 | rescore (int8/bit + oversample) default path | sqlite-vec PR #276 — 2.6–6× |
| V3 | Binary quant + float rescore optional | 15–45× на retrieval при recall ≥0.95 |
| V4 | Partition by `project_id` / memory vs graph namespaces | Isolation + smaller scans |
| V5 | Nightly recall@10 gate vs brute-force | ADR-13 continuity |

**Рекомендация:** вставить **Фазу 72.5 / 76 — Vector Index Production** между FastEmbed и U-PPR.

### 2.5. Tree-sitter: не хватает инкрементальности и resolve

Фаза 70–71 описывает parse, но **killer feature tree-sitter — incremental edit**, а не one-shot parse.

Добавить явно:

- **70.8** Хранение `tree` blob / content hash + `InputEdit` path при `notify` (у вас уже watcher + `project_files.sha256`).
- **71.5** Name-table / import graph (как hearth-graph / codegraph-rust): file-scope `use`/`import` → definition edges с `provenance=RESOLVED`.
- **71.6** Queries `.scm` versioned per language (не hardcoded walk) — проще расширять.
- **71.7** SFC reuse: Vue/Svelte уже regex+TS; tree-sitter injection для `<script>`.
- **71.8** Binary size budget: grammars as **optional language packs** или `include_str` only top-4; не тащить 18 TS grammars в default.
- **71.9** Fallback policy: parse error → regex partial + `confidence=low`, не silent drop.

Ориентиры: [codegraph-rust](https://github.com/sunerpy/codegraph-rust), [hearth-graph](https://lib.rs/crates/hearth-graph), [cgg](https://lib.rs/crates/cgg) — не копировать, а взять паттерны queries + incremental index.

### 2.6. Dream / SUPERSEDES: опасность «авто-LLM переписывает правду»

Фаза 74 — самая рискованная для trust пользователей.

Обязательные гарды в план:

1. **Три типа конфликтов** (survey graph memory 2026), не newest-wins:
   - temporal supersession;
   - user correction;
   - **contextual coexistence** (оба валидны в разных scope/project).
2. **Не удалять** старый факт: `SUPERSEDES` + `valid_until` / `deleted_at` tombstone semantics.
3. **Human-in-the-loop default** для high-importance / SOUL/USER: `knowledge_reconcile` preview → apply.
4. **Bench-gate**: после dream recall/MRR не хуже baseline −10%/−15% (ADR-13) — у вас уже есть; явно привязать к 74.
5. **Cost budget** tokens/$ на night dream + dry-run mode.
6. **Idempotency**: повторный dream не плодит цепочки SUPERSEDES A→B→A.
7. Фильтр U-PPR: walk **не заходит** в superseded targets (как EngramRAG: 70%→0% split-brain).

### 2.7. Что план почти не трогает (а v2.0 «industrial» без этого хромает)

Ниже — блоки, которых **нет или почти нет** в фазах 70–75.

---

## 3. Что добавить в план (приоритизировано)

### P0 — must-have для честного v2.0

#### A. Quality gates & harness (новая **Фаза 69 / сквозная**)
- Golden: `memory`, `graph`, `ast_rust|py|ts|go`, `ppr_multihop`, `supersedes`.
- `ob2h bench --mode v2` → один JSON-отчёт: recall, MRR, p50/p95, embed ms, parse F1.
- CI: default features + `--features onnx,treesitter` на Linux; Windows treesitter — optional job.
- Запрет merge при регрессии ADR-13.

#### B. Vector path production (**Фаза 72.5**)
- Incremental vec0 + rescore + FTS graph уже как precondition.
- Embed dim/model fingerprint в `kv` (смена MiniLM→bge = rebuild embeddings).

#### C. Cross-file semantic resolve (**расширить 71**)
- Без этого «Expression-level Call Graph» останется intra-file и не окупит tree-sitter.

#### D. SUPERSEDES guards (**расширить 74**)
- См. §2.6; без HITL и coexistence — репутационный риск.

#### E. Access telemetry completeness (**в 73.2**)
- Инкремент `access_count` / `last_accessed` не только в `memory_context`/`graph_reason`, но и:
  - Hermes prefetch path;
  - `project_impact` / `project_call_path` hit nodes;
  - Ralph findings touch.
- Иначе U-PPR «не видит» реальную жизнь агента.

---

### P1 — сильно поднимает ценность, можно v2.0.x

#### F. Multilingual / dual embedding strategy
- Сейчас Candle MiniLM; RU+code mix страдает.
- План: default INT8 **multilingual** (e5-small / bge-m3 lite) **или** dual-space: `text_emb` + `code_emb` (jina-code) для `ast_*` nodes.
- Matryoshka 256-d opt-in для disk.

#### G. Query embed LRU + batch pool
- Упомянуто в v1.9; в v2.0 зафиксировать контракт: process-local LRU 1k–5k, shared with MCP concurrent requests, invalidate on model change.

#### H. Ralph ↔ AST stale invalidation
- v1.9 Ralph есть; v2.0 call-graph delta должен **invalidate** findings при signature/edge change (иначе reuse врёт).

#### I. Sync gaps
- Encrypted bundles (age) для manual mode.
- SUPERSEDES/CONTRADICTS **реплицируются**; AST optionally no (тяжёлый) — как ADR-K6, но явно в v2.0 doc.
- LWW vs field-merge для `trust`/`access_count` (CRDT-lite counters: max or sum-with-origin).

#### J. MCP contract hygiene
- Tool descriptions as prompts + golden-prompt tests.
- Audit JSONL tool calls.
- Read-only profile / allowlist.
- Schema N-1: новые поля optional.
- Remote Streamable HTTP + OAuth 2.1 — **не блокер v2.0 core**, но ADR + stub в 75 или v2.1.

#### K. Observability
- `tracing` spans: `embed`, `parse`, `ppr`, `fts`, `vec`, `llm_dream`.
- `omnes_stats` + optional Prometheus endpoint behind flag (local-only).

#### L. Dream pipeline explicit phases
```
extract → conflict_detect → synthesize(optional) → write SUPERSEDES
→ rewrite MD → git commit → bench → rollback if red
```
Сейчас phases есть; в плане 74 описать state machine + lock (у вас autodream lock уже).

---

### P2 — v2.1 / research, не раздувать 2.0

| Тема | Заметка |
|------|---------|
| Metal/CUDA/DirectML | feature-gated после onnx stable |
| tree-sitter language packs on-demand | download grammar |
| Local NER without LLM | cheap extract |
| HTTP MCP + OAuth 2.1 | production remote |
| Multi-peer mesh sync | beyond PC↔VPS |
| Model2Vec static embeddings | ultra-fast index pass |
| WASM read-only search | edge experiment |
| LoCoMo / LongMemEval subset | literature comparison |
| Community-aware prefetch | communities уже в v1.4 — wire into context |

---

## 4. Уточнения по каждому столпу (патчи текста плана)

### Столп I — AST
**Добавить в To-Be:**
- Incremental reparse on watcher events.
- Provenance everywhere: `EXTRACTED | RESOLVED | INFERRED | AMBIGUOUS` (у вас M8 уже частично).
- `confidence` на edges + filter в `project_impact`.
- Size budget: default binary +X МБ max (например ≤8 МБ grammars).
- Parity test: TS parse vs regex на simple files — regression when TS disabled.

**Убрать/смягчить:** «99%» → measurable F1.

### Столп II — Embeddings
**Добавить:**
- Model card в config: name, dim, quant, metric (cosine).
- Auto-migrate: dim mismatch → background reembed job (reuse scanjob pattern).
- `ob2h bench embeddings` — ок; плюс **quality** probe (pair cosine on RU/EN/code triplets).
- Prefer **ort + quantized onnx** pinned version; document VC++ redist on Windows.
- Thread pool must not starve MCP tokio runtime (separate or `spawn_blocking` + semaphore).

### Столп III — U-PPR
**Добавить:**
- Build on existing `personalized_pagerank`, don’t rewrite.
- Edge weight formula + **node prior** (importance, trust, !superseded).
- Subgraph extraction: seeds ∪ k-hop frontier ∪ high-trust hubs, max 5k–20k nodes.
- Multi-hop golden before claiming “authoritativeness”.
- Integration points: `graph_reason`, `memory_context` RRF third channel, `project_context`.

### Столп IV — Distillation
**Переименовать осторожнее:** не «Knowledge Distillation» (ML term), а **Conflict Resolution & Temporal Supersession**.
**Добавить** coexistence + HITL + bench-gate + cost budget.

### Новый Столп V — Retrieval Integrity (vector + FTS + RRF)
Явно в мотивации: без production ANN/quant path «100k nodes» не держатся на SLA.

### Новый Столп VI — Agent Loop Integrity (Ralph + prefetch + audit)
Связка code delta → memory/findings freshness.

---

## 5. Предлагаемая перестройка фаз

```
Фаза 69  Quality Harness & Golden Sets          (2–3 д)  ← NEW, parallel start
Фаза 70  Tree-sitter Core + packs budget        (5–7 д)
Фаза 71  Call Graph + cross-file resolve + incr (4–6 д)
Фаза 72  FastEmbed/ONNX + model fingerprint     (3–4 д)
Фаза 72b Vector production (vec0 incr, rescore) (2–3 д)  ← NEW
Фаза 73  U-PPR on existing PPR + multi-hop gold (3–4 д)
Фаза 74  SUPERSEDES/CONTRADICTS + HITL + gates  (4–5 д)
Фаза 75  Ralph stale + access telemetry parity  (2 д)    ← NEW thin
Фаза 76  Release: CI matrix, docs, migrate M10  (2–3 д)
```

**Два релизных поезда (рекомендую):**

1. **v2.0.0-rc / v2.0** — 69–72b + 76 partial: *Polyglot AST + Fast Embed + Vector*  
2. **v2.1** — 73–75: *Graph RAG 2.0 + Dream conflicts*  

Так пользователи раньше получают скорость/AST, а рискованный LLM-reconcile идёт вторым вагоном с bench-историей.

---

## 6. Нефункциональные требования — вписать в §1–3 плана

| NFR | Цель v2.0 |
|-----|-----------|
| Cold start MCP | &lt; 100 ms (без ONNX load); ONNX lazy-load first embed |
| memory hybrid p95 | &lt; 50 ms @ 50k memories |
| graph_reason p95 | &lt; 200 ms @ 100k nodes (FTS+vec+U-PPR capped) |
| project_scan 100k LOC | &lt; 30 s first / &lt; 3 s incremental |
| Default binary size | ≤ 25–30 МБ; full features ≤ 60–80 МБ |
| RAM idle | ≤ 40 МБ default; ≤ 150 МБ full+model |
| Privacy | no cloud; model files local cache; sync encrypt opt-in |
| Determinism | same inputs → same AST node_ids / edge set (hash stable) |
| Crash safety | dream/scan mid-kill → consistent DB (WAL + job state) |

---

## 7. Риски и митигации (добавить § в план)

| Риск | Митигация |
|------|-----------|
| Windows treesitter/onnx link hell | feature off by default on MSVC CI; prebuilt |
| Binary bloat | language packs / optional features |
| ONNX model download UX | first-run progressive; embed in release optional |
| False SUPERSEDES | HITL, high threshold, category allowlist |
| U-PPR latency | subgraph cap + seed quality |
| Dim/model change brick DB | fingerprint + reembed job |
| Trust weight regression | already seen −27% recall — ramp 0.05→… with history |
| Regex/TS divergence | dual-run shadow mode week, compare counts |

---

## 8. Конкретные «вставки» в текст фаз (чеклист)

**70.x добавить:**  
70.8 incremental edit API · 70.9 size budget · 70.10 error→regex fallback · 70.11 Windows CI policy  

**71.x добавить:**  
71.5 cross-file resolve · 71.6 `.scm` queries · 71.7 SFC injection · 71.8 blast radius uses virtual dispatch edges · 71.9 golden F1 report  

**72.x добавить:**  
72.5 model fingerprint + reembed · 72.6 quality triplets bench · 72.7 no tokio starve · 72.8 Windows redistributable note  

**NEW 72b:** vec0 incremental, rescore, project partition, recall gate  

**73.x добавить:**  
73.0 multi-hop golden · 73.6 subgraph cap · 73.7 skip superseded · 73.8 node priors · 73.9 third RRF channel in memory · 73.10 extend access hooks (prefetch/Ralph/AST)  

**74.x добавить:**  
74.0 conflict taxonomy (3 types) · 74.5 HITL default high-importance · 74.6 cost budget · 74.7 idempotent DAG · 74.8 bench rollback · 74.9 valid_until / coexistence  

**75/76:** dual feature CI · migration guide dim change · ARCHITECTURE update · SKILL.md · NFR table · ADR-36+ for TS/ONNX/U-PPR/SUPERSEDES  

---

## 9. Итоговая рекомендация

| Действие | Решение |
|----------|---------|
| Утверждать план как есть? | **Нет** — сначала патчи KPI, сроков, vec-фазы, SUPERSEDES guards |
| Scope v2.0 | AST hybrid + ONNX + vector production + harness |
| Scope v2.1 | U-PPR usage + Dream conflicts + Ralph stale |
| Главный пробел | **нет production vector phase** и **нет multi-hop golden** |
| Главный overclaim | 12 дней и 99% AST |
| Главный risk | авто-SUPERSEDES без HITL/coexistence |
| Главный leverage | tree-sitter **incremental + resolve**, не «ещё один parser» |
| Сохранить священным | SQLite-only, default zero-C-deps, ADR-13 bench-gate, local-first |

План после правок станет не «списком фич», а **архитектурным релизом с доказуемым SLA** — в духе уже существующих ADR-13/35.1 и комментария Ф54 в `pagerank.rs`.

---

Могу следующим сообщением оформить это как готовый **`PLAN_v2.0.md` diff-патч** (секции Motivation / Phases 69–76 / NFR / Risks) в стиле вашего репозитория — копипастой в PR.
