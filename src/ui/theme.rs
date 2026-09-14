use crate::settings::AppearanceMode;
use gpui::{hsla, rgb, Hsla, WindowAppearance};
use std::sync::atomic::{AtomicBool, Ordering};

static LIGHT_THEME: AtomicBool = AtomicBool::new(false);

/// App-wide adaptive palette. The active variant is synchronized at the start
/// of each window render so child views can keep using the compact Theme API.
pub struct Theme;

impl Theme {
    pub fn sync(preference: AppearanceMode, system: WindowAppearance) {
        let light = match preference {
            AppearanceMode::System => {
                matches!(
                    system,
                    WindowAppearance::Light | WindowAppearance::VibrantLight
                )
            }
            AppearanceMode::Light => true,
            AppearanceMode::Dark => false,
        };
        LIGHT_THEME.store(light, Ordering::Relaxed);
    }

    fn is_light() -> bool {
        LIGHT_THEME.load(Ordering::Relaxed)
    }

    pub fn window_bg() -> Hsla {
        rgb(if Self::is_light() { 0xf7f7f9 } else { 0x141416 }).into()
    }
    pub fn sidebar_bg() -> Hsla {
        rgb(if Self::is_light() { 0xefeff3 } else { 0x1a1a1e }).into()
    }
    pub fn surface() -> Hsla {
        rgb(if Self::is_light() { 0xffffff } else { 0x202024 }).into()
    }
    pub fn surface_hover() -> Hsla {
        rgb(if Self::is_light() { 0xe8eaf0 } else { 0x28282e }).into()
    }
    pub fn input_bg() -> Hsla {
        rgb(if Self::is_light() { 0xf3f3f6 } else { 0x1c1c21 }).into()
    }
    pub fn border() -> Hsla {
        rgb(if Self::is_light() { 0xd9d9e0 } else { 0x2c2c33 }).into()
    }
    pub fn border_strong() -> Hsla {
        rgb(if Self::is_light() { 0xc4c4ce } else { 0x3a3a44 }).into()
    }
    pub fn text() -> Hsla {
        rgb(if Self::is_light() { 0x202126 } else { 0xe8e8ec }).into()
    }
    pub fn text_secondary() -> Hsla {
        rgb(if Self::is_light() { 0x5f616b } else { 0x9b9ba6 }).into()
    }
    pub fn text_tertiary() -> Hsla {
        rgb(if Self::is_light() { 0x858894 } else { 0x6d6d78 }).into()
    }
    pub fn accent() -> Hsla {
        rgb(if Self::is_light() { 0x276df1 } else { 0x4f8cff }).into()
    }
    pub fn accent_soft() -> Hsla {
        rgba_hex(
            if Self::is_light() { 0x276df1 } else { 0x4f8cff },
            if Self::is_light() { 0.12 } else { 0.16 },
        )
    }
    pub fn user_bubble() -> Hsla {
        rgb(if Self::is_light() { 0xdce8ff } else { 0x2c3b58 }).into()
    }
    pub fn tool_bg() -> Hsla {
        rgba_hex(if Self::is_light() { 0x000000 } else { 0xffffff }, 0.05)
    }
    pub fn clarify_bg() -> Hsla {
        rgba_hex(if Self::is_light() { 0x276df1 } else { 0x4f8cff }, 0.10)
    }
    pub fn danger() -> Hsla {
        rgb(if Self::is_light() { 0xd93f38 } else { 0xff5f57 }).into()
    }
    pub fn warn() -> Hsla {
        rgb(if Self::is_light() { 0xb86600 } else { 0xf5a623 }).into()
    }
    pub fn ok() -> Hsla {
        rgb(if Self::is_light() { 0x21883c } else { 0x34c759 }).into()
    }
    pub fn code_bg() -> Hsla {
        rgb(if Self::is_light() { 0xedeef2 } else { 0x1b1b20 }).into()
    }
    /// Overlay scrollbar thumb (drawn on top of the content, fades out).
    pub fn scrollbar_thumb() -> Hsla {
        rgba_hex(if Self::is_light() { 0x1c1c21 } else { 0xffffff }, 0.30)
    }
    pub fn scrollbar_thumb_active() -> Hsla {
        rgba_hex(if Self::is_light() { 0x1c1c21 } else { 0xffffff }, 0.45)
    }
    pub fn quote_bar() -> Hsla {
        rgba_hex(if Self::is_light() { 0x000000 } else { 0xffffff }, 0.25)
    }
}

pub fn rgba_hex(value: u32, alpha: f32) -> Hsla {
    let r = ((value >> 16) & 0xff) as f32 / 255.0;
    let g = ((value >> 8) & 0xff) as f32 / 255.0;
    let b = (value & 0xff) as f32 / 255.0;
    hsla(r, g, b, alpha)
}
