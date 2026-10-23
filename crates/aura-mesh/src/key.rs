//! Persistent Ed25519 identity. DPAPI on Windows, mode 0600 for a headless Unix node.
use crate::{identity, Result};
use std::{fs::OpenOptions, io::Write, path::Path};
pub fn load(path: &Path) -> Result<identity::Keypair> {
    if path.exists() {
        let bytes = std::fs::read(path)?;
        if bytes.len() > 16384 {
            return Err("Identity file is too large".into());
        }
        return Ok(identity::Keypair::from_protobuf_encoding(&unprotect(
            &bytes,
        )?)?);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let key = identity::Keypair::generate_ed25519();
    let bytes = protect(&key.to_protobuf_encoding()?)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = match options.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return load(path),
        Err(e) => return Err(e.into()),
    };
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(key)
}
#[cfg(not(target_os = "windows"))]
fn protect(bytes: &[u8]) -> Result<Vec<u8>> {
    Ok(bytes.to_vec())
}
#[cfg(not(target_os = "windows"))]
fn unprotect(bytes: &[u8]) -> Result<Vec<u8>> {
    Ok(bytes.to_vec())
}
#[cfg(target_os = "windows")]
fn crypt(bytes: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    use windows::Win32::{
        Foundation::{LocalFree, HLOCAL},
        Security::Cryptography::{
            CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        },
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len().try_into()?,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: input is a valid immutable byte slice for the synchronous DPAPI call.
    unsafe {
        if encrypt {
            CryptProtectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )?;
        } else {
            CryptUnprotectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )?;
        }
        // DPAPI owns this allocation; copy and free it exactly once.
        let result = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(result)
    }
}
#[cfg(target_os = "windows")]
fn protect(bytes: &[u8]) -> Result<Vec<u8>> {
    crypt(bytes, true)
}
#[cfg(target_os = "windows")]
fn unprotect(bytes: &[u8]) -> Result<Vec<u8>> {
    crypt(bytes, false)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_survives_restart_and_corruption_is_not_silently_replaced() {
        let path = std::env::temp_dir().join(format!(
            "aura-identity-{}-{}.key",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let first = load(&path).unwrap();
        let second = load(&path).unwrap();
        assert_eq!(first.public().to_peer_id(), second.public().to_peer_id());
        std::fs::write(&path, b"corrupt").unwrap();
        assert!(load(&path).is_err());
        let _ = std::fs::remove_file(path);
    }
}
