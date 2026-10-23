//! Aura presentation in EvertyUI logical coordinates.
use aura_core::{Action, Aura, Presence};
use everty_ui::{
    Color, FontWeight, IconId, LogicalPoint, LogicalPx, LogicalRect, PaintBuilder, PaintList,
    TextStyle, TextSystem,
};
use std::sync::Arc;

const BG: Color = Color::rgba(19, 20, 26, 255);
const PANEL: Color = Color::rgba(29, 30, 39, 255);
const LINE: Color = Color::rgba(47, 48, 61, 255);
const TEXT: Color = Color::rgba(237, 236, 245, 255);
const MUTED: Color = Color::rgba(145, 146, 163, 255);
const ACCENT: Color = Color::rgba(185, 166, 249, 255);
const GREEN: Color = Color::rgba(137, 210, 174, 255);
const DANGER: Color = Color::rgba(240, 151, 164, 255);

#[derive(Clone, Copy)]
#[repr(u32)]
enum AuraIcon {
    Settings = 1,
    Check,
    Skip,
    Music,
    Star,
    StarFilled,
    Minus,
    Pause,
    Play,
    More,
    Copy,
    Share,
    Close,
    Pencil,
    Volume,
    Up,
    Down,
    Search,
}

/// Register the small subset of Tabler Icons used by Aura's native UI.
pub fn register_icons(renderer: &mut everty_ui::Rgba8Renderer) -> Result<(), &'static str> {
    for (id, svg) in [
        (
            AuraIcon::Search,
            include_str!("../assets/tabler/search.svg"),
        ),
        (
            AuraIcon::Up,
            include_str!("../assets/tabler/chevron-up.svg"),
        ),
        (
            AuraIcon::Down,
            include_str!("../assets/tabler/chevron-down.svg"),
        ),
        (
            AuraIcon::Settings,
            include_str!("../assets/tabler/settings.svg"),
        ),
        (AuraIcon::Check, include_str!("../assets/tabler/check.svg")),
        (
            AuraIcon::Skip,
            include_str!("../assets/tabler/player-skip-forward.svg"),
        ),
        (AuraIcon::Music, include_str!("../assets/tabler/music.svg")),
        (AuraIcon::Star, include_str!("../assets/tabler/star.svg")),
        (
            AuraIcon::StarFilled,
            include_str!("../assets/tabler/star-filled.svg"),
        ),
        (AuraIcon::Minus, include_str!("../assets/tabler/minus.svg")),
        (
            AuraIcon::Pause,
            include_str!("../assets/tabler/player-pause.svg"),
        ),
        (
            AuraIcon::Play,
            include_str!("../assets/tabler/player-play.svg"),
        ),
        (
            AuraIcon::More,
            include_str!("../assets/tabler/loader-2.svg"),
        ),
        (AuraIcon::Copy, include_str!("../assets/tabler/copy.svg")),
        (AuraIcon::Share, include_str!("../assets/tabler/share.svg")),
        (AuraIcon::Close, include_str!("../assets/tabler/x.svg")),
        (
            AuraIcon::Pencil,
            include_str!("../assets/tabler/pencil.svg"),
        ),
        (
            AuraIcon::Volume,
            include_str!("../assets/tabler/volume-2.svg"),
        ),
    ] {
        renderer.register_svg_icon(IconId(id as u32), svg)?;
    }
    Ok(())
}

fn symbol_icon(symbol: &str) -> Option<AuraIcon> {
    Some(match symbol {
        "search" => AuraIcon::Search,
        "⚙" => AuraIcon::Settings,
        "✓" => AuraIcon::Check,
        "⏭" => AuraIcon::Skip,
        "♪" => AuraIcon::Music,
        "☆" => AuraIcon::Star,
        "★" => AuraIcon::StarFilled,
        "−" | "–" | "-" => AuraIcon::Minus,
        "Ⅱ" => AuraIcon::Pause,
        "⏵" | "▶" => AuraIcon::Play,
        "…" => AuraIcon::More,
        "⧉" => AuraIcon::Copy,
        "↗" => AuraIcon::Share,
        "×" | "✕" => AuraIcon::Close,
        "✎" => AuraIcon::Pencil,
        "up" => AuraIcon::Up,
        "down" => AuraIcon::Down,
        _ => return None,
    })
}

#[derive(Clone, Debug)]
pub struct Control {
    pub rect: LogicalRect,
    pub action: Action,
    pub label: String,
}
pub struct Frame {
    pub paint: PaintList,
    pub controls: Vec<Control>,
    pub editor_scroll: Option<f64>,
}
pub fn editor_rect(width: f64) -> LogicalRect {
    LogicalRect::from_xywh(32.0, 421.0, width - 64.0, 48.0)
}
pub fn editor_area(width: f64, editor: &aura_core::live::Editor) -> LogicalRect {
    if editor.purpose == aura_core::live::EditPurpose::PublicSearch {
        LogicalRect::from_xywh(20.0, 463.0, width - 40.0, 34.0)
    } else {
        editor_rect(width)
    }
}
pub fn public_page_size(height: f64, search: bool) -> usize {
    ((height - 92.0 - if search { 509.0 } else { 466.0 }) / 52.0)
        .floor()
        .max(1.0) as usize
}
pub fn editor_model(editor: &aura_core::live::Editor) -> everty_ui::TextEdit {
    let mut edit = everty_ui::TextEdit::new(match editor.purpose {
        aura_core::live::EditPurpose::PublicTitle | aura_core::live::EditPurpose::PublicSearch => {
            80
        }
        _ => {
            if editor.name {
                48
            } else {
                150
            }
        }
    });
    edit.set_text(&editor.value);
    if let Some(anchor) = editor.anchor {
        edit.set_caret(anchor, false);
        edit.set_caret(editor.caret, true);
    } else {
        edit.set_caret(editor.caret, false);
    }
    edit
}
pub fn editor_theme() -> everty_ui::Theme {
    let mut theme = everty_ui::Theme::default();
    theme.colors.window = BG;
    theme.colors.text_primary = TEXT;
    theme.colors.text_disabled = MUTED;
    theme.colors.focus = ACCENT;
    theme.colors.accent = ACCENT;
    theme.colors.border = LINE;
    theme.colors.selection = Color::rgba(89, 73, 137, 255);
    theme
}
pub fn update_editor(editor: &mut aura_core::live::Editor, edit: &everty_ui::TextEdit) {
    editor.value = edit.text().into();
    editor.caret = edit.caret();
    editor.anchor = edit
        .selection()
        .map(|(start, end)| if edit.caret() == end { start } else { end });
}

struct Canvas<'a> {
    p: PaintBuilder,
    t: &'a mut TextSystem,
    controls: Vec<Control>,
    focus: Option<usize>,
    hover: Option<LogicalPoint>,
}
impl Canvas<'_> {
    fn tooltip(&mut self, label: Option<&str>, width: f64, height: f64) {
        let (Some(label), Some(point)) = (label, self.hover) else {
            return;
        };
        if !self
            .controls
            .iter()
            .any(|control| control.label == label && control.rect.contains(point))
        {
            return;
        }
        let style = TextStyle {
            font_size: LogicalPx(11.0),
            line_height: LogicalPx(16.0),
            ..TextStyle::default()
        };
        let layout =
            Arc::new(
                self.t
                    .shape_clamped(label, &style, LogicalPx((width - 48.0).min(240.0)), 5),
            );
        let w = layout.metrics.size.width.0 + 24.0;
        let h = layout.metrics.size.height.0 + 18.0;
        let x = (point.x.0 - w / 2.0).clamp(12.0, (width - w - 12.0).max(12.0));
        let y = if point.y.0 > h + 24.0 {
            point.y.0 - h - 14.0
        } else {
            point.y.0 + 24.0
        };
        let y = y.min((height - h - 12.0).max(12.0));
        self.p.set_layer(everty_ui::PaintLayer::Tooltip);
        self.box_(x, y + 3.0, w, h, Color::rgba(0, 0, 0, 100), 9.0);
        self.box_(x, y, w, h, Color::rgba(42, 43, 55, 255), 9.0);
        self.p
            .stroke_rect(LogicalRect::from_xywh(x, y, w, h), LINE, 1.0, 9.0);
        self.p
            .text(LogicalPoint::new(x + 12.0, y + 9.0), layout, TEXT);
    }
    #[allow(clippy::too_many_arguments)]
    fn wrapped(
        &mut self,
        x: f64,
        y: f64,
        w: f64,
        text: &str,
        size: f64,
        color: Color,
        lines: usize,
    ) {
        let style = TextStyle {
            font_size: LogicalPx(size),
            line_height: LogicalPx(size * 1.4),
            ..TextStyle::default()
        };
        let layout = Arc::new(self.t.shape_clamped(text, &style, LogicalPx(w), lines));
        self.p.text(LogicalPoint::new(x, y), layout, color);
    }
    fn box_(&mut self, x: f64, y: f64, w: f64, h: f64, color: Color, radius: f64) {
        self.p
            .fill_rect(LogicalRect::from_xywh(x, y, w, h), color, radius);
    }
    fn icon(&mut self, icon: AuraIcon, rect: LogicalRect, tint: Color) {
        self.p.icon(IconId(icon as u32), rect, tint);
    }
    fn text(&mut self, x: f64, y: f64, text: &str, size: f64, color: Color, bold: bool) {
        let style = TextStyle {
            font_size: LogicalPx(size),
            line_height: LogicalPx(size * 1.4),
            weight: if bold {
                FontWeight::Semibold
            } else {
                FontWeight::Regular
            },
            ..TextStyle::default()
        };
        let layout = Arc::new(self.t.shape(text, &style, None));
        self.p.text(LogicalPoint::new(x, y), layout, color);
    }
    fn button(&mut self, rect: [f64; 4], label: &str, action: Action, primary: bool) {
        self.button_display(rect, label, label, action, primary);
    }
    fn icon_button(&mut self, rect: [f64; 4], icon: &str, label: &str, action: Action) {
        self.button_display(rect, icon, label, action, false);
    }
    fn state_icon_button(
        &mut self,
        rect: [f64; 4],
        icon: &str,
        label: &str,
        action: Action,
        active: bool,
    ) {
        self.button_display(rect, icon, label, action, active);
    }
    fn disabled_icon(&mut self, rect: [f64; 4], icon: &str) {
        let [x, y, w, h] = rect;
        self.box_(x, y, w, h, PANEL, 9.0);
        if let Some(icon) = symbol_icon(icon) {
            self.icon(
                icon,
                LogicalRect::from_xywh(x + (w - 18.0) / 2.0, y + (h - 18.0) / 2.0, 18.0, 18.0),
                MUTED,
            );
            return;
        }
        let style = TextStyle {
            font_size: LogicalPx(19.0),
            line_height: LogicalPx(23.0),
            weight: FontWeight::Semibold,
            ..TextStyle::default()
        };
        let layout = Arc::new(self.t.shape(icon, &style, None));
        let tx = x + (w - layout.metrics.size.width.0) / 2.0;
        self.p
            .text(LogicalPoint::new(tx, y + (h - 23.0) / 2.0), layout, MUTED);
    }
    fn checkbox(&mut self, rect: [f64; 4], checked: bool, label: &str, action: Action) {
        let [x, y, w, h] = rect;
        let area = LogicalRect::from_xywh(x, y, w, h);
        if checked {
            self.box_(x, y, w, h, ACCENT, 8.0);
            self.icon(
                AuraIcon::Check,
                LogicalRect::from_xywh(x + (w - 15.0) / 2.0, y + (h - 15.0) / 2.0, 15.0, 15.0),
                BG,
            );
        } else {
            self.box_(x, y, w, h, PANEL, 8.0);
            self.p.stroke_rect(area, LINE, 1.5, 8.0);
        }
        self.controls.push(Control {
            rect: area,
            action,
            label: label.into(),
        });
    }
    fn button_display(
        &mut self,
        rect: [f64; 4],
        display: &str,
        label: &str,
        action: Action,
        primary: bool,
    ) {
        let [x, y, w, h] = rect;
        let area = LogicalRect::from_xywh(x, y, w, h);
        let active =
            self.focus == Some(self.controls.len()) || self.hover.is_some_and(|p| area.contains(p));
        let color = if primary && action == Action::FriendRemoveConfirm {
            DANGER
        } else if primary {
            ACCENT
        } else if active {
            LINE
        } else {
            PANEL
        };
        self.box_(x, y, w, h, color, 10.0);
        if self.focus == Some(self.controls.len()) {
            self.p.stroke_rect(area, ACCENT, 1.5, 10.0);
        }
        let is_icon = symbol_icon(display).is_some();
        if is_icon {
            if let Some(icon) = symbol_icon(display) {
                self.icon(
                    icon,
                    LogicalRect::from_xywh(x + (w - 18.0) / 2.0, y + (h - 18.0) / 2.0, 18.0, 18.0),
                    if primary { BG } else { TEXT },
                );
                self.controls.push(Control {
                    rect: area,
                    action,
                    label: label.into(),
                });
                return;
            }
        }
        let line_height = if is_icon { 21.0 } else { 19.0 };
        let style = TextStyle {
            font_size: LogicalPx(if is_icon { 19.0 } else { 13.0 }),
            line_height: LogicalPx(if is_icon { 23.0 } else { line_height }),
            weight: FontWeight::Semibold,
            ..TextStyle::default()
        };
        let layout = Arc::new(self.t.shape(display, &style, None));
        let tx = x + (w - layout.metrics.size.width.0) / 2.0;
        self.p.text(
            LogicalPoint::new(tx, y + (h - line_height) / 2.0),
            layout,
            if primary { BG } else { TEXT },
        );
        self.controls.push(Control {
            rect: area,
            action,
            label: label.into(),
        });
    }
}

fn volume_slider(c: &mut Canvas<'_>, right: f64, footer: f64, volume: u8) {
    let x = right - 166.0;
    let y = footer + 23.0;
    let width = 112.0;
    let filled = width * f64::from(volume.min(100)) / 100.0;
    c.icon(
        AuraIcon::Volume,
        LogicalRect::from_xywh(right - 193.0, footer + 15.0, 16.0, 16.0),
        MUTED,
    );
    c.box_(x, y, width, 4.0, LINE, 2.0);
    if filled > 0.0 {
        c.box_(x, y, filled, 4.0, ACCENT, 2.0);
    }
    c.box_(x + filled - 6.0, y - 4.0, 12.0, 12.0, ACCENT, 6.0);
    c.text(
        right - 48.0,
        footer + 15.0,
        &format!("{volume}%"),
        11.0,
        TEXT,
        false,
    );
    c.controls.push(Control {
        rect: LogicalRect::from_xywh(x - 7.0, footer + 11.0, width + 14.0, 28.0),
        action: Action::VolumeSlider,
        label: "Ползунок громкости".into(),
    });
}

pub fn compose(
    app: &Aura,
    text: &mut TextSystem,
    width: f64,
    height: f64,
    phase: f64,
    focus: Option<usize>,
    hover: Option<LogicalPoint>,
) -> Frame {
    compose_with_tooltip(app, text, width, height, phase, focus, hover, None)
}

#[allow(clippy::too_many_arguments)]
pub fn compose_with_tooltip(
    app: &Aura,
    text: &mut TextSystem,
    width: f64,
    height: f64,
    phase: f64,
    focus: Option<usize>,
    hover: Option<LogicalPoint>,
    tooltip: Option<&str>,
) -> Frame {
    if let Some(live) = &app.live {
        return compose_live(
            live, app.volume, text, width, height, phase, focus, hover, tooltip,
        );
    }
    let mut c = Canvas {
        p: PaintBuilder::with_viewport(LogicalRect::from_xywh(0.0, 0.0, width, height)),
        t: text,
        controls: vec![],
        focus,
        hover,
    };
    let right = width - 20.0;
    let w = width - 40.0;
    c.box_(0.0, 0.0, width, height, BG, 0.0);
    c.box_(20.0, 23.0, 35.0, 35.0, ACCENT, 12.0);
    for (i, h) in [9.0, 20.0, 14.0, 7.0].into_iter().enumerate() {
        c.box_(29.0 + i as f64 * 5.0, 40.0 - h / 2.0, 3.0, h, BG, 1.5);
    }
    c.text(65.0, 18.0, "aura", 27.0, TEXT, true);
    c.text(66.0, 53.0, "the music between us", 10.0, MUTED, false);
    c.icon_button(
        [right - 36.0, 26.0, 32.0, 30.0],
        if app.settings { "✓" } else { "⚙" },
        if app.settings {
            "Готово"
        } else {
            "Настройки"
        },
        Action::Settings,
    );
    c.box_(20.0, 79.0, w, 1.0, LINE, 0.0);
    c.box_(
        23.0,
        104.0,
        6.0,
        6.0,
        if app.broadcasting { GREEN } else { MUTED },
        3.0,
    );
    c.text(
        38.0,
        93.0,
        if app.broadcasting {
            "Вы в демоэфире"
        } else {
            "Вы в тени"
        },
        14.0,
        TEXT,
        true,
    );
    c.text(
        38.0,
        114.0,
        if app.broadcasting {
            "0 слушателей · симуляция"
        } else {
            "Только вы решаете, когда делиться"
        },
        10.0,
        MUTED,
        false,
    );
    c.button(
        [right - 116.0, 91.0, 116.0, 40.0],
        if app.broadcasting {
            "Уйти в тень"
        } else {
            "В эфир"
        },
        Action::Broadcast,
        !app.broadcasting,
    );
    if app.settings {
        c.text(20.0, 159.0, "Ваше пространство", 21.0, TEXT, true);
        c.text(20.0, 194.0, "Источник демоэфира", 13.0, MUTED, false);
        for (i, label) in ["Музыкальный плеер", "Браузер", "Другой источник"]
            .into_iter()
            .enumerate()
        {
            let label = format!("{}  {label}", if app.source == Some(i) { "●" } else { "○" });
            c.button(
                [20.0, 222.0 + i as f64 * 43.0, w, 35.0],
                &label,
                Action::Source(i),
                app.source == Some(i),
            );
        }
        c.text(20.0, 357.0, "Кому доступен эфир", 13.0, MUTED, false);
        c.button(
            [20.0, 385.0, w, 37.0],
            if app.friends_only {
                "Только друзья"
            } else {
                "По приглашению"
            },
            Action::Privacy,
            false,
        );
        c.text(20.0, 441.0, "Лаборатория Smart Feed", 14.0, TEXT, true);
        c.text(
            20.0,
            466.0,
            "Завершите текущий эфир и проверьте переход.",
            11.0,
            MUTED,
            false,
        );
        c.button(
            [20.0, 497.0, w, 38.0],
            "Завершить текущий демоэфир",
            Action::SimulateEnd,
            false,
        );
        c.button(
            [20.0, 545.0, w, 36.0],
            "Сбросить демо",
            Action::ResetDemo,
            false,
        );
        c.text(
            20.0,
            599.0,
            "Прототип не захватывает и не передаёт звук.",
            11.0,
            MUTED,
            false,
        );
        c.text(
            20.0,
            618.0,
            "Источники и участники — демонстрационные.",
            11.0,
            MUTED,
            false,
        );
    } else {
        c.box_(20.0, 152.0, w, 190.0, PANEL, 16.0);
        c.text(36.0, 167.0, "SMART FEED", 10.0, ACCENT, true);
        c.text(
            right - 100.0,
            167.0,
            if app.pinned {
                "ЗАКРЕПЛЁН"
            } else {
                "ДЕМО"
            },
            10.0,
            MUTED,
            false,
        );
        if let Some(i) = app.current {
            let friend = &app.friends[i];
            c.text(36.0, 193.0, friend.track, 23.0, TEXT, true);
            c.text(
                36.0,
                228.0,
                &format!("{}  ·  у {}", friend.artist, friend.name),
                12.0,
                MUTED,
                false,
            );
        } else {
            c.text(36.0, 193.0, "Пока тихо", 23.0, TEXT, true);
            c.text(
                36.0,
                228.0,
                "Ждём следующий доступный эфир",
                12.0,
                MUTED,
                false,
            );
        }
        for i in 0..45 {
            let signal =
                ((i as f64 * 1.7 + phase * 4.0).sin() * (i as f64 * 0.37 + phase).cos()).abs();
            let h = if app.playing {
                3.0 + signal * 23.0
            } else {
                3.0
            };
            c.box_(
                36.0 + i as f64 * (w - 32.0) / 45.0,
                269.0 - h / 2.0,
                3.0,
                h,
                if app.playing { ACCENT } else { LINE },
                1.5,
            );
        }
        c.button(
            [36.0, 294.0, 103.0, 33.0],
            if app.playing {
                "Ⅱ  Пауза"
            } else {
                "▶  Слушать"
            },
            Action::Play,
            true,
        );
        c.icon_button([148.0, 294.0, 65.0, 33.0], "⏭", "Далее", Action::Next);
        c.button(
            [222.0, 294.0, w - 218.0, 33.0],
            if app.pinned {
                "Отпустить"
            } else {
                "Закрепить"
            },
            Action::Pin,
            false,
        );
        c.button(
            [20.0, 358.0, 104.0, 31.0],
            "Друзья",
            Action::Friends,
            !app.queue,
        );
        c.button(
            [133.0, 358.0, 104.0, 31.0],
            "Очередь",
            Action::Queue,
            app.queue,
        );
        c.text(
            right - 95.0,
            366.0,
            &format!("{} в эфире", app.available().count()),
            11.0,
            GREEN,
            false,
        );
        let ids: Vec<_> = if app.queue {
            app.available().collect()
        } else {
            (0..app.friends.len()).collect()
        };
        if ids.is_empty() {
            c.text(24.0, 419.0, "Новых эфиров пока нет", 13.0, MUTED, false);
        }
        for (row, i) in ids.into_iter().enumerate() {
            let f = &app.friends[i];
            let y = 400.0 + row as f64 * 47.0;
            let selected = app.current == Some(i);
            if selected {
                c.box_(20.0, y, w, 44.0, PANEL, 10.0);
            }
            let tint = match f.presence {
                Presence::Live => GREEN,
                Presence::Away => ACCENT,
                Presence::Offline => MUTED,
            };
            c.box_(28.0, y + 6.0, 31.0, 31.0, LINE, 11.0);
            c.text(38.0, y + 10.0, f.initials, 14.0, tint, true);
            c.text(70.0, y + 3.0, f.name, 13.0, TEXT, true);
            c.text(
                70.0,
                y + 23.0,
                if f.excluded {
                    "Исключён из потока"
                } else {
                    f.track
                },
                11.0,
                MUTED,
                false,
            );
            if f.presence == Presence::Live {
                if app.queue {
                    c.button(
                        [right - 91.0, y + 9.0, 27.0, 27.0],
                        "×",
                        Action::Exclude(i),
                        false,
                    );
                }
                c.button(
                    [right - 58.0, y + 9.0, 50.0, 27.0],
                    if f.excluded {
                        "+"
                    } else if selected && app.playing {
                        "♫"
                    } else {
                        "→"
                    },
                    if f.excluded {
                        Action::Exclude(i)
                    } else {
                        Action::Select(i)
                    },
                    false,
                );
            } else {
                c.box_(right - 26.0, y + 19.0, 6.0, 6.0, tint, 3.0);
            }
        }
        c.text(
            22.0,
            643.0,
            if app.pinned {
                "Этот эфир с вами до его завершения."
            } else {
                "Другой человек. Та же волна."
            },
            11.0,
            MUTED,
            false,
        );
    }
    let footer = height - 53.0;
    c.box_(20.0, footer, w, 1.0, LINE, 0.0);
    c.text(20.0, footer + 15.0, "Превью интерфейса", 11.0, MUTED, false);
    volume_slider(&mut c, right, footer, app.volume);
    c.tooltip(tooltip, width, height);
    Frame {
        paint: c.p.finish(),
        controls: c.controls,
        editor_scroll: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn compose_live(
    live: &aura_core::live::Live,
    volume: u8,
    text: &mut TextSystem,
    width: f64,
    height: f64,
    phase: f64,
    focus: Option<usize>,
    hover: Option<LogicalPoint>,
    tooltip: Option<&str>,
) -> Frame {
    let mut c = Canvas {
        p: PaintBuilder::with_viewport(LogicalRect::from_xywh(0.0, 0.0, width, height)),
        t: text,
        controls: vec![],
        focus,
        hover,
    };
    let w = width - 40.0;
    let right = width - 20.0;
    c.box_(0.0, 0.0, width, height, BG, 0.0);
    c.box_(20.0, 23.0, 35.0, 35.0, ACCENT, 12.0);
    for (i, h) in [9.0, 20.0, 14.0, 7.0].into_iter().enumerate() {
        c.box_(29.0 + i as f64 * 5.0, 40.0 - h / 2.0, 3.0, h, BG, 1.5);
    }
    c.text(65.0, 18.0, "aura", 27.0, TEXT, true);
    c.text(66.0, 53.0, "the music between us", 10.0, MUTED, false);
    c.box_(20.0, 79.0, w, 1.0, LINE, 0.0);
    c.box_(
        23.0,
        104.0,
        6.0,
        6.0,
        if live.capturing { GREEN } else { MUTED },
        3.0,
    );
    c.text(
        38.0,
        93.0,
        if live.stopping {
            "Остановка…"
        } else if live.capturing {
            "Вы в эфире"
        } else if live.busy {
            "Подготовка…"
        } else {
            "Вы в тени"
        },
        14.0,
        TEXT,
        true,
    );
    c.text(
        38.0,
        115.0,
        &format!(
            "{} слушателей · {}",
            live.listeners,
            if if live.capturing {
                live.public.active
            } else {
                live.public.broadcast
            } {
                "публично"
            } else {
                "по приглашению"
            }
        ),
        10.0,
        MUTED,
        false,
    );
    c.controls.push(Control {
        rect: LogicalRect::from_xywh(38.0, 112.0, (right - 164.0).max(1.0), 21.0),
        action: Action::PublicVisibility,
        label: "Изменить видимость вашего эфира".into(),
    });
    c.button(
        [right - 116.0, 91.0, 116.0, 40.0],
        if live.capturing || live.receiving || live.busy {
            "Остановить"
        } else {
            "В эфир"
        },
        Action::Broadcast,
        !live.capturing && !live.receiving && !live.busy,
    );
    c.box_(20.0, 152.0, w, 180.0, PANEL, 16.0);
    c.button(
        [36.0, 160.0, 168.0, 28.0],
        if live.public.active {
            "Публичный эфир"
        } else if live.use_relay {
            "Через Aura Node"
        } else {
            "Прямое соединение"
        },
        Action::Route,
        false,
    );
    c.text(
        right - 94.0,
        167.0,
        if live.public.active {
            "ЧЕРЕЗ УЗЕЛ"
        } else {
            "ЗАЩИЩЕНО"
        },
        10.0,
        MUTED,
        false,
    );
    if let Some(info) = &live.media.info {
        if let Some(art) = &live.media.artwork {
            c.p.image(
                LogicalRect::from_xywh(36.0, 196.0, 56.0, 56.0),
                art.width,
                art.height,
                art.rgba.clone(),
            );
        } else {
            c.box_(36.0, 196.0, 56.0, 56.0, LINE, 8.0);
            c.icon(
                AuraIcon::Music,
                LogicalRect::from_xywh(51.0, 207.0, 22.0, 22.0),
                ACCENT,
            );
        }
        c.wrapped(
            104.0,
            194.0,
            w - 100.0,
            if info.title.is_empty() {
                "Музыка играет"
            } else {
                &info.title
            },
            17.0,
            TEXT,
            1,
        );
        c.wrapped(104.0, 217.0, w - 100.0, &info.artist, 12.0, TEXT, 1);
        c.wrapped(104.0, 236.0, w - 100.0, &info.album, 10.0, MUTED, 1);
        let clock = |ms: u64| format!("{}:{:02}", ms / 60000, ms / 1000 % 60);
        c.wrapped(
            36.0,
            256.0,
            w - 32.0,
            &format!(
                "{} · {} · {} / {}",
                info.app,
                info.playback.label(),
                clock(info.position_ms),
                clock(info.duration_ms)
            ),
            9.0,
            MUTED,
            1,
        );
    } else {
        c.text(
            36.0,
            194.0,
            if live.receiving {
                "На одной волне"
            } else if live.capturing {
                "Ваша музыка рядом"
            } else {
                if live.public.tab {
                    "На одной волне"
                } else {
                    "Слушайте вместе"
                }
            },
            21.0,
            TEXT,
            true,
        );
        c.wrapped(
            36.0,
            228.0,
            w - 32.0,
            if live.receiving {
                if live.use_relay {
                    "Получение через Aura Node"
                } else {
                    "Получение напрямую от друга"
                }
            } else if live.capturing {
                live.selected
                    .as_ref()
                    .map_or("Источник", |s| s.name.as_str())
            } else {
                if live.public.tab {
                    "Выберите публичный эфир в списке ниже"
                } else {
                    "Вставьте приглашение друга из буфера обмена"
                }
            },
            12.0,
            MUTED,
            1,
        );
    }
    for i in 0..45 {
        let signal = live.rms.min(0.2) as f64 * 5.0;
        let h = 3.0 + signal * 9.0 * ((i as f64 * 1.7 + phase * 4.0).sin().abs());
        c.box_(
            36.0 + i as f64 * (w - 32.0) / 45.0,
            277.0 - h / 2.0,
            3.0,
            h,
            if live.rms > 0.0001 { ACCENT } else { LINE },
            1.5,
        );
    }
    if live.receiving {
        let half = (w - 40.0) / 2.0;
        c.button(
            [36.0, 289.0, half, 29.0],
            "Копировать",
            Action::CopyTrack,
            false,
        );
        c.button(
            [44.0 + half, 289.0, half, 29.0],
            "Поделиться",
            Action::ShareTrack,
            false,
        );
    } else if live.media.info.is_some() && live.capturing {
        let half = (w - 40.0) / 2.0;
        c.button(
            [36.0, 289.0, half, 29.0],
            "Копировать",
            Action::CopyTrack,
            false,
        );
        c.button(
            [44.0 + half, 289.0, half, 29.0],
            "Копировать ссылку",
            Action::CopyInvite,
            true,
        );
    } else if live.capturing {
        c.button(
            [36.0, 289.0, w - 32.0, 29.0],
            "Копировать ссылку",
            Action::CopyInvite,
            true,
        );
    } else if live.receiving || live.busy {
        c.button(
            [36.0, 289.0, w - 32.0, 29.0],
            "Остановить соединение",
            Action::StopAudio,
            false,
        );
    } else {
        c.button(
            [36.0, 289.0, w - 32.0, 29.0],
            if live.public.tab {
                "Выбрать источник для эфира"
            } else {
                "Вставить приглашение и слушать"
            },
            if live.public.tab {
                Action::SocialTab(false)
            } else {
                Action::PasteInvite
            },
            true,
        );
    }
    c.wrapped(22.0, 343.0, w - 4.0, &live.status, 11.0, MUTED, 2);
    let tab_w = (w - 16.0) / 3.0;
    c.button(
        [20.0, 385.0, tab_w, 29.0],
        "Источники",
        Action::SocialTab(false),
        !live.friends_tab && !live.public.tab,
    );
    c.button(
        [28.0 + tab_w, 385.0, tab_w, 29.0],
        "Друзья",
        Action::SocialTab(true),
        live.friends_tab && !live.public.tab,
    );
    c.button(
        [36.0 + 2.0 * tab_w, 385.0, tab_w, 29.0],
        "Эфиры",
        Action::PublicTab,
        live.public.tab,
    );
    if live.public.tab {
        c.button(
            [20.0, 425.0, 60.0, 29.0],
            "Все",
            Action::PublicFilter(false),
            !live.public.favorites_only,
        );
        c.button(
            [88.0, 425.0, 110.0, 29.0],
            "Избранное",
            Action::PublicFilter(true),
            live.public.favorites_only,
        );
        c.state_icon_button(
            [right - 32.0, 425.0, 32.0, 29.0],
            if live.public.search_open {
                "×"
            } else {
                "search"
            },
            if live.public.search_open {
                "Закрыть поиск"
            } else {
                "Поиск по названию и автору"
            },
            Action::PublicSearch,
            live.public.search_open,
        );
        let rooms = live.public.visible();
        let count = public_page_size(height, live.public.search_open);
        let offset = live.public.offset.min(rooms.len().saturating_sub(count));
        let top = if live.public.search_open {
            509.0
        } else {
            466.0
        };
        if rooms.is_empty() {
            c.wrapped(24.0,top+5.0,w-8.0,live.public.error.as_deref().unwrap_or(if live.public.loading{"Загружаем эфиры…"}else if !live.public.query.is_empty(){"Ничего не найдено. Попробуйте другое название или автора."}else if live.public.favorites_only{"Добавьте автора в избранное звёздочкой рядом с эфиром."}else{"Пока никто не вещает публично. Выберите источник и включите публичный режим."}),12.0,MUTED,6);
        }
        for (row, (index, room)) in rooms
            .iter()
            .enumerate()
            .skip(offset)
            .take(count)
            .enumerate()
        {
            let y = top + row as f64 * 52.0;
            let current = live.public.listening.as_deref() == Some(&room.owner)
                && (live.receiving || live.busy);
            c.box_(
                20.0,
                y,
                w - 12.0,
                46.0,
                if current { LINE } else { PANEL },
                10.0,
            );
            c.wrapped(30.0, y + 5.0, w - 110.0, &room.title, 13.0, TEXT, 1);
            c.wrapped(
                30.0,
                y + 26.0,
                w - 110.0,
                &if room.link.is_empty() {
                    format!("{} · не в эфире", room.author)
                } else {
                    format!("{} · {} слуш.", room.author, room.listeners)
                },
                10.0,
                if room.link.is_empty() { MUTED } else { GREEN },
                1,
            );
            c.controls.push(Control {
                rect: LogicalRect::from_xywh(30.0, y, w - 110.0, 46.0),
                action: Action::PublicListen(index),
                label: format!("{} · {}", room.title, room.author),
            });
            let favorite = live
                .public
                .favorites
                .iter()
                .any(|(id, _)| id == &room.owner);
            c.state_icon_button(
                [right - 84.0, y + 8.0, 28.0, 30.0],
                if favorite { "★" } else { "☆" },
                if favorite {
                    "Убрать автора из избранного"
                } else {
                    "Добавить автора в избранное"
                },
                Action::PublicFavorite(index),
                favorite,
            );
            if room.link.is_empty() || live.capturing && room.owner == live.peer_id {
                c.disabled_icon([right - 48.0, y + 8.0, 28.0, 30.0], "⏵");
            } else {
                c.state_icon_button(
                    [right - 48.0, y + 8.0, 28.0, 30.0],
                    if current { "Ⅱ" } else { "⏵" },
                    if current {
                        "Остановить прослушивание"
                    } else {
                        "Слушать публичный эфир"
                    },
                    Action::PublicListen(index),
                    current,
                );
            }
        }
        if rooms.len() > count {
            let track = count as f64 * 52.0 - 6.0;
            let thumb = (track * count as f64 / rooms.len() as f64).max(24.0);
            let position = (track - thumb) * offset as f64 / (rooms.len() - count) as f64;
            c.box_(right - 5.0, top, 4.0, track, LINE, 2.0);
            c.box_(right - 5.0, top + position, 4.0, thumb, ACCENT, 2.0);
            c.controls.push(Control {
                rect: LogicalRect::from_xywh(right - 12.0, top, 12.0, track),
                action: Action::PublicScrollThumb,
                label: "Прокрутить список эфиров".into(),
            });
        }
    } else if live.friends_tab {
        c.button(
            [20.0, 420.0, 92.0, 25.0],
            "+ Aura ID",
            Action::AddFriend,
            false,
        );
        if live.friends.is_empty() {
            c.wrapped(24.0,461.0,w-8.0,"Добавьте Aura ID друга. Он получит запрос, когда будет в сети. Эфир появится после взаимного подтверждения.",13.0,MUTED,5);
        }
        for (row, (i, friend)) in live
            .friends
            .iter()
            .enumerate()
            .skip(live.friend_page * 3)
            .take(3)
            .enumerate()
        {
            use aura_social::Relation;
            let y = 450.0 + row as f64 * 50.0;
            c.box_(20.0, y, w, 47.0, PANEL, 10.0);
            c.wrapped(
                30.0,
                y + 4.0,
                w - 165.0,
                &friend.contact.name,
                13.0,
                TEXT,
                1,
            );
            let status = match friend.contact.relation {
                Relation::Incoming => "Входящий запрос",
                Relation::Outgoing => "Ждём подтверждения",
                Relation::Declined => "Дружба прекращена",
                Relation::Blocked => "Удалён",
                Relation::Accepted if friend.live => "В эфире",
                Relation::Accepted if friend.online => "В сети",
                Relation::Accepted => "Не в сети",
            };
            c.text(
                30.0,
                y + 27.0,
                status,
                10.0,
                if friend.live { GREEN } else { MUTED },
                false,
            );
            if friend.contact.relation == Relation::Incoming {
                c.button(
                    [right - 126.0, y + 8.0, 84.0, 30.0],
                    "Принять",
                    Action::FriendAccept(i),
                    true,
                );
            }
            if friend.contact.relation == Relation::Accepted {
                let pinned = live.smart_pinned.contains(&friend.contact.id);
                let excluded = live.smart_excluded.contains(&friend.contact.id);
                c.state_icon_button(
                    [right - 144.0, y + 8.0, 28.0, 30.0],
                    if pinned { "★" } else { "☆" },
                    if pinned {
                        "Убрать приоритет в Smart Feed"
                    } else {
                        "Приоритет в Smart Feed"
                    },
                    Action::SmartFeedPin(i),
                    pinned,
                );
                c.state_icon_button(
                    [right - 108.0, y + 8.0, 28.0, 30.0],
                    if excluded { "✓" } else { "−" },
                    if excluded {
                        "Вернуть друга в Smart Feed"
                    } else {
                        "Исключить из Smart Feed"
                    },
                    Action::SmartFeedExclude(i),
                    excluded,
                );
                let current = live.receiving
                    && live.smart_current.as_deref() == Some(friend.contact.id.as_str());
                let pending = (live.pending_friend
                    && live.smart_current.as_deref() == Some(friend.contact.id.as_str()))
                    || live.smart_pending_id.as_deref() == Some(friend.contact.id.as_str());
                if current {
                    c.state_icon_button(
                        [right - 72.0, y + 8.0, 28.0, 30.0],
                        "Ⅱ",
                        "Остановить прослушивание",
                        Action::FriendListen(i),
                        true,
                    );
                } else if pending {
                    c.disabled_icon([right - 72.0, y + 8.0, 28.0, 30.0], "…");
                } else if friend.live {
                    c.icon_button(
                        [right - 72.0, y + 8.0, 28.0, 30.0],
                        "⏵",
                        "Слушать эфир",
                        Action::FriendListen(i),
                    );
                } else {
                    c.disabled_icon([right - 72.0, y + 8.0, 28.0, 30.0], "⏵");
                }
            }
            c.icon_button(
                [right - 36.0, y + 8.0, 28.0, 30.0],
                "×",
                "Удалить друга",
                Action::FriendRemove(i),
            );
        }
        c.icon_button(
            [20.0, 606.0, 30.0, 27.0],
            "✎",
            "Изменить моё имя",
            Action::RenameProfile,
        );
        c.button_display(
            [58.0, 606.0, 104.0, 27.0],
            "Smart Feed",
            if live.smart_feed {
                "Выключить Smart Feed. Автоматически переключает эфиры друзей после паузы, тишины или завершения эфира."
            } else {
                "Включить Smart Feed. Автоматически переключает эфиры друзей после паузы, тишины или завершения эфира."
            },
            Action::SmartFeed,
            live.smart_feed,
        );
        c.button_display(
            [174.0, 606.0, 74.0, 27.0],
            "Очередь",
            "Настроить порядок друзей в Smart Feed",
            Action::QueueOpen,
            false,
        );
        if live.friends.len() > 3 {
            c.button(
                [right - 96.0, 606.0, 40.0, 27.0],
                "←",
                Action::FriendPage(false),
                false,
            );
            c.button(
                [right - 40.0, 606.0, 40.0, 27.0],
                "→",
                Action::FriendPage(true),
                false,
            );
        }
    } else {
        c.button(
            [20.0, 606.0, 92.0, 27.0],
            if live.scanning {
                "Поиск…"
            } else {
                "Обновить"
            },
            Action::RefreshSources,
            false,
        );
        if live.sources.is_empty() {
            c.wrapped(24.0,443.0,w-8.0,"Запустите музыку в приложении и обновите список. Системный микс и микрофон не захватываются.",13.0,MUTED,4);
        }
        for (row, (i, s)) in live
            .sources
            .iter()
            .enumerate()
            .skip(live.page * 3)
            .take(3)
            .enumerate()
        {
            let y = 427.0 + row as f64 * 43.0;
            let selected = live
                .selected
                .as_ref()
                .is_some_and(|p| p.pid == s.pid && p.started == s.started);
            c.box_(20.0, y, w, 37.0, if selected { LINE } else { PANEL }, 10.0);
            c.wrapped(33.0, y + 4.0, w - 85.0, &s.name, 13.0, TEXT, 1);
            c.text(
                33.0,
                y + 21.0,
                &format!("PID {} · дерево процесса", s.pid),
                9.0,
                MUTED,
                false,
            );
            c.checkbox(
                [right - 39.0, y + 5.0, 28.0, 27.0],
                selected,
                if selected {
                    "Снять выбор источника"
                } else {
                    "Выбрать источник"
                },
                Action::Source(i),
            );
        }
        c.button_display(
            [20.0, 565.0, 130.0, 29.0],
            if live.public.broadcast {
                "Публично"
            } else {
                "По приглашению"
            },
            "Выбрать видимость эфира",
            Action::PublicVisibility,
            live.public.broadcast,
        );
        if live.public.broadcast {
            c.button_display(
                [158.0, 565.0, w - 138.0, 29.0],
                if live.public.title.is_empty() {
                    "Название эфира"
                } else {
                    &live.public.title
                },
                &live.public.title,
                Action::PublicTitle,
                false,
            );
        } else {
            c.wrapped(158.0, 571.0, w - 138.0, "Доступ по ссылке", 11.0, MUTED, 1);
        }
        if live.sources.len() > 3 {
            c.button(
                [128.0, 606.0, 40.0, 27.0],
                "←",
                Action::SourcePage(false),
                false,
            );
            c.text(
                180.0,
                611.0,
                &format!("{} / {}", live.page + 1, live.sources.len().div_ceil(3)),
                11.0,
                MUTED,
                false,
            );
            c.button(
                [right - 40.0, 606.0, 40.0, 27.0],
                "→",
                Action::SourcePage(true),
                false,
            );
        } else {
            c.text(
                126.0,
                612.0,
                "Выберите источник и начните эфир.",
                10.0,
                MUTED,
                false,
            );
        }
    }
    c.text(
        22.0,
        if live.public.tab {
            height - 77.0
        } else {
            643.0
        },
        &if live.public.tab {
            format!(
                "Каталог · узлы {}/{}",
                live.public.available, live.nodes_total
            )
        } else if live.friends_tab {
            format!(
                "{} · {}",
                live.profile_name,
                if live.social_connected {
                    if live.nodes_total > 1 {
                        "Aura Nodes в сети"
                    } else {
                        "Aura Node в сети"
                    }
                } else {
                    "Подключаем Aura Node…"
                }
            )
        } else {
            format!(
                "RTT {} мс · буфер {} мс · потери {}",
                live.rtt_ms,
                live.buffer_ms,
                live.dropped + live.audio_dropped + live.concealed
            )
        },
        10.0,
        MUTED,
        false,
    );
    let footer = height - 53.0;
    c.box_(20.0, footer, w, 1.0, LINE, 0.0);
    c.button(
        [20.0, footer + 10.0, 120.0, 28.0],
        "Мой Aura ID",
        Action::CopyId,
        false,
    );
    volume_slider(&mut c, right, footer, volume);
    if live.nodes_total > 1 {
        c.text(
            right - 100.0,
            33.0,
            &format!("Узлы {}/{}", live.nodes_connected, live.nodes_total),
            11.0,
            if live.nodes_connected == live.nodes_total {
                GREEN
            } else {
                MUTED
            },
            false,
        );
    }
    let mut editor_scroll = None;
    if let Some(editor) = &live.editor {
        let search = editor.purpose == aura_core::live::EditPurpose::PublicSearch;
        if !search {
            c.controls.clear();
            c.box_(0.0, 0.0, width, height, Color::rgba(10, 10, 15, 220), 0.0);
            c.box_(20.0, 365.0, w, 285.0, PANEL, 14.0);
            c.text(
                34.0,
                383.0,
                if editor.purpose == aura_core::live::EditPurpose::PublicTitle {
                    "Название публичного эфира"
                } else if editor.name {
                    "Ваше имя"
                } else {
                    "Добавить друга по Aura ID"
                },
                16.0,
                TEXT,
                true,
            );
        }
        let edit = editor_model(editor);
        let mut scroll = editor.scroll;
        everty_ui::TextField {
            edit: &edit,
            placeholder: if search {
                "Название или автор…"
            } else if editor.purpose == aura_core::live::EditPurpose::PublicTitle {
                "Как назовём эфир?"
            } else if editor.name {
                "Введите имя"
            } else {
                "Вставьте Aura ID"
            },
            focused: editor.focused,
            caret_on: true,
            scroll: &mut scroll,
        }
        .paint(editor_area(width, editor), c.t, editor_theme(), &mut c.p);
        editor_scroll = Some(scroll);
        c.controls.push(Control {
            rect: editor_area(width, editor),
            action: Action::EditFocus,
            label: "Текстовое поле".into(),
        });
        if !search {
            c.text(
                34.0,
                481.0,
                "Ctrl+A — выделить · Ctrl+V — вставить",
                10.0,
                MUTED,
                false,
            );
            c.button(
                [32.0, 518.0, 90.0, 30.0],
                "Вставить",
                Action::EditPaste,
                false,
            );
            c.button(
                [130.0, 518.0, 100.0, 30.0],
                "Сохранить",
                Action::EditSave,
                true,
            );
            c.button(
                [238.0, 518.0, w - 230.0, 30.0],
                "Отмена",
                Action::EditCancel,
                false,
            );
            c.wrapped(34.0, 563.0, w - 28.0, &live.status, 11.0, MUTED, 4);
        }
    }
    if let Some(editor) = &live.queue_editor {
        c.controls.clear();
        c.p.set_layer(everty_ui::PaintLayer::Popup);
        c.box_(0.0, 0.0, width, height, Color::rgba(8, 9, 14, 225), 0.0);
        let y = ((height - 496.0) / 2.0).max(16.0);
        c.box_(20.0, y, w, 496.0, PANEL, 18.0);
        c.text(40.0, y + 20.0, "Очередь Smart Feed", 21.0, TEXT, true);
        c.wrapped(
            40.0,
            y + 56.0,
            w - 40.0,
            "Меняйте порядок стрелками. Друзья со звездой сохраняют приоритет.",
            12.0,
            MUTED,
            3,
        );
        if editor.ids.is_empty() {
            c.wrapped(
                40.0,
                y + 132.0,
                w - 40.0,
                "Здесь появятся друзья после взаимного подтверждения запроса.",
                13.0,
                MUTED,
                4,
            );
        }
        for (index, id) in editor.ids.iter().enumerate().skip(editor.page * 4).take(4) {
            let row_y = y + 110.0 + (index % 4) as f64 * 56.0;
            c.box_(32.0, row_y, w - 24.0, 48.0, BG, 9.0);
            c.text(
                42.0,
                row_y + 14.0,
                &(index + 1).to_string(),
                11.0,
                MUTED,
                true,
            );
            let friend = live.friends.iter().find(|friend| &friend.contact.id == id);
            c.wrapped(
                68.0,
                row_y + 4.0,
                w - 160.0,
                friend.map_or("Друг удалён", |friend| {
                    friend.contact.name.as_str()
                }),
                13.0,
                TEXT,
                1,
            );
            let available = friend
                .is_some_and(|friend| friend.contact.relation == aura_social::Relation::Accepted);
            let excluded = live.smart_excluded.contains(id);
            let pinned = live.smart_pinned.contains(id);
            let in_air =
                available && !excluded && friend.is_some_and(|friend| friend.online && friend.live);
            let status = if !available {
                "Больше не в друзьях"
            } else if excluded {
                "Исключён из Smart Feed"
            } else if pinned && in_air {
                "Приоритет · В эфире"
            } else if pinned {
                "Приоритет"
            } else if in_air {
                "В эфире"
            } else if friend.is_some_and(|friend| friend.online) {
                "В сети"
            } else {
                "Не в сети"
            };
            c.wrapped(
                68.0,
                row_y + 26.0,
                w - 160.0,
                status,
                10.0,
                if in_air { GREEN } else { MUTED },
                1,
            );
            let up = [right - 84.0, row_y + 9.0, 28.0, 30.0];
            let down = [right - 48.0, row_y + 9.0, 28.0, 30.0];
            if index > 0 {
                c.icon_button(up, "up", "Выше в очереди", Action::QueueMove(index, true));
            } else {
                c.disabled_icon(up, "up");
            }
            if index + 1 < editor.ids.len() {
                c.icon_button(
                    down,
                    "down",
                    "Ниже в очереди",
                    Action::QueueMove(index, false),
                );
            } else {
                c.disabled_icon(down, "down");
            }
        }
        if editor.ids.len() > 4 {
            c.button(
                [40.0, y + 338.0, 64.0, 28.0],
                "Назад",
                Action::QueuePage(false),
                false,
            );
            c.text(
                118.0,
                y + 344.0,
                &format!("{} / {}", editor.page + 1, editor.ids.len().div_ceil(4)),
                11.0,
                MUTED,
                false,
            );
            c.button(
                [right - 84.0, y + 338.0, 64.0, 28.0],
                "Далее",
                Action::QueuePage(true),
                false,
            );
        }
        c.wrapped(
            40.0,
            y + 378.0,
            w - 40.0,
            editor.error.as_deref().unwrap_or(
                "Эфир продолжит играть. Изменения применятся при следующем выборе эфира.",
            ),
            11.0,
            if editor.error.is_some() {
                DANGER
            } else {
                MUTED
            },
            3,
        );
        let button_w = (w - 48.0) / 2.0;
        c.button(
            [40.0, y + 436.0, button_w, 40.0],
            "Отмена",
            Action::QueueCancel,
            false,
        );
        c.button(
            [48.0 + button_w, y + 436.0, button_w, 40.0],
            "Сохранить",
            Action::QueueSave,
            true,
        );
    }
    if let Some((_, name)) = &live.remove_friend {
        c.controls.clear();
        c.p.set_layer(everty_ui::PaintLayer::Popup);
        c.box_(0.0, 0.0, width, height, Color::rgba(8, 9, 14, 225), 0.0);
        let y = ((height - 276.0) / 2.0).max(20.0);
        c.box_(20.0, y, w, 276.0, PANEL, 18.0);
        c.text(40.0, y + 22.0, "Удалить друга?", 21.0, TEXT, true);
        c.wrapped(40.0, y + 60.0, w - 40.0, name, 15.0, ACCENT, 2);
        c.wrapped(40.0, y + 111.0, w - 40.0,
            if live.capturing || live.receiving || live.busy {
                "Друг исчезнет из списка. Текущее соединение завершится, чтобы отозвать доступ к эфиру."
            } else {
                "Друг исчезнет из списка. Чтобы слушать друг друга снова, понадобится новое приглашение в друзья."
            }, 12.0, MUTED, 4);
        let button_w = (w - 48.0) / 2.0;
        c.button(
            [40.0, y + 214.0, button_w, 40.0],
            "Отмена",
            Action::FriendRemoveCancel,
            false,
        );
        c.button(
            [48.0 + button_w, y + 214.0, button_w, 40.0],
            "Удалить",
            Action::FriendRemoveConfirm,
            true,
        );
    }
    c.tooltip(tooltip, width, height);
    Frame {
        paint: c.p.finish(),
        controls: c.controls,
        editor_scroll,
    }
}

#[cfg(test)]
mod editor_tests {
    use super::*;
    #[test]
    fn visibility_can_be_selected_from_the_header_and_sources() {
        let app = Aura::live();
        let mut text = TextSystem::new();
        let frame = compose(&app, &mut text, 380.0, 720.0, 0.0, None, None);
        for point in [
            LogicalPoint::new(80.0, 120.0),
            LogicalPoint::new(80.0, 575.0),
        ] {
            assert_eq!(
                frame
                    .controls
                    .iter()
                    .find(|c| c.rect.contains(point))
                    .map(|c| c.action),
                Some(Action::PublicVisibility)
            );
        }
    }
    #[test]
    fn public_list_search_and_actions_fit_without_overlapping_hit_targets() {
        for search in [false, true] {
            for (width, height) in [(380.0, 720.0), (500.0, 860.0)] {
                let mut app = Aura::live();
                let live = app.live.as_mut().unwrap();
                live.public.tab = true;
                live.public.search_open = search;
                for index in 0..10 {
                    live.public.rooms.push(aura_core::live::PublicRoom {
                        owner: index.to_string(),
                        title: "Длинное название эфира 🎵".into(),
                        author: "Автор".into(),
                        listeners: 4,
                        track: String::new(),
                        artist: String::new(),
                        link: "test".into(),
                    });
                }
                if search {
                    let mut e = aura_core::live::Editor::new(false, "Автор".into());
                    e.purpose = aura_core::live::EditPurpose::PublicSearch;
                    live.editor = Some(e);
                }
                let mut text = TextSystem::new();
                let frame = compose(&app, &mut text, width, height, 0.0, None, None);
                assert!(frame
                    .controls
                    .iter()
                    .any(|c| c.action == Action::PublicScrollThumb));
                assert_eq!(
                    frame
                        .controls
                        .iter()
                        .filter(|c| matches!(c.action, Action::PublicFavorite(_)))
                        .count(),
                    public_page_size(height, search)
                );
                for (i, control) in frame.controls.iter().enumerate() {
                    assert!(
                        control.rect.left() >= 0.0
                            && control.rect.top() >= 0.0
                            && control.rect.right() <= width
                            && control.rect.bottom() <= height,
                        "{:?}",
                        control.action
                    );
                    for other in &frame.controls[i + 1..] {
                        assert!(
                            !control.rect.intersects(other.rect),
                            "Overlapping {:?} / {:?}",
                            control.action,
                            other.action
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn remove_confirmation_exposes_only_cancel_and_confirm() {
        let mut app = Aura::live();
        let live = app.live.as_mut().unwrap();
        live.remove_friend = Some(("stable-peer-id".into(), "Маша".into()));
        live.receiving = true;
        let mut text = TextSystem::new();
        let frame = compose(&app, &mut text, 380.0, 720.0, 0.0, Some(0), None);
        let actions: Vec<_> = frame
            .controls
            .iter()
            .map(|control| control.action)
            .collect();
        assert_eq!(
            actions,
            vec![Action::FriendRemoveCancel, Action::FriendRemoveConfirm]
        );
        assert!(frame
            .controls
            .iter()
            .all(|control| !control.rect.contains(LogicalPoint::new(30.0, 100.0))));
        // Rendering a confirmation must preserve the current connection and target identity.
        let live = app.live.as_ref().unwrap();
        assert!(live.receiving);
        assert_eq!(live.remove_friend.as_ref().unwrap().0, "stable-peer-id");
    }
    #[test]
    fn name_field_is_clickable_and_caret_survives_repaint() {
        let mut app = Aura::live();
        let live = app.live.as_mut().unwrap();
        live.editor = Some(aura_core::live::Editor::new(true, "Слушатель".into()));
        let mut text = TextSystem::new();
        let frame = compose(&app, &mut text, 380.0, 720.0, 0.0, None, None);
        let control = frame
            .controls
            .iter()
            .find(|c| c.action == Action::EditFocus)
            .expect("Editor must expose an input hit target");
        assert!(control.rect.contains(LogicalPoint::new(50.0, 440.0)));
        let editor = app.live.as_mut().unwrap().editor.as_mut().unwrap();
        let mut edit = editor_model(editor);
        assert_eq!(edit.selected_text(), Some("Слушатель"));
        let index = everty_ui::TextField::index_at(
            &editor.value,
            0.0,
            control.rect,
            &mut text,
            editor_theme(),
            44.0,
        );
        edit.set_caret(index, false);
        edit.insert("Мой ");
        update_editor(editor, &edit);
        let mut restored = editor_model(editor);
        assert_eq!(restored.text(), "Мой Слушатель");
        assert_eq!(restored.caret(), "Мой ".len());
        restored.move_caret(everty_ui::Movement::End, true);
        restored.insert("эфир 🎵");
        update_editor(editor, &restored);
        assert_eq!(editor.value, "Мой эфир 🎵");
        assert_eq!(editor_model(editor).selection(), None);
    }
}
