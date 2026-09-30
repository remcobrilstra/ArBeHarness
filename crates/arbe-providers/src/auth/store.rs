//! Credential files: `<auth dir>/<scheme id>.json`, owner-only, written
//! atomically, with a sibling `<scheme id>.lock` held across a refresh.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use arbe_core::ProviderError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{AuthScheme, Issued};

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Stored {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds.
    pub expires_at: i64,
    /// Refresh once `now + refresh_skew_secs >= expires_at`.
    pub refresh_skew_secs: i64,
}

/// Every access and refresh token stored in `dir`, whichever scheme it
/// belongs to. Empty when there are none. Read fresh by each caller — a
/// refresh replaces the tokens on disk — so secrets can be kept out of
/// tool output without knowing which service the session uses.
pub fn secrets_in_dir(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    paths
        .iter()
        .filter_map(|path| fs::read_to_string(path).ok())
        .filter_map(|text| serde_json::from_str::<Stored>(&text).ok())
        .flat_map(|stored| [stored.access_token, stored.refresh_token])
        .collect()
}

pub(super) fn fresh_access_token(
    scheme: &AuthScheme,
    path: &Path,
) -> Result<Option<String>, ProviderError> {
    let Some(stored) = read_optional(scheme, path)? else {
        return Ok(None);
    };
    Ok((now_unix() + stored.refresh_skew_secs < stored.expires_at).then_some(stored.access_token))
}

pub(super) fn read_required(scheme: &AuthScheme, path: &Path) -> Result<Stored, ProviderError> {
    read_optional(scheme, path)?.ok_or_else(|| {
        ProviderError::Auth(format!(
            "not signed in to the {}. {}",
            scheme.display_name,
            scheme.login_hint()
        ))
    })
}

fn read_optional(scheme: &AuthScheme, path: &Path) -> Result<Option<Stored>, ProviderError> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map(Some).map_err(|_| {
            ProviderError::Auth(format!(
                "the {} credential file is unreadable. {}",
                scheme.display_name,
                scheme.login_hint()
            ))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ProviderError::Internal(format!(
            "could not read {}: {e}",
            path.display()
        ))),
    }
}

/// Saves a token pair; returns when it expires (unix seconds).
pub(super) fn write_issued(path: &Path, issued: &Issued) -> Result<i64, ProviderError> {
    let (expires_at, refresh_skew_secs) = expiry_of(issued);
    let stored = Stored {
        access_token: issued.access_token.clone(),
        refresh_token: issued.refresh_token.clone(),
        expires_at,
        refresh_skew_secs,
    };
    let bytes = serde_json::to_vec_pretty(&stored)
        .map_err(|e| ProviderError::Internal(format!("could not encode a credential: {e}")))?;
    write_private(path, &bytes)?;
    Ok(expires_at)
}

/// Deletes the file. `Ok(true)` when there was one.
pub(super) fn remove(path: &Path) -> Result<bool, ProviderError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(ProviderError::Internal(format!(
            "could not remove {}: {e}",
            path.display()
        ))),
    }
}

fn expiry_of(issued: &Issued) -> (i64, i64) {
    let now = now_unix();
    if let Some(expires_in) = issued.expires_in {
        return (now + expires_in, skew_for(expires_in));
    }
    if let Some(exp) = jwt_exp(&issued.access_token) {
        let lifetime = (exp - now).max(1);
        return (exp, skew_for(lifetime));
    }
    // No lifetime at all: use the token briefly, then refresh.
    (now + 60, 15)
}

fn skew_for(lifetime_secs: i64) -> i64 {
    (lifetime_secs / 5).clamp(15, 300)
}

pub(super) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `exp` from a JWT payload, without checking the signature. The token
/// endpoint is what authenticated it; this only decides when to refresh.
fn jwt_exp(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64url_decode(payload)?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value.get("exp")?.as_i64()
}

fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    let mut alphabet = [0u8; 256];
    for (i, byte) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
        .iter()
        .enumerate()
    {
        alphabet[*byte as usize] = i as u8;
    }
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;
    for byte in input.bytes() {
        if byte == b'=' {
            break;
        }
        if !(byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_') {
            return None;
        }
        buf = (buf << 6) | u32::from(alphabet[byte as usize]);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), ProviderError> {
    let parent = path
        .parent()
        .ok_or_else(|| ProviderError::Internal(format!("{} has no directory", path.display())))?;
    fs::create_dir_all(parent).map_err(|e| {
        ProviderError::Internal(format!("could not create {}: {e}", parent.display()))
    })?;
    restrict_dir(parent);
    let tmp = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("cred"),
        std::process::id(),
        now_unix()
    ));
    let write_err = |e: std::io::Error| {
        ProviderError::Internal(format!("could not write {}: {e}", path.display()))
    };
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(write_err)?;
        restrict_file(&tmp);
        file.write_all(bytes).map_err(write_err)?;
        file.sync_all().ok();
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        write_err(e)
    })?;
    restrict_file(path);
    Ok(())
}

fn restrict_dir(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

fn restrict_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// Exclusive lock represented by a sibling `.lock` file. Held across a
/// refresh so two processes cannot redeem the same rotating refresh token.
pub(super) struct FileLock {
    path: PathBuf,
}

impl FileLock {
    pub(super) async fn acquire(credential: &Path) -> Result<Self, ProviderError> {
        let path = credential.with_extension("lock");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                ProviderError::Internal(format!("could not create {}: {e}", parent.display()))
            })?;
        }
        let started = Instant::now();
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Ok(Self { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(&path) {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    if started.elapsed() > Duration::from_secs(40) {
                        return Err(ProviderError::Internal(format!(
                            "timed out waiting for {}",
                            path.display()
                        )));
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(e) => {
                    return Err(ProviderError::Internal(format!(
                        "could not lock {}: {e}",
                        path.display()
                    )));
                }
            }
        }
    }
}

fn lock_is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age > Duration::from_secs(45))
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::temp_dir;
    use super::*;

    fn issued(access: &str, refresh: &str) -> Issued {
        Issued {
            access_token: access.into(),
            refresh_token: refresh.into(),
            expires_in: Some(3600),
        }
    }

    #[test]
    fn jwt_exp_is_read_without_checking_the_signature() {
        // {"exp": 4102444800} — payload only; the signature is irrelevant.
        let token = "eyJhbGciOiJub25lIn0.eyJleHAiOjQxMDI0NDQ4MDB9.e30";
        assert_eq!(jwt_exp(token), Some(4_102_444_800));
        assert_eq!(jwt_exp("not-a-jwt"), None);
    }

    #[test]
    fn a_short_lifetime_refreshes_sooner_than_five_minutes() {
        assert_eq!(skew_for(3600), 300);
        assert_eq!(skew_for(100), 20);
        assert_eq!(skew_for(10), 15);
    }

    #[test]
    fn secrets_come_from_every_credential_file_in_the_directory() {
        let dir = temp_dir("secrets");
        write_issued(
            &dir.join("a.json"),
            &issued("access-a-123456", "refresh-a-123456"),
        )
        .unwrap();
        write_issued(
            &dir.join("b.json"),
            &issued("access-b-123456", "refresh-b-123456"),
        )
        .unwrap();
        fs::write(dir.join("notes.txt"), "not a credential").unwrap();
        fs::write(dir.join("other.json"), r#"{"unrelated": true}"#).unwrap();
        assert_eq!(
            secrets_in_dir(&dir),
            vec![
                "access-a-123456",
                "refresh-a-123456",
                "access-b-123456",
                "refresh-b-123456"
            ]
        );
        assert!(secrets_in_dir(&dir.join("missing")).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_credential_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("mode");
        let path = dir.join("mode.json");
        write_issued(
            &path,
            &issued("access-token-123456", "refresh-token-123456"),
        )
        .unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = fs::remove_dir_all(&dir);
    }
}
