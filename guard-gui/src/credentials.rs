//! The `guard explain` API key and the Jira API token in the Windows
//! Credential Manager (generic credentials, local to this user and machine),
//! one per endpoint host.
//!
//! This is the only place the GUI keeps either secret: never in eframe persistence,
//! a file, the log or a screenshot. Callers read it right before spawning the
//! explain child and drop it afterwards. Other platforms have no store yet:
//! every call returns an error and the GUI falls back to the process
//! environment.

#[cfg(test)]
mod tests;

/// The prefix of the GUI's Credential Manager entries.
pub const TARGET_PREFIX: &str = "guard-gui/explain-api-key";

/// The entry of the key saved for `host` (see
/// [`crate::ai_settings::endpoint_host`]): one key per endpoint, so a key is
/// only ever sent to the host it was saved for.
pub fn target(host: &str) -> String {
    format!("{TARGET_PREFIX}/{host}")
}

/// The prefix of the Jira API token entries.
pub const JIRA_PREFIX: &str = "guard-gui/jira-api-token";

/// The entry of the Jira token saved for `host`: a token is only ever sent
/// to the site it was saved for.
pub fn jira_target(host: &str) -> String {
    format!("{JIRA_PREFIX}/{host}")
}

pub fn load_jira(host: &str) -> Result<Option<String>, String> {
    read(&jira_target(host))
}

pub fn save_jira(host: &str, secret: &str) -> Result<(), String> {
    write(&jira_target(host), secret)
}

pub fn clear_jira(host: &str) -> Result<(), String> {
    delete(&jira_target(host))
}

/// The key saved for `host`, `Ok(None)` when there is none.
pub fn load(host: &str) -> Result<Option<String>, String> {
    read(&target(host))
}

pub fn save(host: &str, secret: &str) -> Result<(), String> {
    write(&target(host), secret)
}

/// Removes the key saved for `host`; a missing one is not an error.
pub fn clear(host: &str) -> Result<(), String> {
    delete(&target(host))
}

/// `text` as a NUL-terminated UTF-16 string.
#[cfg_attr(not(windows), allow(dead_code))]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn read(target: &str) -> Result<Option<String>, String> {
    use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
    use windows_sys::Win32::Security::Credentials::{
        CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
    };
    let name = wide(target);
    let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: `name` is NUL-terminated and outlives the call; on success the
    // API allocates `cred`, which is freed below with CredFree.
    let ok = unsafe { CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) };
    if ok == 0 {
        // SAFETY: reads the calling thread's last error code.
        let code = unsafe { GetLastError() };
        return if code == ERROR_NOT_FOUND {
            Ok(None)
        } else {
            Err(format!("Credential Manager read failed (error {code})"))
        };
    }
    // SAFETY: CredReadW succeeded, so `cred` points at a valid CREDENTIALW
    // whose blob holds `CredentialBlobSize` bytes.
    let bytes = unsafe {
        let c = &*cred;
        let blob = if c.CredentialBlob.is_null() || c.CredentialBlobSize == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec()
        };
        CredFree(cred.cast());
        blob
    };
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "the saved key is not UTF-8".to_string())
}

#[cfg(windows)]
fn write(target: &str, secret: &str) -> Result<(), String> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredWriteW,
    };
    let mut name = wide(target);
    let mut user = wide("guard-gui");
    let mut blob = secret.as_bytes().to_vec();
    // SAFETY: an all-zero CREDENTIALW is valid (null pointers, zero sizes).
    let mut cred: CREDENTIALW = unsafe { std::mem::zeroed() };
    cred.Type = CRED_TYPE_GENERIC;
    cred.TargetName = name.as_mut_ptr();
    cred.UserName = user.as_mut_ptr();
    cred.CredentialBlobSize = blob.len() as u32;
    cred.CredentialBlob = blob.as_mut_ptr();
    // Local to this user on this machine; never roams with the profile.
    cred.Persist = CRED_PERSIST_LOCAL_MACHINE;
    // SAFETY: every pointer in `cred` refers to a live buffer above.
    let ok = unsafe { CredWriteW(&cred, 0) };
    // Best effort: do not leave the plain bytes in this buffer.
    blob.iter_mut().for_each(|b| *b = 0);
    if ok == 0 {
        // SAFETY: reads the calling thread's last error code.
        let code = unsafe { GetLastError() };
        return Err(format!("Credential Manager write failed (error {code})"));
    }
    Ok(())
}

#[cfg(windows)]
fn delete(target: &str) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
    use windows_sys::Win32::Security::Credentials::{CRED_TYPE_GENERIC, CredDeleteW};
    let name = wide(target);
    // SAFETY: `name` is NUL-terminated and outlives the call.
    let ok = unsafe { CredDeleteW(name.as_ptr(), CRED_TYPE_GENERIC, 0) };
    if ok == 0 {
        // SAFETY: reads the calling thread's last error code.
        let code = unsafe { GetLastError() };
        if code != ERROR_NOT_FOUND {
            return Err(format!("Credential Manager delete failed (error {code})"));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
const UNSUPPORTED: &str = "no credential store on this platform: set GUARD_EXPLAIN_API_KEY";

#[cfg(not(windows))]
fn read(_target: &str) -> Result<Option<String>, String> {
    Err(UNSUPPORTED.into())
}

#[cfg(not(windows))]
fn write(_target: &str, _secret: &str) -> Result<(), String> {
    Err(UNSUPPORTED.into())
}

#[cfg(not(windows))]
fn delete(_target: &str) -> Result<(), String> {
    Err(UNSUPPORTED.into())
}
