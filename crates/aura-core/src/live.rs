//! UI snapshots for the real engine. No audio or transport dependencies.
use std::time::Instant;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub pid: u32,
    pub name: String,
    pub started: u64,
}
pub struct Live {
    pub public: PublicState,
    pub media: aura_media::Snapshot,
    pub friends_tab: bool,
    pub pending_friend: bool,
    pub profile_name: String,
    pub social_connected: bool,
    pub nodes_connected: usize,
    pub nodes_total: usize,
    pub friends: Vec<Friend>,
    pub friend_page: usize,
    pub smart_feed: bool,
    pub smart_current: Option<String>,
    pub smart_attempted: Vec<String>,
    pub smart_pinned: Vec<String>,
    pub smart_excluded: Vec<String>,
    pub smart_order: Vec<String>,
    pub queue_editor: Option<QueueEditor>,
    pub smart_pending_id: Option<String>,
    pub smart_transition_authorized: bool,
    pub smart_transition_output_ready: bool,
    pub smart_listen_request: Option<(u64, String)>,
    pub smart_listen_nonce: u64,
    pub smart_prefetch_request: Option<(u64, String)>,
    pub smart_prefetch_nonce: u64,
    pub smart_prefetched: Option<(String, String)>,
    pub paused_since: Option<Instant>,
    pub silence_since: Option<Instant>,
    pub editor: Option<Editor>,
    pub remove_friend: Option<(String, String)>,
    pub use_relay: bool,
    pub peer_id: String,
    pub sources: Vec<Source>,
    pub selected: Option<Source>,
    pub page: usize,
    pub scanning: bool,
    pub busy: bool,
    pub stopping: bool,
    pub capturing: bool,
    pub receiving: bool,
    pub reconnecting: bool,
    pub invitation: Option<String>,
    pub status: String,
    pub rms: f32,
    pub frames: u64,
    pub listeners: usize,
    pub dropped: u64,
    pub audio_dropped: u64,
    pub concealed: u64,
    pub buffer_ms: u32,
    pub rtt_ms: u64,
}
impl Default for Live {
    fn default() -> Self {
        Self {
            public: PublicState::default(),
            media: aura_media::Snapshot::default(),
            friends_tab: false,
            pending_friend: false,
            profile_name: "Слушатель".into(),
            social_connected: false,
            nodes_connected: 0,
            nodes_total: 1,
            friends: vec![],
            friend_page: 0,
            smart_feed: false,
            smart_current: None,
            smart_attempted: Vec::new(),
            smart_pinned: Vec::new(),
            smart_excluded: Vec::new(),
            smart_order: Vec::new(),
            queue_editor: None,
            smart_pending_id: None,
            smart_transition_authorized: false,
            smart_transition_output_ready: false,
            smart_listen_request: None,
            smart_listen_nonce: 0,
            smart_prefetch_request: None,
            smart_prefetch_nonce: 0,
            smart_prefetched: None,
            paused_since: None,
            silence_since: None,
            editor: None,
            remove_friend: None,
            use_relay: true,
            peer_id: String::new(),
            sources: vec![],
            selected: None,
            page: 0,
            scanning: false,
            busy: false,
            stopping: false,
            capturing: false,
            receiving: false,
            reconnecting: false,
            invitation: None,
            status: "Выберите приложение для эфира или вставьте приглашение друга.".into(),
            rms: 0.0,
            frames: 0,
            listeners: 0,
            dropped: 0,
            audio_dropped: 0,
            concealed: 0,
            buffer_ms: 0,
            rtt_ms: 0,
        }
    }
}
pub struct Friend {
    pub contact: aura_social::Contact,
    pub online: bool,
    pub live: bool,
}

pub struct QueueEditor {
    pub ids: Vec<String>,
    pub page: usize,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisibilityChange {
    Unchanged,
    Selected,
    Restart,
}

impl Live {
    /// An explicit visibility choice can restart our source, never a listener session.
    pub fn toggle_public_visibility(&mut self, broadcast_session: bool) -> VisibilityChange {
        if self.stopping {
            self.status = "Дождитесь завершения остановки эфира…".into();
            return VisibilityChange::Unchanged;
        }
        self.public.broadcast = !self.public.broadcast;
        if self.public.broadcast {
            self.use_relay = true;
            if self.public.title.is_empty() {
                self.public.title = format!("Музыка · {}", self.profile_name);
            }
            self.status =
                "Публичный эфир увидят все пользователи Aura. Узел получает доступ к аудио.".into();
        } else {
            self.status = "Эфир будет доступен по приглашению.".into();
        }
        if broadcast_session && self.selected.is_some() {
            VisibilityChange::Restart
        } else {
            if self.receiving || self.busy {
                self.status =
                    "Видимость выбрана для следующего вашего эфира. Прослушивание продолжается."
                        .into();
            }
            VisibilityChange::Selected
        }
    }
    /// Accepted contacts in the saved order; new contacts follow existing entries.
    pub fn ordered_queue(&self) -> Vec<&Friend> {
        let mut friends: Vec<_> = self
            .friends
            .iter()
            .filter(|friend| friend.contact.relation == aura_social::Relation::Accepted)
            .collect();
        friends.sort_by_key(|friend| {
            self.smart_order
                .iter()
                .position(|id| id == &friend.contact.id)
                .unwrap_or(usize::MAX)
        });
        friends
    }

    pub fn smart_candidate(&self) -> Option<(String, String)> {
        if !self.smart_feed {
            return None;
        }
        let queue = self.ordered_queue();
        if queue.is_empty() {
            return None;
        }
        let start = self
            .smart_current
            .as_ref()
            .and_then(|current| {
                queue
                    .iter()
                    .position(|friend| &friend.contact.id == current)
            })
            .map_or(0, |index| (index + 1) % queue.len());
        let available: Vec<_> = (0..queue.len())
            .map(|offset| queue[(start + offset) % queue.len()])
            .filter(|friend| {
                friend.online
                    && friend.live
                    && !self.smart_attempted.contains(&friend.contact.id)
                    && !self.smart_excluded.contains(&friend.contact.id)
            })
            .collect();
        let candidate = available
            .iter()
            .find(|friend| self.smart_pinned.contains(&friend.contact.id))
            .or_else(|| available.first())?;
        Some((candidate.contact.id.clone(), candidate.contact.name.clone()))
    }
}

pub struct Editor {
    pub purpose: EditPurpose,
    pub name: bool,
    pub value: String,
    pub caret: usize,
    pub anchor: Option<usize>,
    pub focused: bool,
    pub scroll: f64,
}
impl Editor {
    pub fn new(name: bool, value: String) -> Self {
        let caret = value.len();
        Self {
            purpose: EditPurpose::Profile,
            name,
            value,
            caret,
            anchor: if name { Some(0) } else { None },
            focused: true,
            scroll: 0.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EditPurpose {
    Profile,
    PublicTitle,
    PublicSearch,
}

#[derive(Clone)]
pub struct PublicRoom {
    pub owner: String,
    pub title: String,
    pub author: String,
    pub listeners: usize,
    pub track: String,
    pub artist: String,
    pub link: String,
}
#[derive(Default)]
pub struct PublicState {
    pub active: bool,
    pub tab: bool,
    pub broadcast: bool,
    pub title: String,
    pub query: String,
    pub search_open: bool,
    pub favorites_only: bool,
    pub favorites: Vec<(String, String)>,
    pub rooms: Vec<PublicRoom>,
    pub listening: Option<String>,
    pub offset: usize,
    pub loading: bool,
    pub available: usize,
    pub error: Option<String>,
}
impl PublicState {
    pub fn visible(&self) -> Vec<PublicRoom> {
        let query = self.query.to_lowercase();
        let mut rooms: Vec<_> = self
            .rooms
            .iter()
            .filter(|room| {
                (!self.favorites_only || self.favorites.iter().any(|(id, _)| id == &room.owner))
                    && format!("{} {}", room.title, room.author)
                        .to_lowercase()
                        .contains(&query)
            })
            .cloned()
            .collect();
        if self.favorites_only {
            for (id, name) in &self.favorites {
                if !self.rooms.iter().any(|room| &room.owner == id)
                    && name.to_lowercase().contains(&query)
                {
                    rooms.push(PublicRoom {
                        owner: id.clone(),
                        author: name.clone(),
                        title: name.clone(),
                        listeners: 0,
                        track: String::new(),
                        artist: String::new(),
                        link: String::new(),
                    });
                }
            }
        }
        rooms
    }
}

#[cfg(test)]
mod public_tests {
    use super::*;
    #[test]
    fn visibility_switches_idle_and_restarts_only_our_broadcast() {
        let mut live = Live {
            profile_name: "VAALI".into(),
            ..Default::default()
        };
        assert_eq!(
            live.toggle_public_visibility(false),
            VisibilityChange::Selected
        );
        assert!(live.public.broadcast);
        assert!(live.use_relay);
        assert!(!live.public.title.is_empty());
        live.selected = Some(Source {
            pid: 123,
            name: "player.exe".into(),
            started: 0,
        });
        live.capturing = true;
        assert_eq!(
            live.toggle_public_visibility(true),
            VisibilityChange::Restart
        );
        assert!(!live.public.broadcast);
        live.capturing = false;
        live.busy = true;
        assert_eq!(
            live.toggle_public_visibility(true),
            VisibilityChange::Restart
        );
        assert!(live.public.broadcast);
        // A selected source must not turn listening into capture.
        live.busy = false;
        live.receiving = true;
        assert_eq!(
            live.toggle_public_visibility(false),
            VisibilityChange::Selected
        );
        assert!(!live.public.broadcast);
        assert!(live.receiving);
        live.stopping = true;
        assert_eq!(
            live.toggle_public_visibility(true),
            VisibilityChange::Unchanged
        );
        assert!(!live.public.broadcast);
    }
    #[test]
    fn favorites_follow_author_identity_and_remain_available_offline() {
        let mut state = PublicState {
            favorites: vec![("author-id".into(), "Автор".into())],
            favorites_only: true,
            ..Default::default()
        };
        assert!(state.visible()[0].link.is_empty());
        state.rooms.push(PublicRoom {
            owner: "author-id".into(),
            title: "Новый эфир".into(),
            author: "Новое имя".into(),
            listeners: 1,
            track: String::new(),
            artist: String::new(),
            link: "live".into(),
        });
        assert_eq!(state.visible().len(), 1);
        assert_eq!(state.visible()[0].title, "Новый эфир");
        state.query = "НОВЫЙ".into();
        assert_eq!(state.visible().len(), 1);
        state.query = "Другое".into();
        assert!(state.visible().is_empty());
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;
    fn feed() -> Live {
        let mut live = Live {
            smart_feed: true,
            ..Live::default()
        };
        live.friends = ["a", "b", "c", "new"]
            .into_iter()
            .map(|id| Friend {
                contact: aura_social::Contact {
                    id: id.into(),
                    name: id.into(),
                    relation: aura_social::Relation::Accepted,
                },
                online: true,
                live: true,
            })
            .collect();
        live.smart_order = ["c", "a", "b"].map(String::from).to_vec();
        live
    }
    #[test]
    fn custom_order_rotates_from_current_and_appends_new_contacts() {
        let mut live = feed();
        assert_eq!(live.smart_candidate().unwrap().0, "c");
        live.smart_current = Some("c".into());
        assert_eq!(live.smart_candidate().unwrap().0, "a");
        live.smart_current = Some("b".into());
        assert_eq!(live.smart_candidate().unwrap().0, "new");
        live.smart_current = Some("new".into());
        assert_eq!(live.smart_candidate().unwrap().0, "c");
        live.smart_order = ["b", "a", "c"].map(String::from).to_vec();
        assert_eq!(live.smart_candidate().unwrap().0, "b");
    }
    #[test]
    fn custom_order_respects_consent_priority_exclusions_and_failed_attempts() {
        let mut live = feed();
        live.smart_pinned.push("b".into());
        assert_eq!(live.smart_candidate().unwrap().0, "b");
        live.smart_excluded.push("b".into());
        live.smart_attempted.push("c".into());
        live.friends[0].online = false;
        live.friends[3].contact.relation = aura_social::Relation::Incoming;
        assert!(live.smart_candidate().is_none());
        live.friends[0].online = true;
        assert_eq!(live.smart_candidate().unwrap().0, "a");
    }
}
