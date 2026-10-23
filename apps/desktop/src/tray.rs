//! Platform tray adapter; unsupported platforms keep close-to-exit behavior.
use crate::UserEvent;
use aura_core::Aura;
use winit::event_loop::EventLoopProxy;

#[cfg(target_os = "windows")]
pub struct Tray {
    icon: tray_icon::TrayIcon,
    broadcast: tray_icon::menu::MenuItem,
    play: tray_icon::menu::MenuItem,
    next: tray_icon::menu::MenuItem,
    listeners: tray_icon::menu::MenuItem,
    summary: std::cell::RefCell<String>,
    live_icon: std::cell::Cell<bool>,
}

#[cfg(target_os = "windows")]
impl Tray {
    pub fn new(proxy: EventLoopProxy<UserEvent>) -> Result<Self, Box<dyn std::error::Error>> {
        use tray_icon::{
            menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
            MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
        };
        let menu = Menu::new();
        let show = MenuItem::with_id("show", "Открыть Aura", true, None);
        let broadcast = MenuItem::with_id("broadcast", "В демоэфир", true, None);
        let play = MenuItem::with_id("play", "Слушать Smart Feed", true, None);
        let next = MenuItem::with_id("next", "Следующий друг", true, None);
        let listeners = MenuItem::new("Демо · 0 слушателей", false, None);
        let quit = MenuItem::with_id("quit", "Завершить Aura", true, None);
        menu.append_items(&[
            &show,
            &PredefinedMenuItem::separator(),
            &broadcast,
            &play,
            &next,
            &listeners,
            &PredefinedMenuItem::separator(),
            &quit,
        ])?;
        let menu_proxy = proxy.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let _ = menu_proxy.send_event(UserEvent::Menu(event.id.0));
        }));
        TrayIconEvent::set_event_handler(Some(move |event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                let _ = proxy.send_event(UserEvent::Show);
            }
        }));
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Aura · в тени")
            .with_icon(Self::icon(false)?)
            .build()?;
        Ok(Self {
            icon,
            broadcast,
            play,
            next,
            listeners,
            summary: std::cell::RefCell::new(String::new()),
            live_icon: std::cell::Cell::new(false),
        })
    }
    fn icon(live: bool) -> Result<tray_icon::Icon, tray_icon::BadIcon> {
        let mut rgba = vec![0u8; 32 * 32 * 4];
        for y in 0..32usize {
            for x in 0..32usize {
                let dx = x as f64 - 15.5;
                let dy = y as f64 - 15.5;
                if dx * dx + dy * dy < 225.0 {
                    let color = if live {
                        [137, 210, 174, 255]
                    } else {
                        [185, 166, 249, 255]
                    };
                    rgba[(y * 32 + x) * 4..(y * 32 + x + 1) * 4].copy_from_slice(&color);
                    if (9..23).contains(&x)
                        && x % 5 < 3
                        && dy.abs() < (5.0 + (x as f64 * 1.7).sin().abs() * 5.0)
                    {
                        rgba[(y * 32 + x) * 4..(y * 32 + x + 1) * 4]
                            .copy_from_slice(&[19, 20, 26, 255]);
                    }
                }
            }
        }
        tray_icon::Icon::from_rgba(rgba, 32, 32)
    }
    pub fn update(&self, model: &Aura) {
        if let Some(live) = &model.live {
            let summary = format!(
                "{}:{}:{}:{}",
                live.capturing, live.receiving, live.busy, live.listeners
            );
            if *self.summary.borrow() == summary {
                return;
            }
            *self.summary.borrow_mut() = summary;
            self.broadcast.set_text(if live.capturing || live.busy {
                "Остановить эфир"
            } else {
                "В эфир"
            });
            self.play.set_text(if live.receiving {
                "Остановить прослушивание"
            } else {
                "Вставить приглашение и слушать"
            });
            self.next.set_enabled(false);
            self.listeners
                .set_text(format!("{} слушателей · по приглашению", live.listeners));
            let _ = self.icon.set_tooltip(Some(if live.capturing {
                "Aura · в эфире"
            } else if live.receiving {
                "Aura · слушаем друга"
            } else {
                "Aura · в тени"
            }));
            if self.live_icon.replace(live.capturing) != live.capturing {
                if let Ok(icon) = Self::icon(live.capturing) {
                    let _ = self.icon.set_icon(Some(icon));
                }
            }
            return;
        }
        self.summary.borrow_mut().clear();
        self.next.set_enabled(true);
        self.listeners.set_text("Демо · 0 слушателей");
        self.broadcast.set_text(if model.broadcasting {
            "Уйти в тень"
        } else {
            "В демоэфир"
        });
        self.play.set_text(if model.playing {
            "Пауза Smart Feed"
        } else {
            "Слушать Smart Feed"
        });
        let _ = self.icon.set_tooltip(Some(if model.broadcasting {
            "Aura · демоэфир · 0 слушателей"
        } else {
            "Aura · в тени · локальный прототип"
        }));
        if let Ok(icon) = Self::icon(model.broadcasting) {
            let _ = self.icon.set_icon(Some(icon));
            self.live_icon.set(model.broadcasting);
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub struct Tray;
#[cfg(not(target_os = "windows"))]
impl Tray {
    pub fn new(_: EventLoopProxy<UserEvent>) -> Result<Self, Box<dyn std::error::Error>> {
        Err("Tray is currently implemented on Windows".into())
    }
    pub fn update(&self, _: &Aura) {}
}
