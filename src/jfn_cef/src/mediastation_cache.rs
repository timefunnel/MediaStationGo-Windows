use jfn_mediastation::{MediaImage, MediaImageRef, MediaImageType, MediaStationSession};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::NamedTempFile;

const HOME_SCHEMA_VERSION: u64 = 1;
const IMAGE_SCHEMA_VERSION: u64 = 1;
const MAX_HOME_SNAPSHOT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_IMAGE_METADATA_BYTES: u64 = 16 * 1024;

#[derive(Debug)]
pub(crate) struct CacheError {
    operation: &'static str,
    detail: String,
}

impl CacheError {
    fn io(operation: &'static str, error: &std::io::Error) -> Self {
        Self {
            operation,
            detail: format!("io_kind={:?}", error.kind()),
        }
    }

    fn invalid(operation: &'static str, detail: impl Into<String>) -> Self {
        Self {
            operation,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for CacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} failed: {}", self.operation, self.detail)
    }
}

pub(crate) struct HomeSnapshot {
    pub(crate) saved_at_ms: u64,
    pub(crate) payload: Value,
}

pub(crate) struct HomeSnapshotCache {
    root: PathBuf,
}

impl HomeSnapshotCache {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) fn load(
        &self,
        session: &MediaStationSession,
    ) -> Result<Option<HomeSnapshot>, CacheError> {
        let account_key = account_cache_key(session);
        let path = self.root.join(format!("{account_key}.json"));
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(CacheError::io("home_snapshot_metadata", &error)),
        };
        if metadata.len() > MAX_HOME_SNAPSHOT_BYTES {
            return Err(CacheError::invalid(
                "home_snapshot_read",
                "snapshot exceeds size limit",
            ));
        }
        let bytes =
            fs::read(&path).map_err(|error| CacheError::io("home_snapshot_read", &error))?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| CacheError::invalid("home_snapshot_decode", "invalid JSON"))?;
        let object = value
            .as_object()
            .ok_or_else(|| CacheError::invalid("home_snapshot_decode", "root is not an object"))?;
        if object.get("schemaVersion").and_then(Value::as_u64) != Some(HOME_SCHEMA_VERSION) {
            return Err(CacheError::invalid(
                "home_snapshot_decode",
                "unsupported schema version",
            ));
        }
        if object.get("accountKey").and_then(Value::as_str) != Some(account_key.as_str()) {
            return Err(CacheError::invalid(
                "home_snapshot_decode",
                "account key mismatch",
            ));
        }
        let saved_at_ms = object
            .get("savedAtMs")
            .and_then(Value::as_u64)
            .ok_or_else(|| CacheError::invalid("home_snapshot_decode", "missing savedAtMs"))?;
        let payload = object
            .get("payload")
            .filter(|payload| payload.is_object())
            .cloned()
            .ok_or_else(|| CacheError::invalid("home_snapshot_decode", "missing payload"))?;
        Ok(Some(HomeSnapshot {
            saved_at_ms,
            payload,
        }))
    }

    pub(crate) fn store(
        &self,
        session: &MediaStationSession,
        payload: &Value,
    ) -> Result<(), CacheError> {
        let account_key = account_cache_key(session);
        let value = json!({
            "schemaVersion": HOME_SCHEMA_VERSION,
            "accountKey": account_key,
            "savedAtMs": now_ms(),
            "payload": payload,
        });
        let bytes = serde_json::to_vec(&value)
            .map_err(|_| CacheError::invalid("home_snapshot_encode", "serialization failed"))?;
        if bytes.len() as u64 > MAX_HOME_SNAPSHOT_BYTES {
            return Err(CacheError::invalid(
                "home_snapshot_write",
                "snapshot exceeds size limit",
            ));
        }
        write_atomic(&self.root, &format!("{account_key}.json"), &bytes)
    }

    pub(crate) fn remove(&self, session: &MediaStationSession) -> Result<(), CacheError> {
        let path = self
            .root
            .join(format!("{}.json", account_cache_key(session)));
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(CacheError::io("home_snapshot_remove", &error)),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ImageCacheStats {
    pub(crate) bytes: u64,
    pub(crate) count: usize,
}

pub(crate) struct ImageDiskCache {
    root: PathBuf,
    maximum_bytes: u64,
}

impl ImageDiskCache {
    pub(crate) fn new(root: PathBuf, maximum_bytes: u64) -> Self {
        Self {
            root,
            maximum_bytes,
        }
    }

    pub(crate) fn get(&self, key: &str) -> Result<Option<MediaImage>, CacheError> {
        let data_path = self.data_path(key);
        let metadata_path = self.metadata_path(key);
        let data_exists = data_path.exists();
        let metadata_exists = metadata_path.exists();
        if !data_exists && !metadata_exists {
            return Ok(None);
        }
        if !data_exists || !metadata_exists {
            return Err(CacheError::invalid(
                "image_cache_read",
                "cache entry is incomplete",
            ));
        }
        let metadata_bytes = read_limited(
            &metadata_path,
            MAX_IMAGE_METADATA_BYTES,
            "image_cache_metadata",
        )?;
        let metadata: Value = serde_json::from_slice(&metadata_bytes)
            .map_err(|_| CacheError::invalid("image_cache_metadata", "invalid JSON"))?;
        if metadata.get("schemaVersion").and_then(Value::as_u64) != Some(IMAGE_SCHEMA_VERSION)
            || metadata.get("key").and_then(Value::as_str) != Some(key)
        {
            return Err(CacheError::invalid(
                "image_cache_metadata",
                "metadata does not match cache key",
            ));
        }
        let content_type = metadata
            .get("contentType")
            .and_then(Value::as_str)
            .filter(|value| value.starts_with("image/"))
            .map(str::to_string)
            .ok_or_else(|| CacheError::invalid("image_cache_metadata", "invalid content type"))?;
        let expected_bytes = metadata
            .get("byteCount")
            .and_then(Value::as_u64)
            .ok_or_else(|| CacheError::invalid("image_cache_metadata", "missing byte count"))?;
        let bytes = read_limited(&data_path, self.maximum_bytes, "image_cache_read")?;
        if bytes.len() as u64 != expected_bytes {
            return Err(CacheError::invalid(
                "image_cache_read",
                "cached byte count mismatch",
            ));
        }
        self.touch_metadata(key, &content_type, expected_bytes)?;
        Ok(Some(MediaImage {
            bytes,
            content_type,
        }))
    }

    pub(crate) fn put(&self, key: &str, image: &MediaImage) -> Result<(), CacheError> {
        if image.bytes.is_empty() || image.bytes.len() as u64 > self.maximum_bytes {
            return Err(CacheError::invalid(
                "image_cache_write",
                "image size is outside cache limits",
            ));
        }
        if !image.content_type.starts_with("image/") {
            return Err(CacheError::invalid(
                "image_cache_write",
                "content type is not an image",
            ));
        }
        write_atomic(&self.root, &format!("{key}.bin"), &image.bytes)?;
        self.touch_metadata(key, &image.content_type, image.bytes.len() as u64)?;
        self.prune()?;
        Ok(())
    }

    pub(crate) fn remove(&self, key: &str) -> Result<(), CacheError> {
        remove_file_if_present(&self.data_path(key), "image_cache_remove")?;
        remove_file_if_present(&self.metadata_path(key), "image_cache_remove")
    }

    pub(crate) fn stats(&self) -> Result<ImageCacheStats, CacheError> {
        let mut stats = ImageCacheStats::default();
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(stats),
            Err(error) => return Err(CacheError::io("image_cache_stats", &error)),
        };
        for entry in entries {
            let entry = entry.map_err(|error| CacheError::io("image_cache_stats", &error))?;
            if entry.path().extension().and_then(|value| value.to_str()) != Some("bin") {
                continue;
            }
            let metadata = entry
                .metadata()
                .map_err(|error| CacheError::io("image_cache_stats", &error))?;
            stats.bytes = stats.bytes.saturating_add(metadata.len());
            stats.count = stats.count.saturating_add(1);
        }
        Ok(stats)
    }

    pub(crate) fn clear(&self) -> Result<ImageCacheStats, CacheError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ImageCacheStats::default());
            }
            Err(error) => return Err(CacheError::io("image_cache_clear", &error)),
        };
        for entry in entries {
            let entry = entry.map_err(|error| CacheError::io("image_cache_clear", &error))?;
            let path = entry.path();
            if path.is_file() {
                fs::remove_file(path)
                    .map_err(|error| CacheError::io("image_cache_clear", &error))?;
            }
        }
        self.stats()
    }

    fn touch_metadata(
        &self,
        key: &str,
        content_type: &str,
        byte_count: u64,
    ) -> Result<(), CacheError> {
        let metadata = json!({
            "schemaVersion": IMAGE_SCHEMA_VERSION,
            "key": key,
            "contentType": content_type,
            "byteCount": byte_count,
            "lastAccessMs": now_ms(),
        });
        let bytes = serde_json::to_vec(&metadata)
            .map_err(|_| CacheError::invalid("image_cache_metadata", "serialization failed"))?;
        write_atomic(&self.root, &format!("{key}.json"), &bytes)
    }

    fn prune(&self) -> Result<(), CacheError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(CacheError::io("image_cache_prune", &error)),
        };
        let mut candidates = Vec::new();
        let mut total_bytes = 0_u64;
        for entry in entries {
            let entry = entry.map_err(|error| CacheError::io("image_cache_prune", &error))?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Some(key) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            let metadata_bytes =
                match read_limited(&path, MAX_IMAGE_METADATA_BYTES, "image_cache_prune") {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        self.remove(key)?;
                        continue;
                    }
                };
            let metadata: Value = match serde_json::from_slice(&metadata_bytes) {
                Ok(metadata) => metadata,
                Err(_) => {
                    self.remove(key)?;
                    continue;
                }
            };
            let Some(byte_count) = metadata.get("byteCount").and_then(Value::as_u64) else {
                self.remove(key)?;
                continue;
            };
            if !self.data_path(key).is_file() {
                self.remove(key)?;
                continue;
            }
            let last_access_ms = metadata
                .get("lastAccessMs")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            total_bytes = total_bytes.saturating_add(byte_count);
            candidates.push((last_access_ms, key.to_string(), byte_count));
        }
        candidates.sort_by_key(|(last_access_ms, _, _)| *last_access_ms);
        for (_, key, byte_count) in candidates {
            if total_bytes <= self.maximum_bytes {
                break;
            }
            self.remove(&key)?;
            total_bytes = total_bytes.saturating_sub(byte_count);
        }
        Ok(())
    }

    fn data_path(&self, key: &str) -> PathBuf {
        self.root.join(format!("{key}.bin"))
    }

    fn metadata_path(&self, key: &str) -> PathBuf {
        self.root.join(format!("{key}.json"))
    }
}

pub(crate) fn image_cache_key(
    session: &MediaStationSession,
    image: &MediaImageRef,
    max_width: u32,
) -> String {
    let image_type = match image.image_type {
        MediaImageType::Primary => "primary",
        MediaImageType::Thumb => "thumb",
        MediaImageType::Backdrop => "backdrop",
    };
    let source = format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}",
        session.base_url.as_str().trim_end_matches('/'),
        session.user_id,
        image.item_id,
        image_type,
        image
            .image_index
            .map_or_else(String::new, |value| value.to_string()),
        image.tag,
        max_width,
    );
    hex_sha256(source.as_bytes())
}

fn account_cache_key(session: &MediaStationSession) -> String {
    hex_sha256(
        format!(
            "{}\n{}",
            session.base_url.as_str().trim_end_matches('/'),
            session.user_id
        )
        .as_bytes(),
    )
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn read_limited(
    path: &Path,
    maximum_bytes: u64,
    operation: &'static str,
) -> Result<Vec<u8>, CacheError> {
    let metadata = fs::metadata(path).map_err(|error| CacheError::io(operation, &error))?;
    if metadata.len() > maximum_bytes {
        return Err(CacheError::invalid(operation, "file exceeds size limit"));
    }
    fs::read(path).map_err(|error| CacheError::io(operation, &error))
}

fn write_atomic(root: &Path, file_name: &str, bytes: &[u8]) -> Result<(), CacheError> {
    fs::create_dir_all(root).map_err(|error| CacheError::io("cache_directory_create", &error))?;
    let mut temporary = NamedTempFile::new_in(root)
        .map_err(|error| CacheError::io("cache_temporary_create", &error))?;
    temporary
        .write_all(bytes)
        .map_err(|error| CacheError::io("cache_temporary_write", &error))?;
    temporary
        .flush()
        .map_err(|error| CacheError::io("cache_temporary_flush", &error))?;
    let target = root.join(file_name);
    remove_file_if_present(&target, "cache_replace")?;
    temporary
        .persist(target)
        .map_err(|error| CacheError::io("cache_persist", &error.error))?;
    Ok(())
}

fn remove_file_if_present(path: &Path, operation: &'static str) -> Result<(), CacheError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CacheError::io(operation, &error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use url::Url;

    fn session(user_id: &str) -> MediaStationSession {
        MediaStationSession::new(
            Url::parse("https://media.example.test").expect("base URL should parse"),
            user_id,
            "token",
            "MediaBrowser Client=\"test\"",
        )
        .expect("session should be valid")
    }

    #[test]
    fn home_snapshots_are_isolated_by_account() {
        let root = tempdir().expect("temporary directory should be created");
        let cache = HomeSnapshotCache::new(root.path().to_path_buf());
        let first = session("first-user");
        let second = session("second-user");
        cache
            .store(&first, &json!({ "libraries": [{ "id": "library-1" }] }))
            .expect("snapshot should be stored");

        let loaded = cache
            .load(&first)
            .expect("snapshot should load")
            .expect("snapshot should exist");
        assert_eq!(loaded.payload["libraries"][0]["id"], "library-1");
        assert!(
            cache
                .load(&second)
                .expect("cache lookup should succeed")
                .is_none()
        );
    }

    #[test]
    fn image_keys_include_account_tag_and_width() {
        let image = MediaImageRef {
            item_id: "item-1".to_string(),
            image_type: MediaImageType::Primary,
            image_index: None,
            tag: "tag-a".to_string(),
        };
        let first = image_cache_key(&session("first-user"), &image, 360);
        let second_account = image_cache_key(&session("second-user"), &image, 360);
        let second_width = image_cache_key(&session("first-user"), &image, 640);
        let mut second_tag_image = image;
        second_tag_image.tag = "tag-b".to_string();
        let second_tag = image_cache_key(&session("first-user"), &second_tag_image, 360);

        assert_ne!(first, second_account);
        assert_ne!(first, second_width);
        assert_ne!(first, second_tag);
    }

    #[test]
    fn image_cache_evicts_least_recently_used_entries() {
        let root = tempdir().expect("temporary directory should be created");
        let cache = ImageDiskCache::new(root.path().to_path_buf(), 6);
        cache
            .put(
                "first",
                &MediaImage {
                    bytes: vec![1, 2, 3, 4],
                    content_type: "image/jpeg".to_string(),
                },
            )
            .expect("first image should be cached");
        std::thread::sleep(std::time::Duration::from_millis(2));
        cache
            .put(
                "second",
                &MediaImage {
                    bytes: vec![5, 6, 7, 8],
                    content_type: "image/png".to_string(),
                },
            )
            .expect("second image should be cached");

        assert!(cache.get("first").expect("lookup should succeed").is_none());
        assert!(
            cache
                .get("second")
                .expect("lookup should succeed")
                .is_some()
        );
        assert_eq!(
            cache.stats().expect("stats should load"),
            ImageCacheStats { bytes: 4, count: 1 }
        );
    }
}
