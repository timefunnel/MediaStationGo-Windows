use jfn_mediastation::{
    ApiError, MediaStationClientProfile, MediaStationConnectionProfile, MediaStationProxyMode,
    MediaStationSession,
};
use serde_json::{Value, json};
use std::fmt;
use url::Url;

const ACTIVE_CREDENTIAL_TARGET: &str = "MediaStationGo.Windows.ActiveSession.v1";
const ACCOUNT_CREDENTIAL_PREFIX: &str = "MediaStationGo.Windows.Account.";
const ACCOUNT_CREDENTIAL_SUFFIX: &str = ".v1";
const ACCOUNT_CREDENTIAL_FILTER: &str = "MediaStationGo.Windows.Account.*";
const ACCOUNT_ID_LEN: usize = 64;
const CREDENTIAL_SCHEMA_VERSION: u64 = 3;
const MAX_CREDENTIAL_BYTES: usize = 3_072;
const MAX_CREDENTIAL_TARGET_UNITS: usize = 256;

/// Stable, hash-based credential target for a saved account. Derived from
/// (server, user id) so re-logging-in to the same account reuses its entry.
pub(crate) fn account_credential_target(base_url: &Url, user_id: &str) -> String {
    let digest = account_credential_id(base_url, user_id);
    format!("{ACCOUNT_CREDENTIAL_PREFIX}{digest}{ACCOUNT_CREDENTIAL_SUFFIX}")
}

pub(crate) fn account_credential_id(base_url: &Url, user_id: &str) -> String {
    let identity = format!("{}|{}", base_url.as_str(), user_id);
    sha256_hex(&identity)
}

fn sha256_hex(value: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(value.as_bytes());
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn valid_account_id(value: &str) -> bool {
    value.len() == ACCOUNT_ID_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn account_id_from_target(target: &str) -> Result<&str, CredentialError> {
    let account_id = target
        .strip_prefix(ACCOUNT_CREDENTIAL_PREFIX)
        .and_then(|value| value.strip_suffix(ACCOUNT_CREDENTIAL_SUFFIX))
        .ok_or_else(|| CredentialError::new("credential_account_target_invalid"))?;
    if !valid_account_id(account_id) {
        return Err(CredentialError::new("credential_account_target_invalid"));
    }
    Ok(account_id)
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct StoredSession {
    pub(crate) base_url: Url,
    pub(crate) user_id: String,
    pub(crate) user_name: String,
    pub(crate) connection: MediaStationConnectionProfile,
    access_token: String,
}

impl StoredSession {
    #[cfg(test)]
    pub(crate) fn new(
        base_url: Url,
        user_id: impl Into<String>,
        user_name: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Result<Self, CredentialError> {
        Self::new_with_profile(
            base_url,
            user_id,
            user_name,
            access_token,
            MediaStationConnectionProfile::default_emby(),
        )
    }

    pub(crate) fn new_with_profile(
        base_url: Url,
        user_id: impl Into<String>,
        user_name: impl Into<String>,
        access_token: impl Into<String>,
        connection: MediaStationConnectionProfile,
    ) -> Result<Self, CredentialError> {
        let user_id = user_id.into();
        let user_name = user_name.into();
        let access_token = access_token.into();
        validate_base_url(&base_url)?;
        validate_field("user_id", &user_id, 256)?;
        validate_field("user_name", &user_name, 256)?;
        validate_field("access_token", &access_token, 2_048)?;
        if !connection.is_supported() {
            return Err(CredentialError::new("credential_connection_invalid"));
        }
        // Proxy mode is an application-wide preference. Account credentials
        // retain only the client identity needed by the media server.
        let connection = MediaStationConnectionProfile {
            client: connection.client,
            proxy: MediaStationProxyMode::Direct,
        };
        Ok(Self {
            base_url,
            user_id,
            user_name,
            connection,
            access_token,
        })
    }

    pub(crate) fn to_session_with_proxy(
        &self,
        authorization: String,
        proxy: MediaStationProxyMode,
    ) -> Result<MediaStationSession, ApiError> {
        MediaStationSession::new_with_profile(
            self.base_url.clone(),
            self.user_id.clone(),
            self.access_token.clone(),
            authorization,
            MediaStationConnectionProfile {
                client: self.connection.client,
                proxy,
            },
        )
    }

    pub(crate) fn access_token_secret(&self) -> &str {
        &self.access_token
    }

    pub(crate) fn account_id(&self) -> String {
        account_credential_id(&self.base_url, &self.user_id)
    }

    fn encode(&self) -> Result<Vec<u8>, CredentialError> {
        let bytes = serde_json::to_vec(&json!({
            "version": CREDENTIAL_SCHEMA_VERSION,
            "baseUrl": self.base_url.as_str(),
            "userId": self.user_id,
            "userName": self.user_name,
            "accessToken": self.access_token,
            "connection": {
                "clientProfile": self.connection.client.as_str(),
            },
        }))
        .map_err(|_| CredentialError::new("credential_encode_failed"))?;
        if bytes.len() > MAX_CREDENTIAL_BYTES {
            return Err(CredentialError::new("credential_too_large"));
        }
        Ok(bytes)
    }

    fn decode(bytes: &[u8]) -> Result<Self, CredentialError> {
        if bytes.is_empty() || bytes.len() > MAX_CREDENTIAL_BYTES {
            return Err(CredentialError::new("credential_size_invalid"));
        }
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|_| CredentialError::new("credential_json_invalid"))?;
        let version = value
            .get("version")
            .and_then(Value::as_u64)
            .ok_or_else(|| CredentialError::new("credential_version_unsupported"))?;
        let connection = match version {
            1 => MediaStationConnectionProfile::default_emby(),
            2 | CREDENTIAL_SCHEMA_VERSION => decode_connection_profile(&value)?,
            _ => return Err(CredentialError::new("credential_version_unsupported")),
        };
        let base_url = required_string(&value, "baseUrl")?;
        let base_url = Url::parse(&base_url)
            .map_err(|_| CredentialError::new("credential_base_url_invalid"))?;
        Self::new_with_profile(
            base_url,
            required_string(&value, "userId")?,
            required_string(&value, "userName")?,
            required_string(&value, "accessToken")?,
            connection,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SavedAccount {
    pub(crate) account_id: String,
    pub(crate) session: StoredSession,
}

impl SavedAccount {
    fn from_target(target: &str, session: StoredSession) -> Result<Self, CredentialError> {
        let account_id = account_id_from_target(target)?;
        if session.account_id() != account_id {
            return Err(CredentialError::new("credential_account_mismatch"));
        }
        Ok(Self {
            account_id: account_id.to_string(),
            session,
        })
    }
}

impl fmt::Debug for StoredSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredSession")
            .field("base_url", &self.base_url)
            .field("user_id", &self.user_id)
            .field("user_name", &self.user_name)
            .field("connection", &self.connection)
            .field("access_token", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CredentialError {
    code: &'static str,
}

impl CredentialError {
    const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub(crate) const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Display for CredentialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for CredentialError {}

fn required_string(value: &Value, field: &'static str) -> Result<String, CredentialError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| CredentialError::new("credential_field_missing"))
}

fn decode_connection_profile(
    value: &Value,
) -> Result<MediaStationConnectionProfile, CredentialError> {
    let connection = value
        .get("connection")
        .and_then(Value::as_object)
        .ok_or_else(|| CredentialError::new("credential_connection_invalid"))?;
    let client = connection
        .get("clientProfile")
        .and_then(Value::as_str)
        .and_then(MediaStationClientProfile::parse)
        .ok_or_else(|| CredentialError::new("credential_client_profile_invalid"))?;
    Ok(MediaStationConnectionProfile {
        client,
        proxy: MediaStationProxyMode::Direct,
    })
}

fn validate_base_url(base_url: &Url) -> Result<(), CredentialError> {
    if !matches!(base_url.scheme(), "http" | "https")
        || base_url.host_str().is_none()
        || !base_url.username().is_empty()
        || base_url.password().is_some()
        || base_url.query().is_some()
        || base_url.fragment().is_some()
    {
        return Err(CredentialError::new("credential_base_url_invalid"));
    }
    Ok(())
}

fn validate_field(
    field: &'static str,
    value: &str,
    maximum_len: usize,
) -> Result<(), CredentialError> {
    if value.trim().is_empty() || value.len() > maximum_len || value.chars().any(char::is_control) {
        let code = match field {
            "user_id" => "credential_user_id_invalid",
            "user_name" => "credential_user_name_invalid",
            "access_token" => "credential_token_invalid",
            _ => "credential_field_invalid",
        };
        return Err(CredentialError::new(code));
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct WindowsCredentialStore {
    target_name: String,
}

impl WindowsCredentialStore {
    pub(crate) fn active() -> Self {
        Self {
            target_name: ACTIVE_CREDENTIAL_TARGET.to_string(),
        }
    }

    pub(crate) fn account(base_url: &Url, user_id: &str) -> Self {
        Self {
            target_name: account_credential_target(base_url, user_id),
        }
    }

    pub(crate) fn account_by_id(account_id: &str) -> Result<Self, CredentialError> {
        if !valid_account_id(account_id) {
            return Err(CredentialError::new("credential_account_id_invalid"));
        }
        Ok(Self {
            target_name: format!(
                "{ACCOUNT_CREDENTIAL_PREFIX}{account_id}{ACCOUNT_CREDENTIAL_SUFFIX}"
            ),
        })
    }

    #[cfg(test)]
    fn with_target(target_name: String) -> Self {
        Self { target_name }
    }
}

#[cfg(windows)]
mod platform {
    use super::{
        ACCOUNT_CREDENTIAL_FILTER, CredentialError, MAX_CREDENTIAL_BYTES,
        MAX_CREDENTIAL_TARGET_UNITS, SavedAccount, StoredSession, WindowsCredentialStore,
    };
    use std::ptr;
    use std::slice;
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NOT_FOUND};
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredEnumerateW,
        CredFree, CredReadW, CredWriteW,
    };

    struct CredentialBuffer(*mut CREDENTIALW);

    impl Drop for CredentialBuffer {
        fn drop(&mut self) {
            unsafe { CredFree(self.0.cast()) };
        }
    }

    struct CredentialListBuffer(*mut *mut CREDENTIALW);

    impl Drop for CredentialListBuffer {
        fn drop(&mut self) {
            unsafe { CredFree(self.0.cast()) };
        }
    }

    impl WindowsCredentialStore {
        pub(crate) fn load(&self) -> Result<Option<StoredSession>, CredentialError> {
            let target = wide_string(&self.target_name)?;
            let mut credential = ptr::null_mut();
            if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) } == 0 {
                let error = std::io::Error::last_os_error();
                return match error.raw_os_error().map(|code| code as u32) {
                    Some(ERROR_FILE_NOT_FOUND | ERROR_NOT_FOUND) => Ok(None),
                    _ => Err(CredentialError::new("credential_read_failed")),
                };
            }
            if credential.is_null() {
                return Err(CredentialError::new("credential_read_empty"));
            }
            let credential = CredentialBuffer(credential);
            let raw = unsafe { &*credential.0 };
            let length = raw.CredentialBlobSize as usize;
            if raw.CredentialBlob.is_null() || length == 0 || length > MAX_CREDENTIAL_BYTES {
                return Err(CredentialError::new("credential_size_invalid"));
            }
            let bytes = unsafe { slice::from_raw_parts(raw.CredentialBlob, length) };
            StoredSession::decode(bytes).map(Some)
        }

        pub(crate) fn save(&self, session: &StoredSession) -> Result<(), CredentialError> {
            let mut target = wide_string(&self.target_name)?;
            let mut username = wide_string(&session.user_name)?;
            let mut blob = session.encode()?;
            let credential = CREDENTIALW {
                Type: CRED_TYPE_GENERIC,
                TargetName: target.as_mut_ptr(),
                CredentialBlobSize: blob.len() as u32,
                CredentialBlob: blob.as_mut_ptr(),
                Persist: CRED_PERSIST_LOCAL_MACHINE,
                UserName: username.as_mut_ptr(),
                ..Default::default()
            };
            if unsafe { CredWriteW(&credential, 0) } == 0 {
                return Err(CredentialError::new("credential_write_failed"));
            }
            Ok(())
        }

        pub(crate) fn delete(&self) -> Result<bool, CredentialError> {
            let target = wide_string(&self.target_name)?;
            if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } != 0 {
                return Ok(true);
            }
            let error = std::io::Error::last_os_error();
            match error.raw_os_error().map(|code| code as u32) {
                Some(ERROR_FILE_NOT_FOUND | ERROR_NOT_FOUND) => Ok(false),
                _ => Err(CredentialError::new("credential_delete_failed")),
            }
        }

        pub(crate) fn list_saved_accounts() -> Result<Vec<SavedAccount>, CredentialError> {
            let filter = wide_string(ACCOUNT_CREDENTIAL_FILTER)?;
            let mut count = 0_u32;
            let mut buffer: *mut *mut CREDENTIALW = ptr::null_mut();
            let ok = unsafe { CredEnumerateW(filter.as_ptr(), 0, &mut count, &mut buffer) };
            if ok == 0 {
                let error = std::io::Error::last_os_error();
                return match error.raw_os_error().map(|code| code as u32) {
                    Some(ERROR_FILE_NOT_FOUND | ERROR_NOT_FOUND) => Ok(Vec::new()),
                    _ => Err(CredentialError::new("credential_enumerate_failed")),
                };
            }
            if buffer.is_null() {
                return if count == 0 {
                    Ok(Vec::new())
                } else {
                    Err(CredentialError::new("credential_enumerate_empty"))
                };
            }
            let buffer = CredentialListBuffer(buffer);
            let entries = unsafe { slice::from_raw_parts(buffer.0, count as usize) };
            let mut accounts = Vec::with_capacity(entries.len());
            for entry in entries {
                if entry.is_null() {
                    return Err(CredentialError::new("credential_entry_empty"));
                }
                let credential = unsafe { &**entry };
                if credential.Type != CRED_TYPE_GENERIC
                    || credential.CredentialBlob.is_null()
                    || credential.CredentialBlobSize == 0
                {
                    return Err(CredentialError::new("credential_account_invalid"));
                }
                let target = credential_target(credential.TargetName)?;
                let length = credential.CredentialBlobSize as usize;
                if length > MAX_CREDENTIAL_BYTES {
                    return Err(CredentialError::new("credential_size_invalid"));
                }
                let bytes = unsafe { slice::from_raw_parts(credential.CredentialBlob, length) };
                let session = StoredSession::decode(bytes)?;
                accounts.push(SavedAccount::from_target(&target, session)?);
            }
            accounts.sort_by(|left, right| {
                left.session
                    .base_url
                    .as_str()
                    .cmp(right.session.base_url.as_str())
                    .then_with(|| left.session.user_name.cmp(&right.session.user_name))
                    .then_with(|| left.session.user_id.cmp(&right.session.user_id))
            });
            Ok(accounts)
        }
    }

    fn credential_target(target: *mut u16) -> Result<String, CredentialError> {
        if target.is_null() {
            return Err(CredentialError::new("credential_account_target_invalid"));
        }
        let length = (0..MAX_CREDENTIAL_TARGET_UNITS)
            .find(|index| unsafe { *target.add(*index) == 0 })
            .ok_or_else(|| CredentialError::new("credential_account_target_invalid"))?;
        let units = unsafe { slice::from_raw_parts(target, length) };
        String::from_utf16(units)
            .map_err(|_| CredentialError::new("credential_account_target_invalid"))
    }

    fn wide_string(value: &str) -> Result<Vec<u16>, CredentialError> {
        if value.is_empty() || value.encode_utf16().any(|unit| unit == 0) {
            return Err(CredentialError::new("credential_target_invalid"));
        }
        Ok(value.encode_utf16().chain([0]).collect())
    }
}

#[cfg(not(windows))]
impl WindowsCredentialStore {
    pub(crate) fn load(&self) -> Result<Option<StoredSession>, CredentialError> {
        Err(CredentialError::new("credential_store_unsupported"))
    }

    pub(crate) fn save(&self, _session: &StoredSession) -> Result<(), CredentialError> {
        Err(CredentialError::new("credential_store_unsupported"))
    }

    pub(crate) fn delete(&self) -> Result<bool, CredentialError> {
        Err(CredentialError::new("credential_store_unsupported"))
    }

    pub(crate) fn list_saved_accounts() -> Result<Vec<SavedAccount>, CredentialError> {
        Err(CredentialError::new("credential_store_unsupported"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> StoredSession {
        StoredSession::new(
            Url::parse("https://media.example/base").expect("URL should parse"),
            "user-1",
            "Test User",
            "credential-roundtrip-secret",
        )
        .expect("stored session should be valid")
    }

    #[test]
    fn credential_codec_round_trips_and_debug_redacts() {
        let session = session();
        let encoded = session.encode().expect("credential should encode");
        let decoded = StoredSession::decode(&encoded).expect("credential should decode");
        let payload: Value =
            serde_json::from_slice(&encoded).expect("credential should contain valid JSON");

        assert_eq!(decoded, session);
        assert_eq!(
            payload["connection"]["clientProfile"],
            "mediastation_windows"
        );
        let debug = format!("{decoded:?}");
        assert!(!debug.contains("credential-roundtrip-secret"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn emby_credential_codec_round_trips_client_identity() {
        let profile = MediaStationConnectionProfile::emby(MediaStationClientProfile::SenPlayer);
        let session = StoredSession::new_with_profile(
            Url::parse("https://emby.example/base").expect("URL should parse"),
            "user-2",
            "Emby User",
            "standard-emby-roundtrip-secret",
            profile,
        )
        .expect("stored session should be valid");

        let encoded = session.encode().expect("credential should encode");
        let decoded = StoredSession::decode(&encoded).expect("credential should decode");

        assert_eq!(decoded, session);
        assert_eq!(decoded.connection, profile);
        assert!(!format!("{decoded:?}").contains("standard-emby-roundtrip-secret"));
    }

    #[test]
    fn version_one_credentials_migrate_to_default_emby_identity() {
        let encoded = br#"{"version":1,"baseUrl":"https://media.example","userId":"u","userName":"n","accessToken":"t"}"#;

        let decoded = StoredSession::decode(encoded).expect("v1 credential should migrate");

        assert_eq!(
            decoded.connection,
            MediaStationConnectionProfile::default_emby()
        );
    }

    #[test]
    fn credential_codec_migrates_legacy_proxy_to_global_default() {
        let proxied_msg = br#"{"version":2,"baseUrl":"https://media.example","userId":"u","userName":"n","accessToken":"t","connection":{"clientProfile":"mediastation_go","proxyMode":"system"}}"#;
        let decoded =
            StoredSession::decode(proxied_msg).expect("legacy proxy setting should migrate");
        assert_eq!(decoded.connection.proxy, MediaStationProxyMode::Direct);
        assert_eq!(
            decoded.connection.client,
            MediaStationClientProfile::MediaStationWindows
        );

        let direct_emby = br#"{"version":2,"baseUrl":"https://media.example","userId":"u","userName":"n","accessToken":"t","connection":{"clientProfile":"senplayer","proxyMode":"direct"}}"#;
        let decoded = StoredSession::decode(direct_emby)
            .expect("standard Emby client profile should migrate");
        assert_eq!(decoded.connection.proxy, MediaStationProxyMode::Direct);
        assert_eq!(
            decoded.connection.client,
            MediaStationClientProfile::SenPlayer
        );
    }

    #[test]
    fn credential_codec_never_persists_proxy_mode() {
        let stored = StoredSession::new_with_profile(
            Url::parse("https://media.example").expect("URL should parse"),
            "user",
            "User",
            "secret",
            MediaStationConnectionProfile {
                client: MediaStationClientProfile::MediaStationWindows,
                proxy: MediaStationProxyMode::System,
            },
        )
        .expect("stored session should be valid");

        let encoded = stored.encode().expect("credential should encode");
        let payload: Value = serde_json::from_slice(&encoded).expect("credential should be JSON");
        assert_eq!(payload["version"], CREDENTIAL_SCHEMA_VERSION);
        assert!(payload["connection"].get("proxyMode").is_none());
        assert_eq!(stored.connection.proxy, MediaStationProxyMode::Direct);
    }

    #[test]
    fn credential_codec_rejects_unknown_schema() {
        let encoded = br#"{"version":4,"baseUrl":"https://media.example","userId":"u","userName":"n","accessToken":"t"}"#;

        let error = StoredSession::decode(encoded).expect_err("unknown schema must fail");

        assert_eq!(error.code(), "credential_version_unsupported");
    }

    #[test]
    fn account_targets_are_stable_and_bound_to_the_stored_identity() {
        let session = session();
        let account_id = session.account_id();
        let target = account_credential_target(&session.base_url, &session.user_id);
        let saved = SavedAccount::from_target(&target, session.clone())
            .expect("matching account target should be accepted");

        assert_eq!(account_id.len(), ACCOUNT_ID_LEN);
        assert_eq!(
            account_id,
            "f5d0166a364b76b02949fd68fa1ef3ef43a76596eb786ad1fb2bb5c189d29d77"
        );
        assert_eq!(saved.account_id, account_id);
        assert_eq!(saved.session, session);

        let other_target = account_credential_target(&saved.session.base_url, "other-user");
        let error = SavedAccount::from_target(&other_target, saved.session)
            .expect_err("a target for another identity must fail");
        assert_eq!(error.code(), "credential_account_mismatch");
    }

    #[test]
    fn saved_account_targets_reject_invalid_names() {
        let session = session();
        let invalid_targets = [
            "MediaStationGo.Windows.Account.short.v1".to_string(),
            format!("MediaStationGo.Windows.Account.{}.v2", session.account_id()),
            format!("MediaStationGo.Windows.Other.{}.v1", session.account_id()),
        ];
        for target in invalid_targets {
            let error = SavedAccount::from_target(&target, session.clone())
                .expect_err("an invalid account target must fail");
            assert_eq!(error.code(), "credential_account_target_invalid");
        }
    }

    #[test]
    fn account_store_rejects_renderer_supplied_invalid_ids() {
        for invalid in [
            "",
            "abcd",
            &"g".repeat(ACCOUNT_ID_LEN),
            &"A".repeat(ACCOUNT_ID_LEN),
        ] {
            let error = WindowsCredentialStore::account_by_id(invalid)
                .expect_err("invalid account id must fail");
            assert_eq!(error.code(), "credential_account_id_invalid");
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "reads MediaStationGo account entries from Windows Credential Manager"]
    fn windows_saved_account_enumeration_uses_prefix_filter() {
        let accounts = WindowsCredentialStore::list_saved_accounts()
            .expect("prefix-filtered account enumeration should succeed");
        for account in accounts {
            assert_eq!(account.account_id, account.session.account_id());
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "writes an isolated test entry to Windows Credential Manager"]
    fn windows_credential_store_round_trips_and_cleans_up() {
        let store = WindowsCredentialStore::with_target(format!(
            "MediaStationGo.Windows.Test.{}",
            std::process::id()
        ));
        let _ = store.delete();
        store.save(&session()).expect("credential should save");

        let loaded = store
            .load()
            .expect("credential should load")
            .expect("credential should exist");
        assert_eq!(loaded, session());
        assert!(store.delete().expect("credential should delete"));
        assert!(
            store
                .load()
                .expect("credential absence should load")
                .is_none()
        );
    }
}
