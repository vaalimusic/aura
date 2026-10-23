//! Offline prototype state. No capture, transport or UI dependencies.
use std::time::Duration;
pub mod live;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    Live,
    Away,
    Offline,
}

#[derive(Clone, Debug)]
pub struct Friend {
    pub name: &'static str,
    pub initials: &'static str,
    pub track: &'static str,
    pub artist: &'static str,
    pub presence: Presence,
    pub excluded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Broadcast,
    Play,
    Next,
    Pin,
    Settings,
    Queue,
    Friends,
    Select(usize),
    Exclude(usize),
    Source(usize),
    Privacy,
    Volume(u8),
    VolumeSlider,
    SimulateEnd,
    ResetDemo,
    RefreshSources,
    SourcePage(bool),
    CopyInvite,
    CopyTrack,
    ShareTrack,
    SmartFeed,
    PasteInvite,
    StopAudio,
    Route,
    CopyId,
    SocialTab(bool),
    AddFriend,
    RenameProfile,
    EditPaste,
    EditSave,
    EditCancel,
    EditFocus,
    FriendAccept(usize),
    FriendRemove(usize),
    FriendRemoveConfirm,
    FriendRemoveCancel,
    FriendListen(usize),
    SmartFeedPin(usize),
    SmartFeedExclude(usize),
    QueueOpen,
    QueueMove(usize, bool),
    QueuePage(bool),
    QueueSave,
    QueueCancel,
    FriendPage(bool),
    PublicTab,
    PublicVisibility,
    PublicTitle,
    PublicSearch,
    PublicFilter(bool),
    PublicFavorite(usize),
    PublicListen(usize),
    PublicScroll(bool),
    PublicScrollThumb,
    PublicScrollTo(usize),
}

pub struct Aura {
    pub live: Option<live::Live>,
    pub friends: Vec<Friend>,
    pub current: Option<usize>,
    pub playing: bool,
    pub pinned: bool,
    pub broadcasting: bool,
    pub source: Option<usize>,
    pub friends_only: bool,
    pub volume: u8,
    pub settings: bool,
    pub queue: bool,
    pub elapsed: Duration,
    pub notice: &'static str,
}

impl Default for Aura {
    fn default() -> Self {
        Self {
            live: None,
            friends: vec![
                Friend {
                    name: "Маша",
                    initials: "М",
                    track: "Weightless",
                    artist: "Marconi Union",
                    presence: Presence::Live,
                    excluded: false,
                },
                Friend {
                    name: "Саша",
                    initials: "С",
                    track: "Kerala",
                    artist: "Bonobo",
                    presence: Presence::Live,
                    excluded: false,
                },
                Friend {
                    name: "Аня",
                    initials: "А",
                    track: "A Walk",
                    artist: "Tycho",
                    presence: Presence::Live,
                    excluded: false,
                },
                Friend {
                    name: "Даня",
                    initials: "Д",
                    track: "Музыка на паузе",
                    artist: "",
                    presence: Presence::Away,
                    excluded: false,
                },
                Friend {
                    name: "Ника",
                    initials: "Н",
                    track: "Не в сети",
                    artist: "",
                    presence: Presence::Offline,
                    excluded: false,
                },
            ],
            current: Some(0),
            playing: false,
            pinned: false,
            broadcasting: false,
            source: None,
            friends_only: true,
            volume: 65,
            settings: false,
            queue: false,
            elapsed: Duration::ZERO,
            notice: "Выберите эфир и будьте рядом.",
        }
    }
}

impl Aura {
    pub fn live() -> Self {
        Self {
            live: Some(live::Live::default()),
            ..Self::default()
        }
    }
    pub fn available(&self) -> impl Iterator<Item = usize> + '_ {
        self.friends
            .iter()
            .enumerate()
            .filter(|(_, f)| f.presence == Presence::Live && !f.excluded)
            .map(|(i, _)| i)
    }

    fn next(&mut self) {
        let start = self.current.map_or(0, |i| i + 1);
        let next = self
            .available()
            .find(|i| *i >= start)
            .or_else(|| self.available().next());
        self.current = next;
        self.elapsed = Duration::ZERO;
        if self.current.is_none() {
            self.playing = false;
        }
    }

    pub fn act(&mut self, action: Action) {
        match action {
            Action::Broadcast => {
                if self.broadcasting {
                    self.broadcasting = false;
                    self.notice = "Демоэфир завершён.";
                } else if self.source.is_some() {
                    self.broadcasting = true;
                    self.notice = "Демоэфир включён. Реальный звук не захватывается.";
                } else {
                    self.settings = true;
                    self.notice = "Сначала выберите демонстрационный источник.";
                }
            }
            Action::Play => {
                if self.current.is_none() {
                    self.next();
                }
                self.playing = self.current.is_some() && !self.playing;
            }
            Action::Next => {
                self.pinned = false;
                self.next();
            }
            Action::Pin => {
                if self.current.is_some() {
                    self.pinned = !self.pinned;
                }
            }
            Action::Select(i) => {
                if self.available().any(|n| n == i) {
                    self.current = Some(i);
                    self.playing = true;
                    self.elapsed = Duration::ZERO;
                    self.pinned = false;
                }
            }
            Action::Exclude(i) => {
                if let Some(friend) = self.friends.get_mut(i) {
                    friend.excluded = !friend.excluded;
                    if friend.excluded && self.current == Some(i) {
                        self.pinned = false;
                        self.next();
                    }
                }
            }
            Action::Settings => self.settings = !self.settings,
            Action::Queue => self.queue = true,
            Action::Friends => self.queue = false,
            Action::Source(i) if i < 3 => {
                // Re-selecting a source stops the previous session immediately.
                self.broadcasting = false;
                self.source = Some(i);
            }
            Action::Source(_) => {}
            Action::Privacy => {
                self.friends_only = !self.friends_only;
                self.broadcasting = false;
            }
            Action::Volume(value) => self.volume = value.min(100),
            Action::VolumeSlider => {}
            Action::SimulateEnd => {
                if let Some(i) = self.current {
                    self.friends[i].presence = Presence::Away;
                    self.pinned = false;
                    self.next();
                    self.notice = "Эфир завершён → выбран следующий доступный друг.";
                }
            }
            Action::ResetDemo => *self = Self::default(),
            Action::RefreshSources
            | Action::SourcePage(_)
            | Action::CopyInvite
            | Action::CopyTrack
            | Action::ShareTrack
            | Action::SmartFeed
            | Action::PasteInvite
            | Action::StopAudio => {}
            Action::Route
            | Action::CopyId
            | Action::SocialTab(_)
            | Action::AddFriend
            | Action::RenameProfile
            | Action::EditPaste
            | Action::EditSave
            | Action::EditCancel
            | Action::EditFocus
            | Action::FriendAccept(_)
            | Action::FriendRemove(_)
            | Action::FriendRemoveConfirm
            | Action::FriendRemoveCancel
            | Action::FriendListen(_)
            | Action::SmartFeedPin(_)
            | Action::SmartFeedExclude(_)
            | Action::QueueOpen
            | Action::QueueMove(_, _)
            | Action::QueuePage(_)
            | Action::QueueSave
            | Action::QueueCancel
            | Action::FriendPage(_)
            | Action::PublicTab
            | Action::PublicVisibility
            | Action::PublicTitle
            | Action::PublicSearch
            | Action::PublicFilter(_)
            | Action::PublicFavorite(_)
            | Action::PublicListen(_)
            | Action::PublicScroll(_)
            | Action::PublicScrollThumb
            | Action::PublicScrollTo(_) => {}
        }
    }

    pub fn tick(&mut self, delta: Duration) {
        if self.playing || self.live.as_ref().is_some_and(|live| live.capturing) {
            self.elapsed += delta;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn broadcast_requires_explicit_source_and_stops_when_permissions_change() {
        let mut app = Aura::default();
        app.act(Action::Broadcast);
        assert!(!app.broadcasting);
        assert!(app.settings);
        app.act(Action::Source(0));
        app.act(Action::Broadcast);
        assert!(app.broadcasting);
        app.act(Action::Privacy);
        assert!(!app.broadcasting);
        app.act(Action::Broadcast);
        app.act(Action::Source(1));
        assert!(!app.broadcasting);
    }
    #[test]
    fn feed_fails_over_and_waits_when_all_sources_end() {
        let mut app = Aura::default();
        app.act(Action::Play);
        app.act(Action::Pin);
        app.act(Action::SimulateEnd);
        assert_eq!(app.current, Some(1));
        assert!(!app.pinned);
        assert!(app.playing);
        app.act(Action::SimulateEnd);
        app.act(Action::SimulateEnd);
        assert_eq!(app.current, None);
        assert!(!app.playing);
        app.act(Action::Play);
        assert!(!app.playing);
    }
    #[test]
    fn exclusions_wrap_and_invalid_actions_are_safe() {
        let mut app = Aura::default();
        app.act(Action::Exclude(0));
        assert_eq!(app.current, Some(1));
        app.act(Action::Next);
        assert_eq!(app.current, Some(2));
        app.act(Action::Next);
        assert_eq!(app.current, Some(1));
        app.act(Action::Select(4));
        assert_eq!(app.current, Some(1));
        app.act(Action::Select(999));
        app.act(Action::Source(99));
        app.act(Action::Exclude(999));
        assert_eq!(app.source, None);
    }
}
