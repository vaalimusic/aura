//! Backward-compatible, bounded and atomically replaced local feed preferences.
use std::{io::Write, path::Path};

#[derive(Default, Debug, PartialEq, Eq)]
pub struct Preferences {
    pub pinned: Vec<String>,
    pub excluded: Vec<String>,
    pub order: Vec<String>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && !id.contains(char::is_control)
}

pub fn load(path: &Path) -> Preferences {
    let mut result = Preferences::default();
    let Ok(file) = std::fs::File::open(path) else {
        return result;
    };
    // Bound the read itself, including a file that grows after it was opened.
    let mut contents = String::new();
    if std::io::Read::read_to_string(&mut std::io::Read::take(file, 64 * 1024 + 1), &mut contents)
        .is_err()
        || contents.len() > 64 * 1024
    {
        return result;
    }
    for line in contents.lines() {
        let Some((kind, id)) = line.split_once('\t') else {
            continue;
        };
        if !valid_id(id) {
            continue;
        }
        let list = match kind {
            "p" => &mut result.pinned,
            "e" => &mut result.excluded,
            "o" => &mut result.order,
            _ => continue,
        };
        if list.len() < 128 && !list.iter().any(|entry| entry == id) {
            list.push(id.to_owned());
        }
    }
    result
}

pub fn save(
    path: &Path,
    pinned: &[String],
    excluded: &[String],
    order: &[String],
) -> Result<(), String> {
    let mut contents = String::new();
    for (kind, ids) in [("p", pinned), ("e", excluded), ("o", order)] {
        if ids.len() > 128 || ids.iter().any(|id| !valid_id(id)) {
            return Err("Некорректные настройки очереди.".into());
        }
        for id in ids {
            contents.push_str(&format!("{kind}\t{id}\n"));
        }
    }
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut temp =
            tempfile::NamedTempFile::new_in(path.parent().ok_or("No preferences directory")?)?;
        temp.write_all(contents.as_bytes())?;
        temp.as_file().sync_all()?;
        temp.persist(path)?;
        Ok(())
    })();
    result.map_err(|error| format!("Не удалось сохранить очередь: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn upgrades_existing_preferences_and_preserves_order_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smart-feed.txt");
        std::fs::write(
            &path,
            "p\tfriend-b\ne\tfriend-c\np\tfriend-b\nunknown\tignored\n",
        )
        .unwrap();
        let old = load(&path);
        assert_eq!(old.pinned, vec!["friend-b"]);
        assert_eq!(old.excluded, vec!["friend-c"]);
        assert!(old.order.is_empty());
        let order = vec!["friend-c".into(), "friend-a".into(), "friend-b".into()];
        save(&path, &old.pinned, &old.excluded, &order).unwrap();
        assert_eq!(load(&path), Preferences { order, ..old });
    }
    #[test]
    fn rejects_invalid_write_without_replacing_previous_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smart-feed.txt");
        save(&path, &[], &[], &["friend-a".into()]).unwrap();
        assert!(save(&path, &[], &[], &["injected\no\tfriend-b".into()]).is_err());
        assert_eq!(load(&path).order, vec!["friend-a"]);
    }
}
