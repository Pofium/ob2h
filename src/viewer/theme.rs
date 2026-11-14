//! Палитра viewer в стиле omnes agent (стилистический бриф Ф70): тёмная тема
//! по умолчанию, красный акцент, цветные бейджи типов графа. Чистые данные
//! (`[u8; 3]`), без зависимости от egui — тестируется headless.

/// Цвет как RGB-тройка.
pub type Rgb = [u8; 3];

/// Набор цветов темы (dark — omnes hsl(222 47% …), light — hsl(210 40% 98%)).
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// Фон окна.
    pub bg: Rgb,
    /// Панели/карточки.
    pub panel: Rgb,
    /// Приподнятая карточка.
    pub card: Rgb,
    /// Границы.
    pub border: Rgb,
    /// Основной текст.
    pub text: Rgb,
    /// Приглушённый текст.
    pub muted: Rgb,
    /// Акцент (omnes primary red).
    pub accent: Rgb,
    /// Фон выделения.
    pub selection: Rgb,
    /// Ошибки.
    pub error: Rgb,
    /// Успех.
    pub ok: Rgb,
}

/// Палитра темы: `dark` — omnes dark, иначе light.
pub fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            bg: [8, 13, 23],        // hsl(222 47% 6%)
            panel: [11, 18, 32],    // hsl(222 47% 8%)
            card: [17, 26, 44],     // hsl(222 40% 12%)
            border: [34, 48, 71],   // hsl(217 33% 20%)
            text: [226, 232, 240],  // slate-200
            muted: [148, 163, 184], // hsl(215 20% 65%)
            accent: [223, 51, 51],  // hsl(0 75% 55%)
            selection: [127, 29, 29],
            error: [248, 113, 113],
            ok: [52, 211, 153],
        }
    } else {
        Palette {
            bg: [248, 250, 252],   // hsl(210 40% 98%)
            panel: [255, 255, 255],
            card: [241, 245, 249],
            border: [203, 213, 225],
            text: [15, 23, 42],
            muted: [100, 116, 139],
            accent: [222, 38, 38], // hsl(0 72% 51%)
            selection: [254, 226, 226],
            error: [220, 38, 38],
            ok: [5, 150, 105],
        }
    }
}

/// Цвет типа узла графа (омnes GraphView.typeColors + AST-типы ob2h).
pub fn type_color(node_type: &str) -> Rgb {
    match node_type {
        "Person" => [244, 63, 94],      // #f43f5e
        "Work" => [139, 92, 246],       // #8b5cf6
        "Location" => [16, 185, 129],   // #10b981
        "Event" => [245, 158, 11],      // #f59e0b
        "Platform" => [59, 130, 246],   // #3b82f6
        "Service" => [99, 102, 241],    // #6366f1
        "Technology" => [6, 182, 212],  // #06b6d4
        "Cloud" => [236, 72, 153],      // #ec4899
        "Artifact" => [249, 115, 22],   // #f97316
        "Concept" => [20, 184, 166],    // #14b8a6
        "Organization" => [236, 72, 153],
        // AST-типы кодового графа (project/ast.rs)
        "function" | "method" => [59, 130, 246],
        "struct" | "class" | "enum" | "trait" | "interface" => [139, 92, 246],
        "module" | "package" => [16, 185, 129],
        "file" => [245, 158, 11],
        "variable" | "const" | "field" => [20, 184, 166],
        _ => [148, 163, 184], // Other/default — slate-400
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_and_light_differ() {
        let d = palette(true);
        let l = palette(false);
        assert_ne!(d.bg, l.bg);
        assert_ne!(d.text, l.text);
        // Акцент — красный в обеих темах (доминанта красного канала).
        assert!(d.accent[0] > d.accent[1] && d.accent[0] > d.accent[2]);
        assert!(l.accent[0] > l.accent[1] && l.accent[0] > l.accent[2]);
    }

    #[test]
    fn known_types_have_distinct_colors_unknown_falls_back() {
        assert_eq!(type_color("Person"), [244, 63, 94]);
        assert_eq!(type_color("Technology"), [6, 182, 212]);
        assert_ne!(type_color("Person"), type_color("Location"));
        assert_eq!(type_color("что-то неизвестное"), [148, 163, 184]);
        assert_eq!(type_color("function"), type_color("method"));
    }
}