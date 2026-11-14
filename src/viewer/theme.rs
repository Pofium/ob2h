//! Палитра viewer по брендбуку семейства Omnes (OmnesProducts/brandbook.html):
//! Dark Charcoal #111827 (~60%), Off-White #F4F5F7 (~30%), Brand Orange #FF6B00
//! (~10%, сигнальный). Плоские формы, без градиентов. Чистые данные (`[u8; 3]`),
//! без зависимости от egui — тестируется headless.

/// Цвет как RGB-тройка.
pub type Rgb = [u8; 3];

/// Набор цветов темы (dark — charcoal-поверхности, light — off-white).
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
    /// Приглушённый текст (контраст ≥ 4.5 к фону).
    pub muted: Rgb,
    /// Акцент (Brand Orange; в light — затемнённый для контраста текста).
    pub accent: Rgb,
    /// Текст поверх акцентной заливки.
    pub accent_fg: Rgb,
    /// Фон выделения.
    pub selection: Rgb,
    /// Ошибки (семантический красный брендбука).
    pub error: Rgb,
    /// Успех (семантический зелёный брендбука).
    pub ok: Rgb,
}

/// Палитра темы: `dark` — charcoal, иначе off-white.
pub fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            bg: [17, 24, 39],        // #111827 Dark Charcoal
            panel: [22, 31, 48],     // charcoal +1 ступень
            card: [27, 37, 55],      // charcoal +2 ступени
            border: [42, 54, 72],
            text: [244, 245, 247],   // #F4F5F7 Off-White
            muted: [154, 163, 178],  // #9AA3B2 (брендбук)
            accent: [255, 107, 0],   // #FF6B00 Brand Orange
            accent_fg: [17, 24, 39], // charcoal поверх оранжевого
            selection: [64, 40, 31], // orange 20% поверх charcoal
            error: [240, 140, 130],  // светлый вариант #C0392B для тёмного фона
            ok: [76, 195, 138],      // светлый вариант #1B7F3B
        }
    } else {
        Palette {
            bg: [244, 245, 247],     // #F4F5F7 Off-White
            panel: [255, 255, 255],
            card: [236, 238, 242],
            border: [216, 220, 227], // #D8DCE3 (брендбук)
            text: [17, 24, 39],      // #111827
            muted: [91, 100, 114],   // #5B6472 (брендбук)
            accent: [194, 84, 0],    // orange, затемнённый до контраста ≥ 4.5
            accent_fg: [255, 255, 255],
            selection: [255, 224, 204], // orange-тинт поверх off-white
            error: [192, 57, 43],    // #C0392B
            ok: [27, 127, 59],       // #1B7F3B
        }
    }
}

/// Цвет типа узла графа (data-viz палитра; бренд-оран reserved для сигналов UI).
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
        // AST-типы кодового графа (project/ast.rs) — PascalCase и lowercase.
        "function" | "method" | "Function" | "Method" => [59, 130, 246],
        "struct" | "class" | "enum" | "trait" | "interface"
        | "Struct" | "Class" | "Enum" | "Trait" | "Interface" => [139, 92, 246],
        "module" | "package" | "Module" | "Package"
        | "ExternalModule" | "ExternalPackage" => [16, 185, 129],
        "file" | "File" => [245, 158, 11],
        "variable" | "const" | "field" | "Variable" | "Const" | "Field"
        | "Mapping" => [20, 184, 166],
        "table" | "Table" | "schema" | "Schema" => [6, 182, 212],
        "element" | "Element" | "CssClass" | "CssId" | "CssVar"
        | "Notebook" => [236, 72, 153],
        _ => [154, 163, 178], // Other/default — брендовый muted #9AA3B2
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
        // Акцент — оранжевый в обеих темах (R > G > B, как #FF6B00).
        for p in [d, l] {
            assert!(p.accent[0] > p.accent[1] && p.accent[1] > p.accent[2]);
        }
    }

    #[test]
    fn brand_hex_exact() {
        let d = palette(true);
        let l = palette(false);
        assert_eq!(d.bg, [17, 24, 39]); // #111827
        assert_eq!(d.text, [244, 245, 247]); // #F4F5F7
        assert_eq!(d.accent, [255, 107, 0]); // #FF6B00
        assert_eq!(l.bg, [244, 245, 247]);
        assert_eq!(l.text, [17, 24, 39]);
        // Текст поверх акцента контрастен самой заливке.
        assert_ne!(d.accent, d.accent_fg);
        assert_ne!(l.accent, l.accent_fg);
    }

    #[test]
    fn known_types_have_distinct_colors_unknown_falls_back() {
        assert_eq!(type_color("Person"), [244, 63, 94]);
        assert_eq!(type_color("Technology"), [6, 182, 212]);
        assert_ne!(type_color("Person"), type_color("Location"));
        assert_eq!(type_color("что-то неизвестное"), [154, 163, 178]);
        assert_eq!(type_color("function"), type_color("method"));
        // PascalCase AST-типы (project/ast.rs) не должны падать в серый дефолт.
        assert_eq!(type_color("File"), [245, 158, 11]);
        assert_eq!(type_color("Function"), type_color("function"));
        assert_eq!(type_color("ExternalModule"), [16, 185, 129]);
        assert_eq!(type_color("Struct"), type_color("class"));
    }
}