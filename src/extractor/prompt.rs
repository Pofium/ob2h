//! Промпты, стоп-слова и шаблоны инференса связей.

pub const EXTRACTION_SYSTEM_PROMPT: &str = "\
Ты — экстрактор сущностей и отношений для графа знаний. Извлеки из текста \
сущности и отношения между ними. Верни СТРОГО JSON без markdown-обёрток:
{\"entities\": [{\"id\": \"e1\", \"label\": \"Имя\", \"type\": \"Person\", \"description\": \"кратко\"}],
 \"relations\": [{\"source\": \"e1\", \"target\": \"e2\", \"label\": \"works_at\", \"contexts\": [\"фраза\"]}]}
Типы: Person | Organization | Location | Event | Concept | Artifact | Other.
Правила: label — точное имя собственное или термин; отношение связывает только \
извлечённые сущности; label отношения — английский snake_case (works_at, father_of, \
located_in, part_of, created, manages, uses, related_to...); contexts — фразы-доказательства. \
Нет сущностей — пустые списки. Только JSON.";

pub const VALID_TYPES: &[&str] = &[
    "Person", "Organization", "Location", "Event", "Concept", "Artifact", "Other",
];

pub const JUNK_LABELS: &[&str] = &[
    "который", "которая", "которое", "которые", "этот", "эта", "это", "эти",
    "также", "где", "что", "как", "или", "для", "при", "все", "весь", "вся",
    "однако", "кроме", "только", "очень", "когда", "тогда", "такой", "такая",
    "it", "the", "this", "that", "and", "for",
];

pub const INFERENCE_PATTERNS: &[(&str, &str)] = &[
    ("отец", "father_of"), ("мать", "mother_of"), ("сын", "son_of"),
    ("дочь", "daughter_of"), ("брат", "brother_of"), ("сестра", "sister_of"),
    ("муж", "husband_of"), ("жена", "wife_of"),
    ("работает в", "works_at"), ("работает на", "works_at"),
    ("работал в", "worked_at"), ("сотрудник", "employee_of"),
    ("руководит", "manages"), ("директор", "directs"), ("возглавляет", "heads"),
    ("основал", "founded"), ("основала", "founded"), ("основатель", "founded"),
    ("входит в", "part_of"), ("является частью", "part_of"),
    ("расположен в", "located_in"), ("находится в", "located_in"),
    ("столица", "capital_of"), ("город", "city_of"),
    ("создал", "created"), ("разработал", "developed"), ("написал", "wrote"),
    ("использует", "uses"), ("применяет", "uses"),
    ("партнёр", "partner_of"), ("сотрудничает", "collaborates_with"),
    ("конкурент", "competitor_of"), ("владеет", "owns"),
    ("произошёл в", "occurred_in"), ("произошла в", "occurred_in"),
    ("состоялся в", "held_in"), ("начался в", "started_in"),
    ("связан с", "related_to"), ("относится к", "related_to"),
    ("включает", "includes"), ("содержит", "contains"),
    ("производит", "produces"), ("выпускает", "produces"),
    ("установлен в", "installed_in"), ("выполняет", "performs"),
];
