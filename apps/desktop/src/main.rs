#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;
mod feed_preferences;
mod media;
mod mesh_smoke;
mod public_preferences;
#[cfg(target_os = "windows")]
mod share;
mod smoke;
mod social_smoke;
mod tray;
use aura_core::{Action, Aura};
use aura_ui::{compose, Control};
use everty_ui::{DpiScale, LogicalPoint, PhysicalSize, Renderer, Rgba8Renderer, TextSystem};
use std::{
    error::Error,
    num::NonZeroU32,
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey},
    window::{Window, WindowId},
};

enum UserEvent {
    Show,
    Menu(String),
    Engine(u64, engine::Event),
    Social(aura_mesh::social::Event),
    Sources(Result<Vec<aura_core::live::Source>, String>),
    PublicCatalog(Result<(Vec<aura_core::live::PublicRoom>, usize), String>),
}

struct Desktop {
    engine: engine::Engine,
    model: Aura,
    text: TextSystem,
    raster: Rgba8Renderer,
    window: Option<Arc<Window>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    tray: Option<tray::Tray>,
    #[cfg(target_os = "windows")]
    share: Option<share::ShareDialog>,
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    controls: Vec<Control>,
    pointer: Option<LogicalPoint>,
    hovered: Option<(Action, String, Instant)>,
    tooltip_visible: bool,
    pressed: Option<(usize, Action)>,
    editor_drag: bool,
    volume_dragging: bool,
    public_scroll_dragging: bool,
    public_refresh: Option<Instant>,
    focus: Option<usize>,
    modifiers: ModifiersState,
    visible: bool,
    occluded: bool,
    last_tick: Instant,
    next_frame: Instant,
}

impl Desktop {
    fn update_hover(&mut self) {
        let hovered = self.controls.iter().find(|control| {
            self.pointer
                .is_some_and(|point| control.rect.contains(point))
        });
        if let Some(control) = hovered {
            if !self.hovered.as_ref().is_some_and(|(action, label, _)| {
                *action == control.action && label == &control.label
            }) {
                self.hovered = Some((control.action, control.label.clone(), Instant::now()));
                self.tooltip_visible = false;
            }
        } else {
            self.hovered = None;
            self.tooltip_visible = false;
        }
    }
    fn update_volume_from_pointer(&mut self) {
        let (Some(point), Some(window)) = (self.pointer, self.window.as_ref()) else {
            return;
        };
        let scale = window.scale_factor();
        let width = window.inner_size().width as f64 / scale;
        let slider_start = width - 186.0;
        let value = (((point.x.0 - slider_start) / 112.0) * 100.0)
            .round()
            .clamp(0.0, 100.0) as u8;
        self.apply(Action::Volume(value));
    }
    fn update_public_scroll(&mut self) {
        let (Some(point), Some(window), Some(live)) =
            (self.pointer, self.window.as_ref(), self.model.live.as_ref())
        else {
            return;
        };
        let height = window.inner_size().height as f64 / window.scale_factor();
        let page = aura_ui::public_page_size(height, live.public.search_open);
        let max = live.public.visible().len().saturating_sub(page);
        let top = if live.public.search_open {
            509.0
        } else {
            466.0
        };
        let fraction = ((point.y.0 - top) / (page as f64 * 52.0 - 6.0)).clamp(0.0, 1.0);
        self.apply(Action::PublicScrollTo(
            (fraction * max as f64).round() as usize
        ));
    }
    fn restore_smart_feed_preferences(&mut self) {
        let preferences = self.engine.smart_feed_preferences();
        if let Some(live) = &mut self.model.live {
            live.smart_pinned = preferences.pinned;
            live.smart_excluded = preferences.excluded;
            live.smart_order = preferences.order;
            live.public.favorites = self.engine.public_favorites();
        }
    }
    fn edit_text(&mut self, change: impl FnOnce(&mut everty_ui::TextEdit)) {
        if let Some(editor) = self
            .model
            .live
            .as_mut()
            .and_then(|live| live.editor.as_mut())
        {
            let mut edit = aura_ui::editor_model(editor);
            change(&mut edit);
            aura_ui::update_editor(editor, &edit);
        }
        if let Some(live) = self.model.live.as_mut() {
            if let Some(editor) = live
                .editor
                .as_ref()
                .filter(|e| e.purpose == aura_core::live::EditPurpose::PublicSearch)
            {
                live.public.query = editor.value.clone();
                live.public.offset = 0;
            }
        }
    }
    fn editor_click(&mut self, extend: bool) {
        let Some(point) = self.pointer else {
            return;
        };
        let Some(editor) = self
            .model
            .live
            .as_mut()
            .and_then(|live| live.editor.as_mut())
        else {
            return;
        };
        let width = self
            .window
            .as_ref()
            .map_or(380.0, |w| w.inner_size().width as f64 / w.scale_factor());
        let index = everty_ui::TextField::index_at(
            &editor.value,
            editor.scroll,
            aura_ui::editor_area(width, editor),
            &mut self.text,
            aura_ui::editor_theme(),
            point.x.0,
        );
        editor.focused = true;
        if let Some(window) = &self.window {
            window.set_ime_allowed(true);
        }
        self.focus = Some(0);
        self.edit_text(|edit| edit.set_caret(index, extend));
    }
    fn apply(&mut self, action: Action) {
        self.hovered = None;
        self.tooltip_visible = false;
        if self.model.live.is_some() {
            self.apply_live(action);
        } else {
            self.model.act(action);
        }
        // Dynamic views rebuild their focus order on the next paint.
        self.focus = None;
        if self
            .model
            .live
            .as_ref()
            .is_some_and(|live| live.remove_friend.is_some())
        {
            self.focus = Some(0);
        }
        if let Some(editor) = self
            .model
            .live
            .as_mut()
            .and_then(|live| live.editor.as_mut())
        {
            if matches!(
                action,
                Action::EditFocus | Action::EditPaste | Action::RenameProfile | Action::AddFriend
            ) {
                editor.focused = true;
                self.focus = Some(0);
            }
        }
        if let Some(window) = &self.window {
            window.set_ime_allowed(
                self.model
                    .live
                    .as_ref()
                    .is_some_and(|live| live.editor.as_ref().is_some_and(|editor| editor.focused)),
            );
        }
        if self.model.settings {
            self.show();
        }
        if let Some(tray) = &self.tray {
            tray.update(&self.model);
        }
        self.redraw();
    }
    fn scan_sources(&mut self) {
        if let Some(live) = &mut self.model.live {
            if live.scanning {
                return;
            }
            live.scanning = true;
            self.engine.scan(self.proxy.clone());
        }
    }
    fn request_smart_next(&mut self) {
        if self
            .model
            .live
            .as_ref()
            .is_some_and(|live| live.smart_pending_id.is_some())
        {
            return;
        }
        let Some((id, name)) = self.model.live.as_ref().and_then(smart_candidate) else {
            if let Some(live) = &mut self.model.live {
                live.pending_friend = false;
                live.busy = false;
                live.smart_pending_id = None;
                live.paused_since = None;
                live.silence_since = None;
                live.status = "Smart Feed ждёт доступный эфир друзей…".into();
            }
            return;
        };
        let prefetched = self.model.live.as_mut().and_then(|live| {
            live.smart_prefetch_request = None;
            live.smart_prefetched
                .take()
                .filter(|(friend_id, _)| friend_id == &id)
                .map(|(_, invitation)| invitation)
        });
        if let Some(link) = prefetched {
            if let Ok(invite) = engine::Invitation::parse(&link) {
                self.start_smart_invitation(id, invite);
                return;
            }
        }
        let Some(live) = &mut self.model.live else {
            return;
        };
        live.smart_prefetched = None;
        live.smart_pending_id = Some(id.clone());
        live.smart_listen_nonce = live.smart_listen_nonce.wrapping_add(1);
        let request = (1_u64 << 63) | (live.smart_listen_nonce & ((1_u64 << 62) - 1));
        live.smart_listen_request = Some((request, id.clone()));
        live.pending_friend = true;
        live.stopping = false;
        live.busy = true;
        live.paused_since = None;
        live.silence_since = None;
        if !live.receiving {
            live.capturing = false;
            live.invitation = None;
            live.media = aura_media::Snapshot::default();
            live.rms = 0.0;
            self.model.broadcasting = false;
            self.model.playing = false;
        }
        live.status = format!("Smart Feed подключается к {name}…");
        self.engine
            .social(aura_mesh::social::Command::Listen(id, request));
    }
    fn start_smart_invitation(&mut self, id: String, invite: engine::Invitation) {
        let use_relay = invite.uses_relay();
        if let Some(live) = &mut self.model.live {
            live.smart_pending_id = Some(id);
            live.smart_transition_authorized = false;
            live.smart_transition_output_ready = false;
            live.smart_listen_request = None;
            live.smart_prefetch_request = None;
            live.smart_prefetched = None;
            live.pending_friend = false;
            live.stopping = false;
            live.busy = true;
            live.use_relay = use_relay;
            if !live.receiving {
                live.capturing = false;
                live.invitation = None;
                live.media = aura_media::Snapshot::default();
                live.rms = 0.0;
                self.model.broadcasting = false;
                self.model.playing = false;
            }
            live.status = "Подключение к следующему эфиру… Текущий эфир продолжает играть.".into();
        }
        self.engine
            .start_crossfade(invite, use_relay, self.proxy.clone());
    }
    fn request_smart_prefetch(&mut self) {
        let Some((friend_id, _)) = self.model.live.as_ref().and_then(smart_candidate) else {
            return;
        };
        let Some(live) = &mut self.model.live else {
            return;
        };
        if !live.smart_feed || !live.receiving || live.pending_friend {
            return;
        }
        if live
            .smart_prefetched
            .as_ref()
            .is_some_and(|(cached_id, _)| cached_id == &friend_id)
        {
            return;
        }
        if live
            .smart_prefetch_request
            .as_ref()
            .is_some_and(|(_, pending_id)| pending_id == &friend_id)
        {
            return;
        }
        live.smart_prefetch_nonce = live.smart_prefetch_nonce.wrapping_add(1);
        let request = (3_u64 << 62) | (live.smart_prefetch_nonce & ((1_u64 << 62) - 1));
        live.smart_prefetch_request = Some((request, friend_id.clone()));
        live.smart_prefetched = None;
        self.engine
            .social(aura_mesh::social::Command::Listen(friend_id, request));
    }
    fn start_selected_broadcast(&mut self) {
        let restarting = self.engine.is_broadcast_session();
        let Some(live) = self.model.live.as_mut() else {
            return;
        };
        let Some(source) = live.selected.clone() else {
            live.status = "Сначала выберите приложение в списке ниже.".into();
            return;
        };
        self.engine.public_broadcast = if live.public.broadcast {
            Some(engine::PublicBroadcast {
                title: live.public.title.clone(),
                author: live.profile_name.clone(),
            })
        } else {
            None
        };
        live.smart_feed = false;
        live.smart_pending_id = None;
        live.smart_listen_request = None;
        live.smart_prefetch_request = None;
        live.smart_prefetched = None;
        live.smart_transition_authorized = false;
        live.smart_transition_output_ready = false;
        live.smart_current = None;
        live.pending_friend = false;
        live.public.listening = None;
        live.public.active = false;
        live.stopping = false;
        live.busy = true;
        live.capturing = false;
        live.receiving = false;
        live.reconnecting = false;
        live.invitation = None;
        live.media = aura_media::Snapshot::default();
        live.rms = 0.0;
        live.frames = 0;
        live.dropped = 0;
        live.audio_dropped = 0;
        live.concealed = 0;
        live.rtt_ms = 0;
        live.buffer_ms = 0;
        live.paused_since = None;
        live.silence_since = None;
        live.status = if restarting {
            "Меняем видимость и перезапускаем эфир…"
        } else {
            "Подготовка выбранного источника…"
        }
        .into();
        self.model.broadcasting = false;
        self.model.playing = false;
        self.engine.start(
            Some(aura_audio::Source {
                pid: source.pid,
                name: source.name,
                started: source.started,
            }),
            None,
            live.use_relay,
            self.proxy.clone(),
        );
    }
    fn apply_live(&mut self, action: Action) {
        if action == Action::RefreshSources {
            self.scan_sources();
            return;
        }
        let Some(live) = &mut self.model.live else {
            return;
        };
        if live.remove_friend.is_some()
            && !matches!(
                action,
                Action::FriendRemoveConfirm | Action::FriendRemoveCancel
            )
        {
            return;
        }
        if live.queue_editor.is_some()
            && !matches!(
                action,
                Action::QueueMove(_, _)
                    | Action::QueuePage(_)
                    | Action::QueueSave
                    | Action::QueueCancel
            )
        {
            return;
        }
        if action == Action::FriendRemoveConfirm && live.remove_friend.is_none() {
            return;
        }
        if let Action::FriendRemove(i) = action {
            live.remove_friend = live
                .friends
                .get(i)
                .map(|friend| (friend.contact.id.clone(), friend.contact.name.clone()));
            return;
        }
        if action == Action::FriendRemoveCancel {
            live.remove_friend = None;
            return;
        }
        if matches!(
            action,
            Action::Broadcast | Action::Source(_) | Action::PasteInvite | Action::Play
        ) {
            live.smart_feed = false;
            live.smart_pending_id = None;
            live.smart_listen_request = None;
            live.smart_prefetch_request = None;
            live.smart_prefetched = None;
            live.paused_since = None;
            live.silence_since = None;
        }
        if matches!(
            action,
            Action::Broadcast
                | Action::StopAudio
                | Action::Source(_)
                | Action::PasteInvite
                | Action::Play
                | Action::FriendListen(_)
        ) {
            live.media = aura_media::Snapshot::default();
        }
        if live.pending_friend
            && matches!(
                action,
                Action::StopAudio
                    | Action::Broadcast
                    | Action::Source(_)
                    | Action::PasteInvite
                    | Action::Play
                    | Action::FriendListen(_)
                    | Action::FriendRemoveConfirm
            )
        {
            self.engine.invalidate();
            live.pending_friend = false;
            live.busy = false;
            live.smart_feed = false;
            live.smart_attempted.clear();
            live.smart_pending_id = None;
            live.smart_listen_request = None;
            live.smart_prefetch_request = None;
            live.smart_prefetched = None;
            if matches!(
                action,
                Action::StopAudio | Action::Broadcast | Action::PasteInvite | Action::Play
            ) {
                live.status = "Подключение отменено.".into();
                return;
            }
        }
        match action {
            Action::QueueOpen => {
                live.queue_editor = Some(aura_core::live::QueueEditor {
                    ids: live
                        .ordered_queue()
                        .iter()
                        .map(|friend| friend.contact.id.clone())
                        .collect(),
                    page: 0,
                    error: None,
                });
            }
            Action::QueueMove(index, up) => {
                if let Some(editor) = &mut live.queue_editor {
                    let target = if up {
                        index.checked_sub(1)
                    } else {
                        index.checked_add(1)
                    };
                    if let Some(target) = target
                        .filter(|target| *target < editor.ids.len() && index < editor.ids.len())
                    {
                        editor.ids.swap(index, target);
                        editor.page = target / 4;
                        editor.error = None;
                    }
                }
            }
            Action::QueuePage(next) => {
                if let Some(editor) = &mut live.queue_editor {
                    let pages = editor.ids.len().div_ceil(4).max(1);
                    editor.page = if next {
                        (editor.page + 1) % pages
                    } else {
                        (editor.page + pages - 1) % pages
                    };
                }
            }
            Action::QueueCancel => live.queue_editor = None,
            Action::QueueSave => {
                if let Some(editor) = &live.queue_editor {
                    let mut order: Vec<_> = editor
                        .ids
                        .iter()
                        .filter(|id| {
                            live.friends.iter().any(|friend| {
                                friend.contact.id == **id
                                    && friend.contact.relation == aura_social::Relation::Accepted
                            })
                        })
                        .cloned()
                        .collect();
                    for friend in live.ordered_queue() {
                        if !order.contains(&friend.contact.id) {
                            order.push(friend.contact.id.clone());
                        }
                    }
                    match self.engine.save_smart_feed_preferences(
                        &live.smart_pinned,
                        &live.smart_excluded,
                        &order,
                    ) {
                        Ok(()) => {
                            live.smart_order = order;
                            live.queue_editor = None;
                            live.smart_prefetch_request = None;
                            live.smart_prefetched = None;
                            live.status =
                                "Порядок сохранён. Он действует при следующем выборе эфира.".into();
                            if live.smart_feed && live.receiving {
                                self.request_smart_prefetch();
                            }
                        }
                        Err(error) => {
                            if let Some(editor) = &mut live.queue_editor {
                                editor.error = Some(error);
                            }
                        }
                    }
                }
            }
            Action::SocialTab(friends) => {
                live.friends_tab = friends;
                live.public.tab = false;
                if live
                    .editor
                    .as_ref()
                    .is_some_and(|e| e.purpose == aura_core::live::EditPurpose::PublicSearch)
                {
                    live.editor = None;
                    live.public.search_open = false;
                    live.public.query.clear();
                }
            }
            Action::PublicTab => {
                live.public.tab = true;
                if !live.capturing && !live.receiving && !live.busy {
                    live.status =
                        "Общий каталог эфиров. Сохраняйте любимых авторов звёздочкой.".into();
                }
                if !live.public.loading {
                    live.public.loading = true;
                    self.engine.refresh_public(self.proxy.clone());
                    self.public_refresh = Some(Instant::now());
                }
            }
            Action::PublicVisibility => {
                if live.toggle_public_visibility(self.engine.is_broadcast_session())
                    == aura_core::live::VisibilityChange::Restart
                {
                    self.start_selected_broadcast();
                }
            }
            Action::PublicTitle => {
                if !live.capturing && !live.busy {
                    let mut editor = aura_core::live::Editor::new(true, live.public.title.clone());
                    editor.purpose = aura_core::live::EditPurpose::PublicTitle;
                    live.editor = Some(editor);
                }
            }
            Action::PublicSearch => {
                live.public.search_open = !live.public.search_open;
                live.public.offset = 0;
                if live.public.search_open {
                    let mut editor = aura_core::live::Editor::new(false, live.public.query.clone());
                    editor.purpose = aura_core::live::EditPurpose::PublicSearch;
                    live.editor = Some(editor);
                } else {
                    live.editor = None;
                    live.public.query.clear();
                }
            }
            Action::PublicFilter(favorites) => {
                live.public.favorites_only = favorites;
                live.public.offset = 0;
            }
            Action::PublicFavorite(index) => {
                if let Some(room) = live.public.visible().get(index) {
                    let mut values = live.public.favorites.clone();
                    if values.iter().any(|(id, _)| id == &room.owner) {
                        values.retain(|(id, _)| id != &room.owner);
                    } else {
                        values.push((room.owner.clone(), room.author.clone()));
                    }
                    match self.engine.save_public_favorites(&values) {
                        Ok(()) => live.public.favorites = values,
                        Err(error) => live.status = error,
                    }
                }
            }
            Action::PublicListen(index) => {
                if let Some(room) = live
                    .public
                    .visible()
                    .get(index)
                    .cloned()
                    .filter(|room| !room.link.is_empty())
                {
                    if live.capturing && room.owner == live.peer_id {
                        live.status =
                            "Это ваш эфир. Другие пользователи могут слушать его из общего списка."
                                .into();
                        return;
                    }
                    live.smart_feed = false;
                    if live.public.listening.as_deref() == Some(&room.owner)
                        && (live.receiving || live.busy)
                    {
                        self.engine.stop();
                        live.stopping = true;
                        live.busy = true;
                    } else if let Ok(invite) = engine::Invitation::parse(&room.link) {
                        live.capturing = false;
                        live.receiving = false;
                        live.pending_friend = false;
                        live.invitation = None;
                        live.media = aura_media::Snapshot::default();
                        live.rms = 0.0;
                        live.reconnecting = false;
                        live.paused_since = None;
                        live.silence_since = None;
                        live.smart_current = None;
                        live.smart_pending_id = None;
                        live.smart_transition_authorized = false;
                        live.smart_transition_output_ready = false;
                        live.smart_listen_request = None;
                        live.smart_prefetch_request = None;
                        live.smart_prefetched = None;
                        self.model.broadcasting = false;
                        self.model.playing = false;
                        live.public.listening = Some(room.owner);
                        live.use_relay = true;
                        live.stopping = false;
                        live.busy = true;
                        live.status = format!("Подключаемся: {}…", room.title);
                        self.engine
                            .start(None, Some(invite), true, self.proxy.clone());
                    }
                }
            }
            Action::PublicScroll(next) => {
                let count = live.public.visible().len();
                let height = self
                    .window
                    .as_ref()
                    .map_or(720.0, |w| w.inner_size().height as f64 / w.scale_factor());
                let max = count
                    .saturating_sub(aura_ui::public_page_size(height, live.public.search_open));
                live.public.offset = if next {
                    (live.public.offset + 1).min(max)
                } else {
                    live.public.offset.saturating_sub(1)
                };
            }
            Action::PublicScrollTo(offset) => live.public.offset = offset,
            Action::AddFriend | Action::RenameProfile => {
                live.editor = Some(aura_core::live::Editor::new(
                    action == Action::RenameProfile,
                    if action == Action::RenameProfile {
                        live.profile_name.clone()
                    } else {
                        String::new()
                    },
                ));
            }
            Action::EditFocus => {
                if let Some(editor) = &mut live.editor {
                    editor.focused = true;
                }
            }
            Action::EditCancel => {
                if live
                    .editor
                    .as_ref()
                    .is_some_and(|e| e.purpose == aura_core::live::EditPurpose::PublicSearch)
                {
                    live.public.search_open = false;
                    live.public.query.clear();
                    live.public.offset = 0;
                }
                live.editor = None;
            }
            Action::EditPaste => {
                if let Some(editor) = &mut live.editor {
                    if let Ok(text) = arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
                        let mut edit = aura_ui::editor_model(editor);
                        edit.insert(&text);
                        editor.value = edit.text().into();
                        editor.caret = edit.caret();
                        editor.anchor = None;
                        if editor.purpose == aura_core::live::EditPurpose::PublicSearch {
                            live.public.query = editor.value.clone();
                            live.public.offset = 0;
                        }
                    }
                }
            }
            Action::EditSave => {
                if let Some(editor) = live.editor.as_ref() {
                    match editor.purpose {
                        aura_core::live::EditPurpose::PublicTitle => {
                            match aura_mesh::public::title(&editor.value) {
                                Ok(value) => {
                                    live.public.title = value;
                                    live.editor = None;
                                }
                                Err(error) => live.status = error.to_string(),
                            }
                            return;
                        }
                        aura_core::live::EditPurpose::PublicSearch => {
                            live.public.query = editor.value.clone();
                            live.public.offset = 0;
                            return;
                        }
                        _ => {}
                    }
                }
                if let Some(editor) = &live.editor {
                    let result = if editor.name {
                        aura_social::name(&editor.value)
                    } else {
                        aura_social::peer_id(&editor.value)
                    };
                    match result {
                        Ok(value) => {
                            self.engine.social(if editor.name {
                                aura_mesh::social::Command::Rename(value)
                            } else {
                                aura_mesh::social::Command::Add(value)
                            });
                            live.editor = None;
                            live.status = "Сохранено. Ожидаем ответ друга.".into();
                        }
                        Err(error) => live.status = error.to_string(),
                    }
                }
            }
            Action::FriendPage(next) => {
                let pages = live.friends.len().div_ceil(3).max(1);
                live.friend_page = if next {
                    (live.friend_page + 1) % pages
                } else {
                    (live.friend_page + pages - 1) % pages
                };
            }
            Action::SmartFeedPin(i) => {
                if let Some(id) = live
                    .friends
                    .get(i)
                    .filter(|friend| friend.contact.relation == aura_social::Relation::Accepted)
                    .map(|friend| friend.contact.id.clone())
                {
                    if live.smart_pinned.contains(&id) {
                        live.smart_pinned.retain(|pinned| pinned != &id);
                        live.status = "Приоритет друга в Smart Feed снят.".into();
                    } else {
                        live.smart_pinned.push(id.clone());
                        live.smart_excluded.retain(|excluded| excluded != &id);
                        live.status = "Друг закреплён в приоритете Smart Feed.".into();
                    }
                    if let Err(error) = self.engine.save_smart_feed_preferences(
                        &live.smart_pinned,
                        &live.smart_excluded,
                        &live.smart_order,
                    ) {
                        live.status = error;
                    }
                }
            }
            Action::SmartFeedExclude(i) => {
                if let Some(id) = live
                    .friends
                    .get(i)
                    .filter(|friend| friend.contact.relation == aura_social::Relation::Accepted)
                    .map(|friend| friend.contact.id.clone())
                {
                    if live.smart_excluded.contains(&id) {
                        live.smart_excluded.retain(|excluded| excluded != &id);
                        live.status = "Друг возвращён в Smart Feed.".into();
                    } else {
                        live.smart_excluded.push(id.clone());
                        live.smart_pinned.retain(|pinned| pinned != &id);
                        live.status = "Друг исключён из Smart Feed.".into();
                    }
                    if let Err(error) = self.engine.save_smart_feed_preferences(
                        &live.smart_pinned,
                        &live.smart_excluded,
                        &live.smart_order,
                    ) {
                        live.status = error;
                    }
                }
            }
            Action::FriendAccept(i) => {
                if let Some(friend) = live.friends.get(i) {
                    self.engine.social(aura_mesh::social::Command::Accept(
                        friend.contact.id.clone(),
                    ));
                }
            }
            Action::FriendRemoveConfirm => {
                if let Some((friend_id, _)) = live.remove_friend.take() {
                    self.engine
                        .social(aura_mesh::social::Command::Remove(friend_id.clone()));
                    live.smart_pinned.retain(|id| id != &friend_id);
                    live.smart_excluded.retain(|id| id != &friend_id);
                    live.smart_order.retain(|id| id != &friend_id);
                    if let Err(error) = self.engine.save_smart_feed_preferences(
                        &live.smart_pinned,
                        &live.smart_excluded,
                        &live.smart_order,
                    ) {
                        live.status = error;
                    }
                    if live.capturing || live.receiving || live.busy {
                        self.engine.stop();
                        live.invitation = None;
                        live.stopping = true;
                        live.busy = true;
                        live.status =
                            "Контакт удалён. Текущая сессия останавливается для отзыва доступа."
                                .into();
                    }
                }
            }
            Action::FriendListen(i) => {
                if let Some((friend_id, friend_name)) = live
                    .friends
                    .get(i)
                    .map(|friend| (friend.contact.id.clone(), friend.contact.name.clone()))
                {
                    if live.receiving && live.smart_current.as_deref() == Some(friend_id.as_str()) {
                        live.smart_feed = false;
                        live.smart_pending_id = None;
                        live.smart_listen_request = None;
                        live.smart_prefetch_request = None;
                        live.smart_prefetched = None;
                        live.pending_friend = false;
                        live.paused_since = None;
                        live.silence_since = None;
                        self.engine.stop();
                        live.stopping = true;
                        live.busy = true;
                        live.invitation = None;
                        live.status = "Останавливаем прослушивание…".into();
                        self.model.playing = false;
                        return;
                    }
                    live.smart_feed = false;
                    live.smart_attempted.clear();
                    live.smart_current = Some(friend_id.clone());
                    live.smart_pending_id = None;
                    live.smart_listen_request = None;
                    live.paused_since = None;
                    live.silence_since = None;
                    live.smart_prefetch_request = None;
                    live.smart_prefetched = None;
                    self.engine.invalidate();
                    self.model.broadcasting = false;
                    self.model.playing = false;
                    live.stopping = false;
                    live.busy = true;
                    live.capturing = false;
                    live.receiving = false;
                    live.invitation = None;
                    live.pending_friend = true;
                    live.status = format!("Проверяем эфир {friend_name}…");
                    self.engine.social(aura_mesh::social::Command::Listen(
                        friend_id,
                        self.engine.generation,
                    ));
                }
            }
            Action::SmartFeed => {
                if live.smart_feed {
                    let was_pending = live.pending_friend;
                    live.smart_feed = false;
                    live.smart_attempted.clear();
                    live.smart_pending_id = None;
                    live.smart_listen_request = None;
                    live.smart_prefetch_request = None;
                    live.smart_prefetched = None;
                    live.paused_since = None;
                    live.silence_since = None;
                    live.pending_friend = false;
                    if was_pending {
                        self.engine.invalidate();
                        live.stopping = false;
                        live.busy = false;
                        live.status = "Smart Feed остановлен.".into();
                    } else if live.receiving || live.busy {
                        self.engine.stop();
                        live.stopping = true;
                        live.busy = true;
                        live.status = "Останавливаем Smart Feed…".into();
                    } else {
                        live.status = "Smart Feed остановлен.".into();
                    }
                } else {
                    let keep_current = live.receiving;
                    live.smart_feed = true;
                    live.smart_attempted.clear();
                    live.smart_prefetch_request = None;
                    live.smart_prefetched = None;
                    if keep_current {
                        if let Some(current) = &live.smart_current {
                            live.smart_attempted.push(current.clone());
                        }
                    } else {
                        live.smart_current = None;
                    }
                    live.paused_since = None;
                    live.silence_since = None;
                    if keep_current {
                        self.request_smart_prefetch();
                    } else {
                        self.request_smart_next();
                    }
                }
            }
            Action::CopyId => {
                let result = arboard::Clipboard::new()
                    .and_then(|mut c| c.set_text(self.engine.peer_id.clone()));
                live.status = if result.is_ok() {
                    "Постоянный Aura ID скопирован.".into()
                } else {
                    "Не удалось скопировать Aura ID.".into()
                };
            }
            Action::CopyTrack => {
                let copied = live.media.info.as_ref().and_then(|info| {
                    let value = match (info.title.trim(), info.artist.trim()) {
                        ("", "") => return None,
                        ("", artist) => artist.to_owned(),
                        (title, "") => title.to_owned(),
                        (title, artist) => format!("{title} — {artist}"),
                    };
                    Some(
                        arboard::Clipboard::new()
                            .and_then(|mut clipboard| clipboard.set_text(value))
                            .is_ok(),
                    )
                });
                live.status = match copied {
                    Some(true) => "Название и исполнитель скопированы.".into(),
                    Some(false) => "Не удалось скопировать трек.".into(),
                    None => "Нет названия или исполнителя для копирования.".into(),
                };
            }
            Action::ShareTrack => {
                let text = live.media.info.as_ref().map(|info| {
                    match (info.title.trim(), info.artist.trim()) {
                        ("", "") => "Музыка рядом с Aura".to_owned(),
                        ("", artist) => artist.to_owned(),
                        (title, "") => title.to_owned(),
                        (title, artist) => format!("{title} — {artist}"),
                    }
                });
                #[cfg(target_os = "windows")]
                let shared = text.as_deref().is_some_and(|text| {
                    share::show(
                        &mut self.share,
                        self.window.as_deref(),
                        "Трек из Aura",
                        text,
                    )
                    .is_ok()
                });
                #[cfg(not(target_os = "windows"))]
                let shared = false;
                if !shared {
                    if let Some(text) = &text {
                        let _ = arboard::Clipboard::new()
                            .and_then(|mut clipboard| clipboard.set_text(text.clone()));
                    }
                }
                live.status = if text.is_none() {
                    "Нет названия или исполнителя для отправки.".into()
                } else if shared {
                    "Выберите приложение, чтобы поделиться треком.".into()
                } else {
                    "Меню отправки недоступно. Трек скопирован.".into()
                };
            }
            Action::Route => {
                if live.capturing || live.receiving || live.busy {
                    live.status = "Сначала остановите текущую сессию.".into();
                } else {
                    live.use_relay = !live.use_relay;
                    live.status = if live.use_relay {
                        "Выбран Aura Node: проброс портов не требуется.".into()
                    } else {
                        "Выбрано прямое UDP-соединение: нужен доступный порт вещателя.".into()
                    };
                }
            }
            Action::Broadcast => {
                if live.capturing || live.busy || live.receiving {
                    live.smart_feed = false;
                    live.paused_since = None;
                    live.silence_since = None;
                    self.engine.stop();
                    live.stopping = true;
                    live.busy = true;
                    live.invitation = None;
                    live.status = "Останавливаем захват и соединение…".into();
                } else {
                    self.start_selected_broadcast();
                }
            }
            Action::StopAudio => {
                live.smart_feed = false;
                live.smart_attempted.clear();
                live.smart_pending_id = None;
                live.smart_listen_request = None;
                live.smart_prefetch_request = None;
                live.smart_prefetched = None;
                live.paused_since = None;
                live.silence_since = None;
                self.engine.stop();
                live.stopping = true;
                live.busy = true;
                live.invitation = None;
                live.status = "Останавливаем соединение…".into();
            }
            Action::Source(i) => {
                if let Some(source) = live.sources.get(i) {
                    if live.capturing || live.busy || live.receiving {
                        self.engine.stop();
                        live.stopping = true;
                        live.busy = true;
                        live.invitation = None;
                    }
                    if live.selected.as_ref().is_some_and(|selected| {
                        selected.pid == source.pid && selected.started == source.started
                    }) {
                        live.selected = None;
                        live.status = "Выбор источника снят.".into();
                    } else {
                        live.selected = Some(source.clone());
                        live.status =
                            "Источник выбран. Нажмите «В эфир», чтобы начать захват.".into();
                    }
                }
            }
            Action::SourcePage(next) => {
                let pages = live.sources.len().div_ceil(3).max(1);
                live.page = if next {
                    (live.page + 1) % pages
                } else {
                    (live.page + pages - 1) % pages
                };
            }
            Action::CopyInvite => {
                if let Some(invite) = &live.invitation {
                    let result =
                        arboard::Clipboard::new().and_then(|mut c| c.set_text(invite.clone()));
                    live.status = if result.is_ok() {
                        "Приглашение скопировано. Доступ действует до остановки эфира.".into()
                    } else {
                        "Не удалось скопировать приглашение.".into()
                    };
                }
            }
            Action::PasteInvite | Action::Play => {
                if live.receiving || live.capturing || live.busy {
                    self.engine.stop();
                    live.stopping = true;
                    live.busy = true;
                    live.invitation = None;
                    live.status = "Останавливаем соединение…".into();
                    return;
                }
                let invitation = arboard::Clipboard::new()
                    .and_then(|mut c| c.get_text())
                    .map_err(|_| "Не удалось прочитать приглашение из буфера обмена.".to_string())
                    .and_then(|s| {
                        engine::Invitation::parse(&s).map_err(|_| {
                            "Неверное приглашение. Скопируйте ссылку aura://… целиком.".to_string()
                        })
                    });
                match invitation {
                    Ok(invitation) => {
                        if let engine::Invitation::Public(link) = &invitation {
                            live.public.listening = Some(link.owner.clone());
                            live.smart_feed = false;
                        }
                        live.use_relay = invitation.uses_relay();
                        live.stopping = false;
                        live.busy = true;
                        live.frames = 0;
                        live.dropped = 0;
                        live.audio_dropped = 0;
                        live.concealed = 0;
                        live.buffer_ms = 0;
                        live.rtt_ms = 0;
                        live.status = format!("Подключение к {}…", invitation.address());
                        self.engine.start(
                            None,
                            Some(invitation),
                            live.use_relay,
                            self.proxy.clone(),
                        );
                    }
                    Err(error) => live.status = error,
                }
            }
            Action::Volume(value) => {
                self.model.volume = value.min(100);
                self.engine.volume(self.model.volume);
            }
            _ => {}
        }
    }
    fn engine_event(&mut self, id: u64, event: engine::Event) {
        if id != self.engine.generation {
            return;
        }
        let transition_failed = matches!(event, engine::Event::Ended(_))
            && self
                .model
                .live
                .as_ref()
                .is_some_and(|live| live.smart_pending_id.is_some())
            && self.engine.cancel_transition();
        let Some(live) = &mut self.model.live else {
            return;
        };
        if live.stopping
            && matches!(
                event,
                engine::Event::CaptureReady(_)
                    | engine::Event::Authorized
                    | engine::Event::PlaybackReady
                    | engine::Event::Reconnecting(_)
            )
        {
            return;
        }
        let mut retry_smart = false;
        let mut prefetch_smart = false;
        match event {
            engine::Event::PublicMode(value) => {
                live.public.active = value;
                if !value {
                    live.public.listening = None;
                }
            }
            engine::Event::Metadata(snapshot) => {
                if !live.stopping && (live.busy || live.capturing || live.receiving) {
                    let paused = snapshot
                        .info
                        .as_ref()
                        .is_some_and(|info| info.playback == aura_media::Playback::Paused);
                    if live.smart_feed && live.receiving && !live.reconnecting && paused {
                        live.paused_since.get_or_insert_with(Instant::now);
                    } else {
                        live.paused_since = None;
                    }
                    live.media = snapshot;
                }
            }
            engine::Event::CaptureReady(link) => {
                live.reconnecting = false;
                if !live.public.active {
                    self.engine
                        .social(aura_mesh::social::Command::Publish(Some(link.clone())));
                }
                live.capturing = true;
                live.busy = false;
                live.invitation = Some(link);
                live.status = if live.public.active {
                    "Публичный эфир доступен в общем списке. Раздачу выполняет Aura Node.".into()
                } else if live.use_relay {
                    "Эфир через Aura Node. Пригласите друга кнопкой выше.".into()
                } else {
                    "Прямой эфир включён. Пригласите друга кнопкой выше.".into()
                };
                self.model.broadcasting = true;
            }
            engine::Event::Authorized => {
                live.reconnecting = false;
                live.receiving = true;
                live.busy = false;
                if let Some(friend_id) = live.smart_pending_id.take() {
                    live.smart_current = Some(friend_id.clone());
                    if !live.smart_attempted.contains(&friend_id) {
                        live.smart_attempted.push(friend_id);
                    }
                    live.paused_since = None;
                    live.silence_since = None;
                    live.media = aura_media::Snapshot::default();
                    live.rms = 0.0;
                    live.smart_transition_authorized = true;
                    if live.smart_transition_output_ready {
                        live.smart_transition_authorized = false;
                        live.smart_transition_output_ready = false;
                        self.engine.complete_transition();
                    }
                }
                prefetch_smart = live.smart_feed && smart_candidate(live).is_some();
                live.status = if live.public.active {
                    "Публичный эфир. Соединение с узлом зашифровано; узел раздаёт аудио.".into()
                } else if live.use_relay {
                    "Соединение через Aura Node. Идентичность вещателя проверена, звук зашифрован сквозным Noise.".into()
                } else {
                    "Прямое соединение установлено. Сертификат эфира проверен.".into()
                };
                self.model.playing = true;
            }
            engine::Event::PlaybackReady => {
                if live.smart_pending_id.is_some() || live.smart_transition_authorized {
                    live.smart_transition_output_ready = true;
                }
                if live.smart_transition_authorized && live.smart_transition_output_ready {
                    live.smart_transition_authorized = false;
                    live.smart_transition_output_ready = false;
                    self.engine.complete_transition();
                }
            }
            engine::Event::Reconnecting(recovering) => {
                live.reconnecting = recovering;
                live.paused_since = None;
                live.silence_since = None;
                live.rms = 0.0;
                live.status = if recovering {
                    "Восстанавливаем эфир через доступный Aura Node…".into()
                } else if live.public.active {
                    "Публичный эфир восстановлен через Aura Node.".into()
                } else if live.capturing {
                    "Эфир восстановлен. Приглашение продолжает работать.".into()
                } else {
                    "Соединение восстановлено. Звук зашифрован сквозным Noise.".into()
                };
            }
            engine::Event::Audio(stats) => {
                live.audio_dropped = stats.dropped;
                live.rms = stats.rms;
                live.frames = stats.frames;
                live.concealed = stats.concealed;
                live.buffer_ms = stats.buffer_ms;
                if live.smart_feed && live.receiving && !live.reconnecting {
                    if stats.rms < 0.001 {
                        live.silence_since.get_or_insert_with(Instant::now);
                    } else if stats.rms >= 0.0015 {
                        live.silence_since = None;
                    }
                } else {
                    live.silence_since = None;
                }
            }
            engine::Event::Network(stats) => {
                live.listeners = stats.listeners;
                live.dropped = stats.dropped;
                live.rtt_ms = stats.rtt_ms;
            }
            engine::Event::Ended(error) => {
                live.public.active = false;
                live.public.listening = None;
                live.reconnecting = false;
                if transition_failed {
                    live.smart_transition_authorized = false;
                    live.smart_transition_output_ready = false;
                    if let Some(failed_id) = live.smart_pending_id.take() {
                        if !live.smart_attempted.contains(&failed_id) {
                            live.smart_attempted.push(failed_id);
                        }
                    }
                    live.pending_friend = false;
                    live.busy = false;
                    live.status =
                        "Не удалось подключиться к следующему эфиру; продолжаем текущий.".into();
                    retry_smart = live.smart_feed;
                } else {
                    live.media = aura_media::Snapshot::default();
                    self.engine
                        .social(aura_mesh::social::Command::Publish(None));
                    live.stopping = false;
                    live.capturing = false;
                    live.receiving = false;
                    live.busy = false;
                    live.invitation = None;
                    live.rms = 0.0;
                    live.listeners = 0;
                    live.status = error.unwrap_or_else(|| {
                        "Захват и соединение остановлены. Приглашение отозвано.".into()
                    });
                    live.paused_since = None;
                    live.silence_since = None;
                    retry_smart = live.smart_feed;
                    self.model.broadcasting = false;
                    self.model.playing = false;
                }
            }
        }
        if retry_smart {
            self.request_smart_next();
        } else if prefetch_smart {
            self.request_smart_prefetch();
        }
        if let Some(tray) = &self.tray {
            tray.update(&self.model);
        }
        self.redraw();
    }
    fn show(&mut self) {
        self.visible = true;
        self.occluded = false;
        if let Some(w) = &self.window {
            w.set_visible(true);
            w.focus_window();
            w.request_redraw();
        }
        self.last_tick = Instant::now();
    }
    fn redraw(&self) {
        if self.visible && !self.occluded {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }
    fn paint(&mut self) -> Result<(), Box<dyn Error>> {
        let Some(window) = &self.window else {
            return Ok(());
        };
        let size = window.inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return Ok(());
        };
        let scale = window.scale_factor();
        let tooltip = self
            .hovered
            .as_ref()
            .filter(|(_, _, since)| {
                since.elapsed() >= Duration::from_millis(550)
                    && !self.volume_dragging
                    && !self.editor_drag
                    && self.pressed.is_none()
            })
            .map(|(_, label, _)| label.as_str());
        let frame = aura_ui::compose_with_tooltip(
            &self.model,
            &mut self.text,
            size.width as f64 / scale,
            size.height as f64 / scale,
            self.model.elapsed.as_secs_f64(),
            self.focus,
            self.pointer,
            tooltip,
        );
        self.controls = frame.controls;
        if let Some(editor) = self
            .model
            .live
            .as_mut()
            .and_then(|live| live.editor.as_mut())
        {
            if let Some(scroll) = frame.editor_scroll {
                editor.scroll = scroll;
            }
        }
        self.raster
            .render(
                &frame.paint,
                &mut self.text,
                DpiScale::new(scale).map_err(|e| format!("DPI: {e:?}"))?,
                PhysicalSize::new(size.width as f64, size.height as f64),
            )
            .map_err(|e| format!("Raster: {e:?}"))?;
        let surface = self.surface.as_mut().ok_or("No window surface")?;
        surface.resize(width, height)?;
        let mut buffer = surface.buffer_mut()?;
        for (pixel, rgba) in buffer
            .iter_mut()
            .zip(self.raster.pixels().as_chunks::<4>().0.iter())
        {
            *pixel = (u32::from(rgba[0]) << 16) | (u32::from(rgba[1]) << 8) | u32::from(rgba[2]);
        }
        buffer.present()?;
        Ok(())
    }
}

impl ApplicationHandler<UserEvent> for Desktop {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let result = (|| -> Result<(), Box<dyn Error>> {
            let window = Arc::new(
                event_loop.create_window(
                    Window::default_attributes()
                        .with_title("Aura")
                        .with_inner_size(LogicalSize::new(380.0, 720.0))
                        .with_min_inner_size(LogicalSize::new(380.0, 720.0)),
                )?,
            );
            let context = softbuffer::Context::new(window.clone())?;
            self.surface = Some(softbuffer::Surface::new(&context, window.clone())?);
            self.window = Some(window);
            match tray::Tray::new(self.proxy.clone()) {
                Ok(tray) => self.tray = Some(tray),
                Err(error) => eprintln!("Tray unavailable; closing the window will exit: {error}"),
            }
            if let Some(tray) = &self.tray {
                tray.update(&self.model);
            }
            if self.model.live.is_some() {
                self.scan_sources();
            }
            self.redraw();
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("Cannot open Aura: {error}");
            event_loop.exit();
        }
    }
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Social(event) => {
                let mut retry_smart = false;
                let mut prefetch_smart = false;
                let mut smart_switch = None;
                if let Some(live) = &mut self.model.live {
                    match event {
                        aura_mesh::social::Event::Snapshot(snapshot) => {
                            live.profile_name = snapshot.name;
                            live.social_connected = snapshot.connected;
                            live.nodes_connected = snapshot.nodes_connected;
                            live.nodes_total = snapshot.nodes_total;
                            let incoming = snapshot
                                .friends
                                .into_iter()
                                .map(|f| aura_core::live::Friend {
                                    contact: f.contact,
                                    online: f.online,
                                    live: f.live,
                                })
                                .collect::<Vec<_>>();
                            let old_preferences = (
                                live.smart_pinned.len(),
                                live.smart_excluded.len(),
                                live.smart_order.len(),
                            );
                            live.smart_pinned.retain(|id| {
                                incoming.iter().any(|friend| friend.contact.id == *id)
                            });
                            live.smart_excluded.retain(|id| {
                                incoming.iter().any(|friend| friend.contact.id == *id)
                            });
                            live.smart_order.retain(|id| {
                                incoming.iter().any(|friend| friend.contact.id == *id)
                            });
                            if old_preferences
                                != (
                                    live.smart_pinned.len(),
                                    live.smart_excluded.len(),
                                    live.smart_order.len(),
                                )
                            {
                                if let Err(error) = self.engine.save_smart_feed_preferences(
                                    &live.smart_pinned,
                                    &live.smart_excluded,
                                    &live.smart_order,
                                ) {
                                    live.status = error;
                                }
                            }
                            live.smart_attempted.retain(|id| {
                                incoming
                                    .iter()
                                    .any(|friend| &friend.contact.id == id && friend.live)
                            });
                            let friend_is_live = |id: &str| {
                                incoming.iter().any(|friend| {
                                    friend.contact.id == id
                                        && friend.contact.relation
                                            == aura_social::Relation::Accepted
                                        && friend.online
                                        && friend.live
                                })
                            };
                            if live
                                .smart_prefetched
                                .as_ref()
                                .is_some_and(|(id, _)| !friend_is_live(id))
                            {
                                live.smart_prefetched = None;
                            }
                            if live
                                .smart_prefetch_request
                                .as_ref()
                                .is_some_and(|(_, id)| !friend_is_live(id))
                            {
                                live.smart_prefetch_request = None;
                            }
                            if live
                                .smart_listen_request
                                .as_ref()
                                .is_some_and(|(_, id)| !friend_is_live(id))
                            {
                                live.smart_listen_request = None;
                                live.smart_pending_id = None;
                                live.pending_friend = false;
                                live.busy = false;
                            }
                            live.friends = incoming;
                            live.friend_page = live
                                .friend_page
                                .min(live.friends.len().saturating_sub(1) / 3);
                            let has_next = smart_candidate(live).is_some();
                            retry_smart =
                                live.smart_feed && !live.busy && !live.receiving && has_next;
                            prefetch_smart = live.smart_feed && live.receiving && has_next;
                        }
                        aura_mesh::social::Event::Notice(error) => live.status = error,
                        aura_mesh::social::Event::Listen(id, result)
                            if live
                                .smart_listen_request
                                .as_ref()
                                .is_some_and(|(request, _)| *request == id) =>
                        {
                            let (_, friend_id) = live.smart_listen_request.take().unwrap();
                            live.smart_pending_id = None;
                            live.pending_friend = false;
                            live.busy = false;
                            match result.and_then(|link| {
                                engine::Invitation::parse(&link).map_err(|error| error.to_string())
                            }) {
                                Ok(invite) if live.smart_feed => {
                                    smart_switch = Some((friend_id, invite));
                                }
                                Ok(_) => {}
                                Err(_) if live.smart_feed => {
                                    if !live.smart_attempted.contains(&friend_id) {
                                        live.smart_attempted.push(friend_id);
                                    }
                                    live.status =
                                        "Эфир недоступен. Smart Feed ищет следующего друга…".into();
                                    retry_smart = true;
                                }
                                Err(error) => live.status = error,
                            }
                        }
                        aura_mesh::social::Event::Listen(id, result)
                            if live
                                .smart_prefetch_request
                                .as_ref()
                                .is_some_and(|(request, _)| *request == id) =>
                        {
                            let (_, friend_id) = live.smart_prefetch_request.take().unwrap();
                            if live.smart_feed
                                && live.receiving
                                && smart_candidate(live)
                                    .is_some_and(|(candidate, _)| candidate == friend_id)
                            {
                                if let Ok(invitation) = result {
                                    live.smart_prefetched = Some((friend_id, invitation));
                                }
                            }
                        }
                        aura_mesh::social::Event::Listen(id, result)
                            if id == self.engine.generation && live.pending_friend =>
                        {
                            live.pending_friend = false;
                            live.busy = false;
                            match result.and_then(|link| {
                                engine::Invitation::parse(&link).map_err(|e| e.to_string())
                            }) {
                                Ok(invite) => {
                                    let use_relay = invite.uses_relay();
                                    live.busy = true;
                                    live.frames = 0;
                                    live.rms = 0.0;
                                    live.dropped = 0;
                                    live.audio_dropped = 0;
                                    live.concealed = 0;
                                    live.buffer_ms = 0;
                                    live.rtt_ms = 0;
                                    live.use_relay = use_relay;
                                    live.status = "Подключение к эфиру друга…".into();
                                    self.engine.start(
                                        None,
                                        Some(invite),
                                        use_relay,
                                        self.proxy.clone(),
                                    );
                                }
                                Err(error) => {
                                    if live.smart_feed {
                                        live.status =
                                            "Эфир недоступен. Smart Feed ищет следующего друга…"
                                                .into();
                                        retry_smart = true;
                                    } else {
                                        live.status = error;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if let Some((friend_id, invite)) = smart_switch {
                    self.start_smart_invitation(friend_id, invite);
                } else if retry_smart {
                    self.request_smart_next();
                } else if prefetch_smart {
                    self.request_smart_prefetch();
                }
                self.redraw();
            }
            UserEvent::Engine(id, event) => self.engine_event(id, event),
            UserEvent::PublicCatalog(result) => {
                self.pressed = None;
                if let Some(live) = self.model.live.as_mut() {
                    live.public.loading = false;
                    match result {
                        Ok((rooms, count)) => {
                            for (id, name) in &mut live.public.favorites {
                                if let Some(room) = rooms.iter().find(|r| &r.owner == id) {
                                    *name = room.author.clone();
                                }
                            }
                            live.public.rooms = rooms;
                            live.public.available = count;
                            live.public.error = None;
                        }
                        Err(error) => {
                            live.public.rooms.clear();
                            live.public.available = 0;
                            live.public.error = Some(error);
                        }
                    }
                }
                self.redraw();
            }
            UserEvent::Sources(result) => {
                if let Some(live) = &mut self.model.live {
                    live.scanning = false;
                    match result {
                        Ok(sources) => {
                            live.sources = sources;
                            live.page = live
                                .page
                                .min(live.sources.len().div_ceil(3).saturating_sub(1));
                            if live
                                .selected
                                .as_ref()
                                .is_some_and(|s| !live.sources.contains(s))
                            {
                                live.selected = None;
                                self.engine.stop();
                                if live.capturing || live.receiving || live.busy {
                                    live.stopping = true;
                                    live.busy = true;
                                }
                                live.invitation = None;
                            }
                        }
                        Err(error) => live.status = error,
                    }
                }
                self.redraw();
            }
            UserEvent::Show => self.show(),
            UserEvent::Menu(id) => match id.as_str() {
                "show" => self.show(),
                "broadcast" => self.apply(Action::Broadcast),
                "play" => self.apply(Action::Play),
                "next" => self.apply(Action::Next),
                "quit" => {
                    self.engine.stop();
                    self.model.broadcasting = false;
                    self.model.playing = false;
                    event_loop.exit();
                }
                _ => {}
            },
        }
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                if self.tray.is_some() {
                    self.visible = false;
                    if let Some(w) = &self.window {
                        w.set_visible(false);
                    }
                } else {
                    event_loop.exit();
                }
            }
            WindowEvent::Occluded(value) => {
                self.occluded = value;
                if !value {
                    self.redraw();
                }
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => self.redraw(),
            WindowEvent::RedrawRequested if self.visible && !self.occluded => {
                if let Err(error) = self.paint() {
                    eprintln!("Cannot paint Aura: {error}");
                    event_loop.exit();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self.window.as_ref().map_or(1.0, |w| w.scale_factor());
                self.pointer = Some(LogicalPoint::new(position.x / scale, position.y / scale));
                self.update_hover();
                if self.volume_dragging {
                    self.update_volume_from_pointer();
                }
                if self.public_scroll_dragging {
                    self.update_public_scroll();
                }
                if self.editor_drag {
                    self.editor_click(true);
                }
                let hit = self
                    .controls
                    .iter()
                    .any(|c| self.pointer.is_some_and(|p| c.rect.contains(p)));
                if let Some(w) = &self.window {
                    let text_field = self.controls.iter().any(|c| {
                        c.action == Action::EditFocus
                            && self.pointer.is_some_and(|p| c.rect.contains(p))
                    });
                    w.set_cursor(if text_field {
                        winit::window::CursorIcon::Text
                    } else if hit {
                        winit::window::CursorIcon::Pointer
                    } else {
                        winit::window::CursorIcon::Default
                    });
                }
                self.redraw();
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer = None;
                self.hovered = None;
                self.tooltip_visible = false;
                self.pressed = None;
                self.volume_dragging = false;
                self.public_scroll_dragging = false;
                self.redraw();
            }
            WindowEvent::Focused(false) => {
                self.hovered = None;
                self.tooltip_visible = false;
                self.pressed = None;
                self.editor_drag = false;
                self.volume_dragging = false;
                self.modifiers = ModifiersState::empty();
                self.public_scroll_dragging = false;
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let hit = self
                    .controls
                    .iter()
                    .position(|c| self.pointer.is_some_and(|p| c.rect.contains(p)));
                if state == ElementState::Pressed {
                    self.hovered = None;
                    self.tooltip_visible = false;
                    if hit.is_some_and(|i| self.controls[i].action == Action::VolumeSlider) {
                        self.volume_dragging = true;
                        self.pressed = None;
                        self.update_volume_from_pointer();
                        return;
                    }
                    if hit.is_some_and(|i| self.controls[i].action == Action::PublicScrollThumb) {
                        self.public_scroll_dragging = true;
                        self.pressed = None;
                        self.update_public_scroll();
                        return;
                    }
                    if hit.is_some_and(|i| self.controls[i].action == Action::EditFocus) {
                        self.editor_click(self.modifiers.shift_key());
                        self.editor_drag = true;
                        self.pressed = None;
                        self.redraw();
                        return;
                    }
                    if let Some(editor) = self
                        .model
                        .live
                        .as_mut()
                        .and_then(|live| live.editor.as_mut())
                    {
                        editor.focused = false;
                    }
                    self.pressed = hit.map(|i| (i, self.controls[i].action));
                } else if let Some((_, action)) = self.pressed.take().filter(|(i, action)| {
                    Some(*i) == hit
                        && self
                            .controls
                            .get(*i)
                            .is_some_and(|control| control.action == *action)
                }) {
                    self.apply(action);
                }
                if state == ElementState::Released {
                    self.public_scroll_dragging = false;
                    self.editor_drag = false;
                    self.volume_dragging = false;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.model.live.as_ref().is_some_and(|live| {
                    live.public.tab
                        && live.queue_editor.is_none()
                        && live.remove_friend.is_none()
                        && live
                            .editor
                            .as_ref()
                            .is_none_or(|e| e.purpose == aura_core::live::EditPurpose::PublicSearch)
                }) && self.pointer.is_some_and(|p| p.y.0 >= 425.0)
                {
                    let y = match delta {
                        winit::event::MouseScrollDelta::LineDelta(_, y) => y as f64,
                        winit::event::MouseScrollDelta::PixelDelta(p) => p.y,
                    };
                    if y != 0.0 {
                        self.apply(Action::PublicScroll(y < 0.0));
                        self.redraw();
                    }
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                if self
                    .model
                    .live
                    .as_ref()
                    .and_then(|live| live.editor.as_ref())
                    .is_some_and(|editor| editor.focused)
                {
                    self.edit_text(|edit| edit.insert(&text));
                    self.redraw();
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let modal_cancel = self.model.live.as_ref().and_then(|live| {
                    if live.remove_friend.is_some() {
                        Some(Action::FriendRemoveCancel)
                    } else if live.queue_editor.is_some() {
                        Some(Action::QueueCancel)
                    } else {
                        None
                    }
                });
                if let Some(cancel) = modal_cancel {
                    match event.logical_key {
                        Key::Named(NamedKey::Escape) => self.apply(cancel),
                        Key::Named(NamedKey::Tab) => {
                            let count = self.controls.len();
                            if count > 0 {
                                self.focus = Some(self.focus.map_or(0, |index| {
                                    if self.modifiers.shift_key() {
                                        (index + count - 1) % count
                                    } else {
                                        (index + 1) % count
                                    }
                                }));
                            }
                        }
                        Key::Named(NamedKey::Enter | NamedKey::Space) if !event.repeat => {
                            let action = self
                                .focus
                                .and_then(|index| self.controls.get(index))
                                .map(|control| control.action)
                                .filter(|action| {
                                    if cancel == Action::QueueCancel {
                                        matches!(
                                            action,
                                            Action::QueueMove(_, _)
                                                | Action::QueuePage(_)
                                                | Action::QueueSave
                                                | Action::QueueCancel
                                        )
                                    } else {
                                        matches!(
                                            action,
                                            Action::FriendRemoveConfirm
                                                | Action::FriendRemoveCancel
                                        )
                                    }
                                })
                                .unwrap_or(cancel);
                            self.apply(action);
                        }
                        _ => {}
                    }
                    self.redraw();
                    return;
                }
                if self
                    .model
                    .live
                    .as_ref()
                    .is_some_and(|live| live.editor.is_some())
                {
                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => self.apply(Action::EditCancel),
                        Key::Named(NamedKey::Tab) => {
                            let count = self.controls.len();
                            if count > 0 {
                                let next = self.focus.map_or(0, |i| {
                                    if self.modifiers.shift_key() {
                                        (i + count - 1) % count
                                    } else {
                                        (i + 1) % count
                                    }
                                });
                                self.focus = Some(next);
                                if let Some(editor) = self
                                    .model
                                    .live
                                    .as_mut()
                                    .and_then(|live| live.editor.as_mut())
                                {
                                    editor.focused =
                                        self.controls[next].action == Action::EditFocus;
                                }
                                if let Some(w) = &self.window {
                                    w.set_ime_allowed(
                                        self.controls[next].action == Action::EditFocus,
                                    );
                                }
                            }
                        }
                        Key::Named(NamedKey::Enter) => {
                            if self
                                .model
                                .live
                                .as_ref()
                                .and_then(|live| live.editor.as_ref())
                                .is_some_and(|editor| editor.focused)
                            {
                                self.apply(Action::EditSave);
                                self.redraw();
                                return;
                            }
                            let action = self.focus.and_then(|i| self.controls.get(i)).map_or(
                                Action::EditSave,
                                |c| {
                                    if c.action == Action::EditFocus {
                                        Action::EditSave
                                    } else {
                                        c.action
                                    }
                                },
                            );
                            self.apply(action);
                        }
                        _ if self
                            .model
                            .live
                            .as_ref()
                            .and_then(|live| live.editor.as_ref())
                            .is_some_and(|editor| editor.focused) =>
                        {
                            let control = self.modifiers.control_key();
                            if control
                                && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyV))
                            {
                                self.apply(Action::EditPaste);
                            } else if control
                                && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyA))
                            {
                                self.edit_text(|edit| edit.select_all());
                            } else if control
                                && matches!(
                                    event.physical_key,
                                    PhysicalKey::Code(KeyCode::KeyC | KeyCode::KeyX)
                                )
                            {
                                let selected = self
                                    .model
                                    .live
                                    .as_ref()
                                    .and_then(|live| live.editor.as_ref())
                                    .and_then(|editor| {
                                        aura_ui::editor_model(editor)
                                            .selected_text()
                                            .map(str::to_owned)
                                    });
                                if let Some(selected) = selected {
                                    if arboard::Clipboard::new()
                                        .and_then(|mut clipboard| clipboard.set_text(selected))
                                        .is_ok()
                                        && event.physical_key == PhysicalKey::Code(KeyCode::KeyX)
                                    {
                                        self.edit_text(|edit| edit.delete(false));
                                    }
                                }
                            } else {
                                let editing_key = match event.logical_key {
                                    Key::Named(NamedKey::Backspace) => {
                                        Some(everty_ui::Key::Backspace)
                                    }
                                    Key::Named(NamedKey::Delete) => Some(everty_ui::Key::Delete),
                                    Key::Named(NamedKey::Home) => Some(everty_ui::Key::Home),
                                    Key::Named(NamedKey::End) => Some(everty_ui::Key::End),
                                    Key::Named(NamedKey::ArrowLeft) => {
                                        Some(everty_ui::Key::ArrowLeft)
                                    }
                                    Key::Named(NamedKey::ArrowRight) => {
                                        Some(everty_ui::Key::ArrowRight)
                                    }
                                    _ => None,
                                };
                                let modifiers = everty_ui::Modifiers {
                                    control,
                                    shift: self.modifiers.shift_key(),
                                    alt: self.modifiers.alt_key(),
                                    meta: self.modifiers.super_key(),
                                };
                                if let Some(key) = editing_key {
                                    self.edit_text(|edit| {
                                        edit.handle_key(key, modifiers);
                                    });
                                } else if !control && !modifiers.alt {
                                    if let Some(text) = &event.text {
                                        self.edit_text(|edit| edit.insert(text));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    self.redraw();
                    return;
                }
                if event.repeat {
                    return;
                }
                if self.modifiers.control_key()
                    && self.modifiers.shift_key()
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyC)
                {
                    self.apply(Action::CopyTrack);
                    return;
                }
                match event.logical_key {
                    Key::Named(NamedKey::Tab) => {
                        if !self.controls.is_empty() {
                            self.focus =
                                Some(self.focus.map_or(0, |i| (i + 1) % self.controls.len()));
                            self.redraw();
                        }
                    }
                    Key::Named(NamedKey::Enter) => {
                        if let Some(c) = self.focus.and_then(|i| self.controls.get(i)) {
                            self.apply(c.action);
                        }
                    }
                    Key::Named(NamedKey::Space) => {
                        let action = self
                            .focus
                            .and_then(|i| self.controls.get(i))
                            .map_or(Action::Play, |c| c.action);
                        self.apply(action);
                    }
                    Key::Named(NamedKey::Escape) if self.model.settings => {
                        self.apply(Action::Settings)
                    }
                    Key::Named(NamedKey::ArrowRight) => self.apply(Action::Next),
                    Key::Named(NamedKey::ArrowUp) => {
                        self.apply(Action::Volume(self.model.volume.saturating_add(5)))
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        self.apply(Action::Volume(self.model.volume.saturating_sub(5)))
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if let Some(live) = self.model.live.as_mut() {
            if live.public.tab
                && !live.public.loading
                && self
                    .public_refresh
                    .is_none_or(|time| now.duration_since(time) >= Duration::from_secs(10))
            {
                live.public.loading = true;
                self.public_refresh = Some(now);
                self.engine.refresh_public(self.proxy.clone());
            }
        }
        self.model.tick(now.duration_since(self.last_tick));
        self.last_tick = now;
        let (switch_due, has_candidate, mut switch_deadline) =
            self.model
                .live
                .as_ref()
                .map_or((false, false, None), |live| {
                    let deadline = live
                        .paused_since
                        .map(|since| since + Duration::from_secs(3))
                        .into_iter()
                        .chain(
                            live.silence_since
                                .map(|since| since + Duration::from_secs(12)),
                        )
                        .min();
                    (
                        live.smart_feed
                            && !live.reconnecting
                            && deadline.is_some_and(|deadline| now >= deadline),
                        smart_candidate(live).is_some(),
                        deadline.filter(|deadline| *deadline > now),
                    )
                });
        if let Some((_, _, since)) = &self.hovered {
            let deadline = *since + Duration::from_millis(550);
            if now >= deadline {
                if !self.tooltip_visible {
                    self.tooltip_visible = true;
                    self.redraw();
                }
            } else {
                switch_deadline =
                    Some(switch_deadline.map_or(deadline, |other| other.min(deadline)));
            }
        }
        if switch_due {
            if has_candidate {
                self.request_smart_next();
            } else if let Some(live) = &mut self.model.live {
                live.paused_since = None;
                live.silence_since = None;
            }
        }
        let animate = self
            .model
            .live
            .as_ref()
            .map_or(self.model.playing && !self.model.settings, |live| {
                (live.capturing || live.receiving) && live.rms > 0.0001
            });
        if self
            .model
            .live
            .as_ref()
            .is_some_and(|live| live.public.tab && !live.public.loading)
        {
            if let Some(time) = self.public_refresh {
                let deadline = time + Duration::from_secs(10);
                switch_deadline = Some(switch_deadline.map_or(deadline, |old| old.min(deadline)));
            }
        }
        if self.visible && !self.occluded && animate {
            if now >= self.next_frame {
                self.redraw();
                self.next_frame = now + Duration::from_millis(80);
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                switch_deadline.map_or(self.next_frame, |deadline| deadline.min(self.next_frame)),
            ));
        } else if let Some(deadline) = switch_deadline {
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

fn smart_candidate(live: &aura_core::live::Live) -> Option<(String, String)> {
    live.smart_candidate()
}
fn render_preview(args: &[String]) -> Result<(), Box<dyn Error>> {
    let path = args
        .get(2)
        .ok_or("Usage: --render <output.png> [idle|playing|settings|broadcast|waiting] [scale]")?;
    let mut model = Aura::default();
    match args.get(3).map(String::as_str).unwrap_or("idle") {
        "live" => model = Aura::live(),
        "public" | "public-search" | "public-favorites" | "public-broadcast" => {
            model = Aura::live();
            let live = model.live.as_mut().unwrap();
            live.public.tab = true;
            live.public.available = 2;
            live.nodes_total = 2;
            live.nodes_connected = 2;
            live.social_connected = true;
            live.profile_name = "VAALI".into();
            for (index, (title, name)) in [
                ("Вечерний psytrance", "Артур"),
                ("Музыка для работы", "Никита"),
                ("Ночной джаз", "Аня"),
                (
                    "Длинное название публичного эфира для проверки размещения",
                    "Марк",
                ),
                ("Тёплый винил", "Оля"),
            ]
            .into_iter()
            .enumerate()
            {
                live.public.rooms.push(aura_core::live::PublicRoom {
                    owner: format!("preview-public-{index}"),
                    title: title.into(),
                    author: name.into(),
                    listeners: index + 3,
                    track: String::new(),
                    artist: String::new(),
                    link: "preview".into(),
                });
            }
            live.public
                .favorites
                .push(("preview-public-0".into(), "Артур".into()));
            if args.get(3).is_some_and(|s| s == "public-search") {
                live.public.search_open = true;
                let mut e = aura_core::live::Editor::new(false, String::new());
                e.purpose = aura_core::live::EditPurpose::PublicSearch;
                live.editor = Some(e);
            }
            if args.get(3).is_some_and(|s| s == "public-favorites") {
                live.public.favorites_only = true;
            }
            if args.get(3).is_some_and(|s| s == "public-broadcast") {
                live.public.tab = false;
                live.public.broadcast = true;
                live.public.active = true;
                live.public.title = "Вечерний psytrance".into();
                live.capturing = true;
                live.listeners = 12;
                live.sources.push(aura_core::live::Source {
                    pid: 123,
                    name: "chrome.exe".into(),
                    started: 0,
                });
                live.selected = live.sources.first().cloned();
            }
        }
        "nodes" => {
            model = Aura::live();
            let live = model.live.as_mut().unwrap();
            live.friends_tab = true;
            live.social_connected = true;
            live.nodes_total = 2;
            live.nodes_connected = 1;
            live.status = "Основной узел недоступен. Присутствие работает через резервный.".into();
        }
        "queue" | "queue-page2" | "queue-empty" | "queue-error" => {
            model = Aura::live();
            let live = model.live.as_mut().unwrap();
            live.friends_tab = true;
            live.receiving = true;
            live.social_connected = true;
            if !args
                .get(3)
                .is_some_and(|scenario| scenario == "queue-empty")
            {
                for (index, name) in [
                    "Маша",
                    "Александра с очень длинным именем",
                    "Саша",
                    "Олег",
                    "Аня",
                    "Денис",
                ]
                .into_iter()
                .enumerate()
                {
                    live.friends.push(aura_core::live::Friend {
                        contact: aura_social::Contact {
                            id: format!("preview-queue-{index}"),
                            name: name.into(),
                            relation: aura_social::Relation::Accepted,
                        },
                        online: index % 2 == 0,
                        live: index % 3 == 0,
                    });
                }
            }
            live.smart_pinned.push("preview-queue-0".into());
            live.smart_excluded.push("preview-queue-2".into());
            live.queue_editor = Some(aura_core::live::QueueEditor {
                ids: live
                    .ordered_queue()
                    .iter()
                    .map(|friend| friend.contact.id.clone())
                    .collect(),
                page: if args
                    .get(3)
                    .is_some_and(|scenario| scenario == "queue-page2")
                {
                    1
                } else {
                    0
                },
                error: if args
                    .get(3)
                    .is_some_and(|scenario| scenario == "queue-error")
                {
                    Some("Не удалось сохранить очередь: файл недоступен для записи. Попробуйте снова.".into())
                } else {
                    None
                },
            });
        }
        "media" => {
            model = Aura::live();
            let live = model.live.as_mut().unwrap();
            live.receiving = true;
            live.rms = 0.08;
            live.media = aura_media::Snapshot {
                info: Some(aura_media::Info {
                    title: "Awake".into(),
                    artist: "Tycho".into(),
                    album: "Awake (Deluxe Version)".into(),
                    app: "Spotify".into(),
                    playback: aura_media::Playback::Playing,
                    position_ms: 38000,
                    duration_ms: 283000,
                }),
                artwork: None,
            };
            live.status = "Эфир друга · звук и сведения о треке зашифрованы.".into();
        }
        #[cfg(target_os = "windows")]
        "media-source" => {
            let pid = args
                .get(5)
                .ok_or("Use --render <file> media-source <scale> <PID>")?
                .parse::<u32>()?;
            let source = aura_audio::sources()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|s| s.pid == pid)
                .ok_or("Audio source not found")?;
            model = Aura::live();
            let live = model.live.as_mut().unwrap();
            live.receiving = true;
            live.media = media::probe(source).map_err(|e| e.to_string())?;
            live.status = "Предпросмотр метаданных Windows · без захвата и передачи звука.".into();
        }
        "friends" | "friend-listening" | "friend-editor" | "friend-remove" | "friend-tooltip" => {
            model = Aura::live();
            let live = model.live.as_mut().unwrap();
            live.friends_tab = true;
            live.social_connected = true;
            live.profile_name = "Ваали".into();
            live.status = "Друзья сохранены. Выберите эфир, чтобы подключиться.".into();
            for (i, (name, relation, online, on_air)) in [
                ("Маша", aura_social::Relation::Accepted, true, true),
                ("Саша", aura_social::Relation::Accepted, true, false),
                ("Аня", aura_social::Relation::Incoming, false, false),
            ]
            .into_iter()
            .enumerate()
            {
                live.friends.push(aura_core::live::Friend {
                    contact: aura_social::Contact {
                        id: format!("preview-contact-{i}"),
                        name: name.into(),
                        relation,
                    },
                    online,
                    live: on_air,
                });
            }
            if args.get(3).is_some_and(|s| s == "friend-listening") {
                live.smart_feed = true;
                live.smart_current = Some("preview-contact-0".into());
                live.smart_pinned.push("preview-contact-0".into());
                live.smart_excluded.push("preview-contact-1".into());
                live.receiving = true;
                live.status = "Один друг в эфире · кнопка Pause остановит прослушивание.".into();
            }
            if args.get(3).is_some_and(|s| s == "friend-editor") {
                live.editor = Some(aura_core::live::Editor::new(
                    false,
                    "12D3KooWNhJjzd72GhJXPM1LZn25UGnwzDyXmKD8xUo3qiy4T3Rw".into(),
                ));
            }
            if args.get(3).is_some_and(|s| s == "friend-remove") {
                live.remove_friend = Some(("preview-contact-0".into(), "Маша".into()));
                live.receiving = true;
            }
        }
        "live-capture" => {
            model = Aura::live();
            let live = model.live.as_mut().unwrap();
            live.sources = vec![aura_core::live::Source {
                pid: 1234,
                name: "MusicPlayer.exe".into(),
                started: 0,
            }];
            live.selected = live.sources.first().cloned();
            live.capturing = true;
            live.listeners = 1;
            live.rms = 0.08;
            live.status = "Превью интерфейса реального эфира (без захвата).".into();
        }
        "idle" => {}
        "playing" => {
            model.act(Action::Play);
            model.tick(Duration::from_millis(12300));
        }
        "settings" => model.act(Action::Settings),
        "broadcast" => {
            model.act(Action::Source(0));
            model.act(Action::Broadcast);
            model.act(Action::Play);
        }
        "waiting" => {
            for _ in 0..3 {
                model.act(Action::SimulateEnd);
            }
        }
        _ => return Err("Unknown preview scenario".into()),
    }
    let scale = args
        .get(4)
        .map(|s| s.parse::<f64>())
        .transpose()?
        .unwrap_or(1.0);
    if !(0.5..=4.0).contains(&scale) {
        return Err("Preview scale must be 0.5..4".into());
    }
    let mut text = TextSystem::new();
    let mut raster = Rgba8Renderer::new();
    aura_ui::register_icons(&mut raster)?;
    let frame = if args.get(3).is_some_and(|s| s == "friend-tooltip") {
        aura_ui::compose_with_tooltip(&model, &mut text, 380.0, 720.0, 1.2, None,
            Some(LogicalPoint::new(104.0, 618.0)),
            Some("Включить Smart Feed. Автоматически переключает эфиры друзей после паузы, тишины или завершения эфира."))
    } else {
        compose(&model, &mut text, 380.0, 720.0, 1.2, None, None)
    };
    let (w, h) = (
        (380.0 * scale).round() as u32,
        (720.0 * scale).round() as u32,
    );
    raster
        .render(
            &frame.paint,
            &mut text,
            DpiScale::new(scale).map_err(|e| format!("DPI: {e:?}"))?,
            PhysicalSize::new(w as f64, h as f64),
        )
        .map_err(|e| format!("Raster: {e:?}"))?;
    let mut encoder =
        png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(path)?), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(raster.pixels())?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "--mesh-smoke") {
        let relay = args
            .get(2)
            .map(String::as_str)
            .unwrap_or(aura_mesh::audio::DEFAULT_RELAY)
            .parse()?;
        return mesh_smoke::run(relay).map_err(|e| e.to_string().into());
    }
    #[cfg(target_os = "windows")]
    if args.get(1).is_some_and(|s| s == "--test-tone") {
        let seconds = args
            .get(2)
            .map(|s| s.parse::<u64>())
            .transpose()?
            .unwrap_or(6)
            .min(30);
        return aura_audio::test_tone(Duration::from_secs(seconds))
            .map_err(|e| e.to_string().into());
    }
    if args.get(1).is_some_and(|s| s == "--audio-smoke") {
        return smoke::run().map_err(|e| e.to_string().into());
    }
    if args.get(1).is_some_and(|s| s == "--sources") {
        for source in aura_audio::sources().map_err(|e| e.to_string())? {
            println!("{}\t{}", source.pid, source.name);
        }
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    if args.get(1).is_some_and(|s| s == "--media-sessions") {
        let snapshots = media::sessions().map_err(|e| e.to_string())?;
        let mut report = String::new();
        if snapshots.is_empty() {
            report.push_str("Windows GSMTC: no media sessions\n");
        }
        for snapshot in &snapshots {
            report.push_str(snapshot);
            report.push('\n');
        }
        let path = args.get(2).map_or_else(
            || {
                std::env::current_exe()
                    .unwrap_or_else(|_| "aura-desktop.exe".into())
                    .with_file_name("aura-media-sessions.txt")
            },
            std::path::PathBuf::from,
        );
        std::fs::write(path, report)?;
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    if args
        .get(1)
        .is_some_and(|s| s == "--media-info" || s == "--media-watch")
    {
        let pid = args
            .get(2)
            .ok_or("Usage: --media-info <source PID>")?
            .parse::<u32>()?;
        let source = aura_audio::sources()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|s| s.pid == pid)
            .ok_or("Select a currently listed audio source PID")?;
        if args[1] == "--media-watch" {
            let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stop = stopped.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(15));
                stop.store(true, std::sync::atomic::Ordering::Release);
            });
            media::observe(source, stopped, |snapshot| {
                println!(
                    "Media event: {:?}; artwork={} bytes",
                    snapshot.info,
                    snapshot.artwork.as_ref().map_or(0, |a| a.jpeg.len())
                )
            });
        } else {
            let snapshot = media::probe(source).map_err(|e| e.to_string())?;
            println!(
                "Selected source metadata: {:?}; artwork={} bytes",
                snapshot.info,
                snapshot.artwork.as_ref().map_or(0, |a| a.jpeg.len())
            );
        }
        return Ok(());
    }
    if args.get(1).is_some_and(|s| s == "--render") {
        return render_preview(&args);
    }
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let mut raster = Rgba8Renderer::new();
    aura_ui::register_icons(&mut raster)?;
    let mut desktop = Desktop {
        engine: engine::Engine::new(event_loop.create_proxy(), true)?,
        model: Aura::live(),
        text: TextSystem::new(),
        raster,
        window: None,
        surface: None,
        tray: None,
        #[cfg(target_os = "windows")]
        share: None,
        proxy: event_loop.create_proxy(),
        controls: vec![],
        pointer: None,
        hovered: None,
        tooltip_visible: false,
        pressed: None,
        editor_drag: false,
        volume_dragging: false,
        public_scroll_dragging: false,
        public_refresh: None,
        focus: None,
        modifiers: ModifiersState::empty(),
        visible: true,
        occluded: false,
        last_tick: Instant::now(),
        next_frame: Instant::now(),
    };
    if let Some(live) = &mut desktop.model.live {
        live.peer_id = desktop.engine.peer_id.clone();
    }
    desktop.restore_smart_feed_preferences();
    event_loop.run_app(&mut desktop)?;
    Ok(())
}
