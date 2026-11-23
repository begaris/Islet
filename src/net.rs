//! Blocking HTTPS for the worker threads, on WinHTTP (part of Windows: no
//! dependency, no TLS library in the exe, the system proxy settings apply).
//!
//! Nothing in Islet calls this unless the user switched the feature on in the
//! settings file: weather, AI chat and currency rates are all off by default.

use std::ffi::c_void;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::*;

struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// Splits `https://host[:port]/path?query` into (secure, host, port, path).
/// Plain `http://` is accepted for this PC only (a local model server), so a
/// key or a question never crosses the network unencrypted.
fn split(url: &str) -> Option<(bool, &str, u16, &str)> {
    let (secure, rest) = match url.strip_prefix("https://") {
        Some(rest) => (true, rest),
        None => (false, url.strip_prefix("http://")?),
    };
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().ok()?),
        None => (authority, if secure { 443 } else { 80 }),
    };
    let local = matches!(host, "localhost" | "127.0.0.1");
    (!host.is_empty() && (secure || local)).then_some((secure, host, port, path))
}

/// Percent-encodes a query value.
pub fn escape(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Sends a request (POST when there is a body) and feeds the response body to
/// `on_data` (with the HTTP status) as it arrives; returning `false` from it
/// stops the transfer. Returns the HTTP status.
pub fn request(url: &str, headers: &[(&str, &str)], body: Option<&[u8]>, on_data: &mut dyn FnMut(u32, &[u8]) -> bool) -> Result<u32, String> {
    let (secure, host, port, path) = split(url).ok_or("Only https:// addresses are supported (http:// for localhost)")?;
    let failed = |step: &str| format!("{step} failed ({})", windows::core::Error::from_thread().message());
    unsafe {
        let session = Handle(WinHttpOpen(w!("Islet"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if session.0.is_null() {
            return Err(failed("Opening the network"));
        }
        // Resolve, connect, send: 15 s each; a streamed answer may pause up to 90 s.
        let _ = WinHttpSetTimeouts(session.0, 15_000, 15_000, 15_000, 90_000);
        let connection = Handle(WinHttpConnect(session.0, &HSTRING::from(host), port, 0));
        if connection.0.is_null() {
            return Err(failed("Connecting"));
        }
        let verb = if body.is_some() { w!("POST") } else { w!("GET") };
        let call = Handle(WinHttpOpenRequest(connection.0, verb, &HSTRING::from(path), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), if secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) }));
        if call.0.is_null() {
            return Err(failed("Preparing the request"));
        }
        let lines: Vec<u16> = headers.iter().flat_map(|(name, value)| format!("{name}: {value}\r\n").encode_utf16().collect::<Vec<_>>()).collect();
        let payload = body.unwrap_or(&[]);
        WinHttpSendRequest(call.0, (!lines.is_empty()).then_some(&lines[..]), Some(payload.as_ptr() as *const c_void), payload.len() as u32, payload.len() as u32, 0).map_err(|e| format!("Could not reach {host} ({})", e.message()))?;
        WinHttpReceiveResponse(call.0, std::ptr::null_mut()).map_err(|e| format!("No answer from {host} ({})", e.message()))?;
        let (mut status, mut size) = (0u32, 4u32);
        WinHttpQueryHeaders(call.0, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut status as *mut u32 as *mut c_void), &mut size, std::ptr::null_mut()).map_err(|_| failed("Reading the answer"))?;
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            let mut got = 0u32;
            WinHttpReadData(call.0, buffer.as_mut_ptr() as *mut c_void, buffer.len() as u32, &mut got).map_err(|e| format!("The connection broke ({})", e.message()))?;
            if got == 0 || !on_data(status, &buffer[..got as usize]) {
                break;
            }
        }
        Ok(status)
    }
}

/// GET returning the body as text (at most 4 MB); an HTTP error status is an `Err`.
pub fn get(url: &str) -> Result<String, String> {
    let mut body = Vec::new();
    let status = request(url, &[], None, &mut |_, chunk| {
        body.extend_from_slice(chunk);
        body.len() < 4 << 20
    })?;
    if status != 200 {
        return Err(format!("The server answered {status}"));
    }
    String::from_utf8(body).map_err(|_| "The answer was not text".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_escaping() {
        assert_eq!(split("https://api.open-meteo.com/v1/forecast?x=1"), Some((true, "api.open-meteo.com", 443, "/v1/forecast?x=1")));
        assert_eq!(split("https://localhost:8443"), Some((true, "localhost", 8443, "/")));
        // Unencrypted only to this PC (a local model server).
        assert_eq!(split("http://localhost:11434/v1/chat/completions"), Some((false, "localhost", 11434, "/v1/chat/completions")));
        assert_eq!(split("http://plain.example/"), None);
        assert_eq!(split("https:///nohost"), None);
        assert_eq!(escape("Šiauliai, LT"), "%C5%A0iauliai%2C%20LT");
    }

    /// One real request to a public API.
    #[test]
    #[ignore = "uses the network"]
    fn live_get() {
        let body = get("https://api.open-meteo.com/v1/forecast?latitude=52.52&longitude=13.41&current=temperature_2m").unwrap();
        println!("{} bytes: {}", body.len(), &body[..body.len().min(160)]);
        assert!(body.contains("temperature_2m"));
        assert!(get("https://api.open-meteo.com/v1/nothing-here").is_err());
    }
}
