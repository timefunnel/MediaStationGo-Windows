use jfn_mediastation::{ApiError, MediaStationSession};
use serde_json::{Value, json};
use std::fmt;
use url::Url;

const ACTIVE_CREDENTIAL_TARGET: &str = "MediaStationGo.Windows.ActiveSession.v1";
const CREDENTIAL_SCHEMA_VERSION: u64 = 1;
const MAX_CREDENTIAL_BYTES: usize = 2_560;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct StoredSession {
    pub(crate) base_url: Url,
    pub(crate) user_id: String,
    pub(crate) user_name: String,
    access_token: String,
}

impl StoredSession {
    pub(crate) fn new(
        base_url: Url,
        user_id: impl Into<String>,
        user_name: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Result<Self, CredentialError> {
        let user_id = user_id.into();
        let user_name = user_name.into();
        let access_token = access_token.into();
        validate_base_url(&base_url)?;
        validate_field("user_id", &user_id, 256)?;
        validate_field("user_name", &user_name, 256)?;
        validate_field("access_token", &access_token, 2_048)?;
        Ok(Self {
            base_url,
            user_id,
            user_name,
            access_token,
        })
    }

    pub(crate) fn to_session(
        &self,
        authorization: String,
    ) -> Result<MediaStationSession, ApiError> {
        MediaStationSession::new(
            self.base_url.clone(),
            self.user_id.clone(),
            self.access_token.clone(),
            authorization,
        )
    }

    pub(crate) fn access_token_secret(&self) -> &str {
        &self.access_token
    }

    fn encode(&self) -> Result<Vec<u8>, CredentialError> {
        let bytes = serde_json::to_vec(&json!({
            "version": CREDENTIAL_SCHEMA_VERSION,
            "baseUrl": self.base_url.as_str(),
            "userId": self.user_id,
            "userName": self.user_name,
            "accessToken": self.access_token,
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
        if value.get("version").and_then(Value::as_u64) != Some(CREDENTIAL_SCHEMA_VERSION) {
            return Err(CredentialError::new("credential_version_unsupported"));
        }
        let base_url = required_string(&value, "baseUrl")?;
        let base_url = Url::parse(&base_url)
            .map_err(|_| CredentialError::new("credential_base_url_invalid"))?;
        Self::new(
            base_url,
            required_string(&value, "userId")?,
            required_string(&value, "userName")?,
            required_string(&value, "accessToken")?,
        )
    }
}

impl fmt::Debug for StoredSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredSession")
            .field("base_url", &self.base_url)
            .field("user_id", &self.user_id)
            .field("user_name", &self.user_name)
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

pub(crate) struct WindowsCredentialStore {
    target_name: String,
}

impl WindowsCredentialStore {
    pub(crate) fn active() -> Self {
        Self {
            target_name: ACTIVE_CREDENTIAL_TARGET.to_string(),
        }
    }

    #[cfg(test)]
    fn with_target(target_name: String) -> Self {
        Self { target_name }
    }
}

#[cfg(windows)]
mod platform {
    use super::{CredentialError, MAX_CREDENTIAL_BYTES, StoredSession, WindowsCredentialStore};
    use std::ptr;
    use std::slice;
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NOT_FOUND};
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree,
        CredReadW, CredWriteW,
    };

    struct CredentialBuffer(*mut CREDENTIALW);

    impl Drop for CredentialBuffer {
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

        assert_eq!(decoded, session);
        let debug = format!("{decoded:?}");
        assert!(!debug.contains("credential-roundtrip-secret"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn credential_codec_rejects_unknown_schema() {
        let encoded = br#"{"version":2,"baseUrl":"https://media.example","userId":"u","userName":"n","accessToken":"t"}"#;

        let error = StoredSession::decode(encoded).expect_err("unknown schema must fail");

        assert_eq!(error.code(), "credential_version_unsupported");
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
