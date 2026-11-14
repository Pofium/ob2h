//! Бренд семейства Omnes (OmnesProducts/brandbook.html): логотип ob2h и
//! типографика. Ассет — растрированный `brand/ob2h.svg` (слон в угольном круге,
//! оранжевый промпт `>_`), встроен в бинарник. Декод PNG и шрифты — только gui;
//! headless видит байты ассета (тест целостности).

/// PNG-логотип ob2h (815×815, RGBA). Источник: OmnesProducts/render/crops.
pub const LOGO_PNG: &[u8] = include_bytes!("assets/ob2h.png");

/// Сигнатура PNG — headless-проверка целостности встроенного ассета.
pub fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])
}

#[cfg(feature = "gui")]
mod gui {
    use eframe::egui;

    /// Логотип для шапки (128×128 — с запасом для HiDPI при 28 px).
    pub fn logo_image() -> Option<egui::ColorImage> {
        static IMG: std::sync::OnceLock<Option<egui::ColorImage>> = std::sync::OnceLock::new();
        IMG.get_or_init(|| {
            let img = image::load_from_memory(super::LOGO_PNG).ok()?.to_rgba8();
            let small = image::imageops::resize(&img, 128, 128, image::imageops::FilterType::Lanczos3);
            Some(egui::ColorImage::from_rgba_unmultiplied(
                [128, 128],
                small.as_raw(),
            ))
        })
        .clone()
    }

    /// Иконка окна (заголовок/taskbar), 256×256.
    pub fn window_icon() -> Option<egui::IconData> {
        let img = image::load_from_memory(super::LOGO_PNG).ok()?.to_rgba8();
        let icon = image::imageops::resize(&img, 256, 256, image::imageops::FilterType::Lanczos3);
        Some(egui::IconData {
            rgba: icon.into_raw(),
            width: 256,
            height: 256,
        })
    }

    /// Шрифты брендбука: Segoe UI (текст/UI), Segoe UI Bold (заголовки),
    /// Consolas (код/терминал), Segoe UI Symbol (моно-глифы кнопок).
    /// Только локальное чтение системных шрифтов Windows; без них — дефолты egui.
    pub fn install_fonts(ctx: &egui::Context) {
        let Some(root) = std::env::var_os("SystemRoot") else { return };
        let dir = std::path::PathBuf::from(root).join("Fonts");
        let read = |name: &str| std::fs::read(dir.join(name)).ok();
        let Some(regular) = read("segoeui.ttf") else { return };
        let bold = read("segoeuib.ttf");
        let mono = read("consola.ttf");
        let symbol = read("seguisym.ttf");

        let mut defs = egui::FontDefinitions::default();
        defs.font_data.insert(
            "SegoeUI".into(),
            std::sync::Arc::new(egui::FontData::from_owned(regular)),
        );
        if let Some(b) = &bold {
            defs.font_data.insert(
                "SegoeUI-Bold".into(),
                std::sync::Arc::new(egui::FontData::from_owned(b.clone())),
            );
        }
        if let Some(m) = &mono {
            defs.font_data.insert(
                "Consolas".into(),
                std::sync::Arc::new(egui::FontData::from_owned(m.clone())),
            );
        }
        if let Some(s) = &symbol {
            defs.font_data.insert(
                "SegoeUISymbol".into(),
                std::sync::Arc::new(egui::FontData::from_owned(s.clone())),
            );
        }

        let prop = defs.families.entry(egui::FontFamily::Proportional).or_default();
        prop.insert(0, "SegoeUI".into());
        if symbol.is_some() {
            prop.push("SegoeUISymbol".into());
        }
        if mono.is_some() {
            let mono_f = defs.families.entry(egui::FontFamily::Monospace).or_default();
            mono_f.insert(0, "Consolas".into());
            if symbol.is_some() {
                mono_f.push("SegoeUISymbol".into());
            }
        }
        let has_bold = bold.is_some();
        if has_bold {
            // Заголовки — полужирный гротеск (брендбук: SemiBold/Bold).
            defs.families.insert(
                egui::FontFamily::Name("BrandSemibold".into()),
                vec!["SegoeUI-Bold".into(), "SegoeUI".into()],
            );
        }
        ctx.set_fonts(defs);
        if has_bold {
            for theme in [egui::Theme::Dark, egui::Theme::Light] {
                ctx.style_mut_of(theme, |s| {
                    s.text_styles.insert(
                        egui::TextStyle::Heading,
                        egui::FontId::new(14.0, egui::FontFamily::Name("BrandSemibold".into())),
                    );
                });
            }
        }
    }
}

#[cfg(feature = "gui")]
pub use gui::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_logo_is_valid_png() {
        assert!(is_png(LOGO_PNG));
        assert!(LOGO_PNG.len() > 1000);
        assert!(!is_png(b"not a png"));
    }
}