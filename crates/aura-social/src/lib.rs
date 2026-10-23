//! Local consent and profile storage. Audio capabilities are deliberately never persisted.
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::Write,
    path::{Path, PathBuf},
};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const MAX_CONTACTS: usize = 128;
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Relation {
    Outgoing,
    Incoming,
    Accepted,
    Declined,
    Blocked,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Intent {
    Request,
    Pending,
    Approved,
    Revoked,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Contact {
    pub id: String,
    pub name: String,
    pub relation: Relation,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub version: u32,
    pub owner: String,
    pub name: String,
    pub contacts: Vec<Contact>,
}
pub struct Store {
    path: PathBuf,
    data: Profile,
}
pub fn peer_id(text: &str) -> Result<String> {
    let text = text
        .trim()
        .strip_prefix("aura://user/")
        .unwrap_or(text.trim());
    if text.len() > 128 {
        return Err("Aura ID слишком длинный".into());
    }
    text.parse::<libp2p_identity::PeerId>()
        .map(|id| id.to_string())
        .map_err(|_| "Неверный Aura ID. Скопируйте «Мой Aura ID» у друга.".into())
}
pub fn name(text: &str) -> Result<String> {
    let value = text.trim();
    if value.is_empty() || value.chars().count() > 48 || value.chars().any(char::is_control) {
        return Err("Имя: от 1 до 48 символов, без управляющих знаков.".into());
    }
    Ok(value.into())
}
impl Store {
    pub fn open(path: &Path, owner: &str) -> Result<Self> {
        let owner = peer_id(owner)?;
        let data = if path.exists() {
            if std::fs::metadata(path)?.len() > 256 * 1024 {
                return Err("Profile file is too large".into());
            }
            let data: Profile = serde_json::from_slice(&std::fs::read(path)?)?;
            if data.version != 1 || data.owner != owner || data.contacts.len() > MAX_CONTACTS {
                return Err("Profile version, owner or size is invalid".into());
            }
            name(&data.name)?;
            let mut ids = HashSet::new();
            for contact in &data.contacts {
                let canonical = peer_id(&contact.id)?;
                if canonical != contact.id || canonical == owner || !ids.insert(&contact.id) {
                    return Err("Invalid or duplicate contact".into());
                }
                name(&contact.name)?;
            }
            data
        } else {
            Profile {
                version: 1,
                owner,
                name: "Слушатель".into(),
                contacts: vec![],
            }
        };
        let store = Self {
            path: path.into(),
            data,
        };
        if !path.exists() {
            store.persist(&store.data)?;
        }
        Ok(store)
    }
    pub fn data(&self) -> &Profile {
        &self.data
    }
    fn persist(&self, data: &Profile) -> Result<()> {
        let parent = self.path.parent().ok_or("No profile directory")?;
        std::fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(&serde_json::to_vec_pretty(data)?)?;
        temp.as_file().sync_all()?;
        temp.persist(&self.path).map_err(|e| e.error)?;
        Ok(())
    }
    fn update(&mut self, change: impl FnOnce(&mut Profile) -> Result<()>) -> Result<()> {
        let mut next = self.data.clone();
        change(&mut next)?;
        if next != self.data {
            self.persist(&next)?;
            self.data = next;
        }
        Ok(())
    }
    pub fn rename(&mut self, value: &str) -> Result<()> {
        let value = name(value)?;
        self.update(|p| {
            p.name = value;
            Ok(())
        })
    }
    pub fn add(&mut self, value: &str) -> Result<()> {
        let id = peer_id(value)?;
        if id == self.data.owner {
            return Err("Это ваш собственный Aura ID.".into());
        }
        self.update(|p| {
            if let Some(c) = p.contacts.iter_mut().find(|c| c.id == id) {
                c.relation = match c.relation {
                    Relation::Incoming => Relation::Accepted,
                    Relation::Accepted => Relation::Accepted,
                    _ => Relation::Outgoing,
                };
            } else {
                if p.contacts.len() >= MAX_CONTACTS {
                    return Err("Список контактов заполнен.".into());
                }
                p.contacts.push(Contact {
                    id,
                    name: "Новый контакт".into(),
                    relation: Relation::Outgoing,
                });
            }
            Ok(())
        })
    }
    pub fn accept(&mut self, id: &str) -> Result<()> {
        self.update(|p| {
            let contact = p
                .contacts
                .iter_mut()
                .find(|c| c.id == id)
                .ok_or("Contact not found")?;
            if contact.relation != Relation::Incoming {
                return Err("Нет входящего запроса.".into());
            }
            contact.relation = Relation::Accepted;
            Ok(())
        })
    }
    pub fn remove(&mut self, id: &str) -> Result<()> {
        self.update(|p| {
            let c = p
                .contacts
                .iter_mut()
                .find(|c| c.id == id)
                .ok_or("Contact not found")?;
            c.relation = Relation::Blocked;
            Ok(())
        })
    }
    pub fn share(&self, id: &str) -> bool {
        self.data
            .contacts
            .iter()
            .any(|c| c.id == id && c.relation == Relation::Accepted)
    }
    pub fn intent(&self, id: &str) -> Intent {
        match self
            .data
            .contacts
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.relation)
        {
            Some(Relation::Accepted) => Intent::Approved,
            Some(Relation::Outgoing) => Intent::Request,
            Some(Relation::Incoming) => Intent::Pending,
            _ => Intent::Revoked,
        }
    }
    pub fn observe(&mut self, id: &str, remote_name: &str, intent: Intent) -> Result<()> {
        let id = peer_id(id)?;
        if id == self.data.owner {
            return Err("Self request".into());
        }
        if self.intent(&id) == Intent::Revoked
            && self
                .data
                .contacts
                .iter()
                .any(|c| c.id == id && c.relation == Relation::Blocked)
        {
            return Ok(());
        }
        let remote_name = if intent == Intent::Revoked {
            None
        } else {
            Some(name(remote_name)?)
        };
        self.update(|p| {
            if let Some(c) = p.contacts.iter_mut().find(|c| c.id == id) {
                c.relation = match (c.relation, intent) {
                    (Relation::Blocked, _) => Relation::Blocked,
                    (_, Intent::Revoked) => Relation::Declined,
                    (Relation::Outgoing, Intent::Approved | Intent::Request) => Relation::Accepted,
                    (Relation::Declined, Intent::Request | Intent::Approved) => Relation::Incoming,
                    (r, _) => r,
                };
                if let Some(n) = remote_name {
                    c.name = n;
                }
            } else if matches!(intent, Intent::Request | Intent::Approved) {
                if p.contacts.len() >= MAX_CONTACTS
                    || p.contacts
                        .iter()
                        .filter(|c| c.relation == Relation::Incoming)
                        .count()
                        >= 32
                {
                    return Err("Incoming request limit reached".into());
                }
                p.contacts.push(Contact {
                    id,
                    name: remote_name.ok_or("Missing profile name")?,
                    relation: Relation::Incoming,
                });
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(n: u8) -> String {
        let mut bytes = vec![0x12, 0x20];
        bytes.extend([n; 32]);
        libp2p_identity::PeerId::from_bytes(&bytes)
            .unwrap()
            .to_string()
    }
    #[test]
    fn consent_survives_restart_and_block_cannot_be_replayed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("friends.json");
        let a = id(1);
        let b = id(2);
        let mut store = Store::open(&path, &a).unwrap();
        store.observe(&b, "Маша", Intent::Approved).unwrap();
        assert!(!store.share(&b));
        assert_eq!(store.data.contacts[0].relation, Relation::Incoming);
        store.accept(&b).unwrap();
        assert!(store.share(&b));
        let mut store = Store::open(&path, &a).unwrap();
        assert!(store.share(&b));
        store.remove(&b).unwrap();
        store.observe(&b, "Маша", Intent::Approved).unwrap();
        assert!(!store.share(&b));
        assert_eq!(store.data.contacts[0].relation, Relation::Blocked);
        assert!(Store::open(&path, &b).is_err());
    }
    #[test]
    fn outgoing_confirmation_and_remote_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("friends.json"), &id(1)).unwrap();
        let b = id(2);
        store.add(&b).unwrap();
        store.observe(&b, "Саша", Intent::Pending).unwrap();
        assert!(!store.share(&b));
        store.observe(&b, "Саша", Intent::Approved).unwrap();
        assert!(store.share(&b));
        store.observe(&b, "", Intent::Revoked).unwrap();
        assert!(!store.share(&b));
        assert!(store.add(&id(1)).is_err());
        assert!(store.add("bad").is_err());
        assert!(store.rename("\n").is_err());
    }
}
