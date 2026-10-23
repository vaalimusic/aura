use std::{
    io::{Read, Write},
    path::Path,
};
pub fn load(path: &Path) -> Vec<(String, String)> {
    let Ok(file) = std::fs::File::open(path) else {
        return vec![];
    };
    let mut bytes = Vec::new();
    if file.take(65537).read_to_end(&mut bytes).is_err() || bytes.len() > 65536 {
        return vec![];
    }
    let Ok(values) = serde_json::from_slice::<Vec<(String, String)>>(&bytes) else {
        return vec![];
    };
    let mut result = Vec::new();
    for (id, name) in values.into_iter().take(128) {
        if aura_social::peer_id(&id).is_ok()
            && aura_social::name(&name).is_ok()
            && !result.iter().any(|(old, _)| old == &id)
        {
            result.push((id, name));
        }
    }
    result
}
pub fn save(path: &Path, values: &[(String, String)]) -> Result<(), String> {
    if values.len() > 128
        || values
            .iter()
            .any(|(id, name)| aura_social::peer_id(id).is_err() || aura_social::name(name).is_err())
    {
        return Err("Не удалось сохранить избранное.".into());
    }
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut temp =
            tempfile::NamedTempFile::new_in(path.parent().ok_or("No profile directory")?)?;
        temp.write_all(&serde_json::to_vec(values)?)?;
        temp.as_file().sync_all()?;
        temp.persist(path)?;
        Ok(())
    })();
    result.map_err(|_| "Не удалось сохранить избранное.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn favorites_survive_restart_and_bad_update_preserves_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("public-favorites.json");
        let id = aura_mesh::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id()
            .to_string();
        let values = vec![(id, "Автор".into())];
        save(&path, &values).unwrap();
        assert_eq!(load(&path), values);
        assert!(save(&path, &[("bad".into(), "Автор".into())]).is_err());
        assert_eq!(load(&path), values);
    }
}
