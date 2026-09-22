use gpui::{px, rgb, Hsla, Pixels, Rgba, WindowAppearance};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use van_goal_core::settings::{AppearanceMode, FontSize};

static LIGHT_THEME: AtomicBool = AtomicBool::new(false);

/// App-wide multiplier for every font size, held as `f32` bits because atomics
/// have no float variant. Written at the top of each window's render from the
/// saved preference, the same way the appearance above is, so that every view
/// reads one scale without threading it through the element tree.
static FONT_SCALE: AtomicU32 = AtomicU32::new(1.0f32.to_bits());

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

    /// Apply the saved text-size preference. Called from the same render entry
    /// points as [`Theme::sync`], right beside it, so a change in Settings
    /// reaches both windows on their next frame.
    pub fn sync_font_size(font_size: FontSize) {
        FONT_SCALE.store(font_size.scale().to_bits(), Ordering::Relaxed);
    }

    pub fn font_scale() -> f32 {
        f32::from_bits(FONT_SCALE.load(Ordering::Relaxed))
    }

    /// A design-time font size, scaled by the user's text-size preference.
    ///
    /// Every `text_size` in the interface goes through here rather than taking
    /// a literal, so the whole app grows together. Sizes are still written as
    /// the numbers they were designed at, which keeps the call sites readable.
    pub fn text_px(value: f32) -> Pixels {
        px(value * Self::font_scale())
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
    /// The highlight under selected message text. Same blue the accent uses,
    /// translucent, so selected glyphs stay readable on every fill they sit on.
    pub fn selection() -> Hsla {
        rgba_hex(if Self::is_light() { 0x276df1 } else { 0x4f8cff }, 0.40)
    }
    /// The same selection once its field lost focus: dimmer and grayer, so a
    /// retained selection stays visible without competing with the active one.
    pub fn selection_unfocused() -> Hsla {
        rgba_hex(if Self::is_light() { 0x276df1 } else { 0x9aa4b2 }, 0.24)
    }
    pub fn scrollbar_thumb_active() -> Hsla {
        rgba_hex(if Self::is_light() { 0x1c1c21 } else { 0xffffff }, 0.45)
    }
    pub fn quote_bar() -> Hsla {
        rgba_hex(if Self::is_light() { 0x000000 } else { 0xffffff }, 0.25)
    }

    /// Label colour that stays readable on a given fill. The accent and danger
    /// fills are bright enough for a dark label; the neutral surfaces are not.
    /// Comparing lightness beats comparing the fill for equality, which breaks
    /// silently the moment a colour is redefined.
    pub fn label_on(fill: Hsla) -> Hsla {
        if fill.l > 0.5 {
            gpui::black()
        } else {
            Self::text()
        }
    }
}

pub fn rgba_hex(value: u32, alpha: f32) -> Hsla {
    Rgba {
        r: ((value >> 16) & 0xff) as f32 / 255.0,
        g: ((value >> 8) & 0xff) as f32 / 255.0,
        b: (value & 0xff) as f32 / 255.0,
        a: alpha.clamp(0.0, 1.0),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::{rgba_hex, Theme};
    use gpui::{Hsla, Rgba, WindowAppearance};

    #[test]
    fn translucent_hex_colors_are_converted_from_rgb_not_treated_as_hsl() {
        let color: Rgba = rgba_hex(0x276df1, 0.4).into();
        assert!((color.r - 0x27 as f32 / 255.0).abs() < 0.001);
        assert!((color.g - 0x6d as f32 / 255.0).abs() < 0.001);
        assert!((color.b - 0xf1 as f32 / 255.0).abs() < 0.001);
        assert!((color.a - 0.4).abs() < 0.001);
    }
    use van_goal_core::settings::AppearanceMode;

    /// A label that does not contrast with its fill renders as a blank pill,
    /// which is exactly how the session confirmation lost its button labels.
    fn assert_contrast(fill: Hsla) {
        let label = Theme::label_on(fill);
        assert!(
            (label.l - fill.l).abs() > 0.3,
            "label (l={}) is too close to its fill (l={})",
            label.l,
            fill.l
        );
    }

    #[test]
    fn button_labels_contrast_with_every_fill_they_use() {
        for mode in [AppearanceMode::Dark, AppearanceMode::Light] {
            Theme::sync(
                mode,
                match mode {
                    AppearanceMode::Light => WindowAppearance::Light,
                    _ => WindowAppearance::Dark,
                },
            );
            // The fills the buttons actually use in this appearance.
            for fill in [
                Theme::danger(),
                Theme::accent(),
                Theme::surface_hover(),
                Theme::surface(),
            ] {
                assert_contrast(fill);
            }
        }
    }
}
