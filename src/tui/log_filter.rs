// Parses cloudflared log lines into compact per-request rows for ngrok-dev mode.
//
// At DBG level cloudflared emits both request and response lines:
//   Request:  2026-08-01T22:42:33Z DBG GET https://host/path HTTP/1.1 connIndex=1 ... path=/live/longpoll
//   Response: 2026-08-01T22:42:33Z DBG 200 OK connIndex=1 content-length=246 ...
// At any level, failed requests emit:
//   2026-08-01T22:42:33Z ERR Request failed error="..." connIndex=0 dest=https://... type=http
//
// NgrokDevFilter is a stateful filter that pairs request+response by connIndex.

use std::collections::{HashMap, VecDeque};

fn extract_hhmmss(ts: &str) -> Option<String> {
    // ISO-8601 "2026-08-01T22:42:33Z" — extract 22:42:33.
    let t_pos = ts.find('T')?;
    let after_t = &ts[t_pos + 1..];
    let end = after_t.find(|c: char| !c.is_ascii_digit() && c != ':')?;
    let hhmmss = &after_t[..end];
    if hhmmss.len() != 8 {
        return None;
    }
    Some(hhmmss.to_string())
}

fn is_http_method(s: &str) -> bool {
    matches!(
        s,
        "GET" | "POST" | "PUT" | "DELETE" | "HEAD" | "OPTIONS" | "PATCH" | "CONNECT" | "TRACE"
    )
}

// Extract the value of `key=<value>` where value is either quoted ("...") or bare
// non-whitespace. Handles values containing '=' or JSON braces by grabbing
// balanced quoted strings.
fn extract_kv(s: &str, key: &str) -> Option<String> {
    // Search for " key=" or start-of-string "key=" pattern (word boundary).
    let needle = format!(" {}=", key);
    let start = if s.starts_with(&format!("{}=", key)) {
        key.len() + 1
    } else {
        s.find(&needle)? + needle.len()
    };
    let value_str = &s[start..];
    if value_str.starts_with('"') {
        // Quoted: scan for closing quote, honoring backslash escapes.
        let mut chars = value_str.char_indices();
        chars.next(); // skip opening "
        let mut escaped = false;
        for (i, c) in chars {
            if escaped {
                escaped = false;
                continue;
            }
            match c {
                '\\' => escaped = true,
                '"' => return Some(value_str[1..i].to_string()),
                _ => {}
            }
        }
        None
    } else {
        // Bare: read up to next whitespace.
        let end = value_str
            .find(char::is_whitespace)
            .unwrap_or(value_str.len());
        Some(value_str[..end].to_string())
    }
}

fn url_to_path(url: &str) -> Option<String> {
    // http[s]://host/path?query...
    let scheme_end = url.find("://")?;
    let rest = &url[scheme_end + 3..];
    let path_start = rest.find('/').unwrap_or(rest.len());
    Some(rest[path_start..].to_string())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

// Stateful filter that pairs cloudflared request/response events on connIndex.
// One instance per render pass (recreated fresh; not persisted between renders).
pub struct NgrokDevFilter {
    // FIFO queue of pending requests keyed by connIndex.
    pending: HashMap<u32, VecDeque<PendingRequest>>,
}

struct PendingRequest {
    time: String,
    method: String,
    path: String,
}

impl NgrokDevFilter {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
        }
    }

    // Process one raw cloudflared log line. Returns Some(formatted_line) if this
    // event completes a request/response pair (or is an error). Returns None for
    // request lines that stash themselves waiting for a response, and for
    // non-request/non-response noise.
    pub fn process_line(&mut self, line: &str) -> Option<String> {
        let (ts, rest) = line.split_once(' ')?;
        let time = extract_hhmmss(ts)?;
        let (level, rest) = rest.split_once(' ')?;
        match level {
            "DBG" => self.process_dbg(&time, rest),
            "ERR" if rest.starts_with("Request failed") => {
                Some(self.process_err_request_failed(&time, rest))
            }
            _ => None,
        }
    }

    // Emit any requests still waiting for a response (called after all lines
    // processed). Format: same as paired-line but with [pending] status.
    pub fn flush_pending(&mut self) -> Vec<String> {
        // Drop unmatched requests silently. Long-polling / WebSocket / SSE
        // requests never get a "response closed" line from cloudflared, and
        // showing them as [pending] just floods the panel with noise. Users
        // who want to see raw request lines can switch to Debug mode.
        self.pending.clear();
        Vec::new()
    }

    fn process_dbg(&mut self, time: &str, rest: &str) -> Option<String> {
        let (first, rest) = rest.split_once(' ')?;
        if is_http_method(first) {
            // Request line: METHOD URL HTTP/x.x key=value...
            let (_url, rest) = rest.split_once(' ')?;
            let (_httpver, kv_str) = rest.split_once(' ')?;
            let path = extract_kv(kv_str, "path").unwrap_or_else(|| "?".into());
            let conn_index = extract_kv(kv_str, "connIndex").and_then(|s| s.parse::<u32>().ok())?;
            self.pending
                .entry(conn_index)
                .or_default()
                .push_back(PendingRequest {
                    time: time.to_string(),
                    method: first.to_string(),
                    path,
                });
            None
        } else if is_status_code(first) {
            // Response line: STATUS TEXT... connIndex=... content-length=...
            let status = first.to_string();
            let conn_index = extract_kv(rest, "connIndex").and_then(|s| s.parse::<u32>().ok());
            let content_length =
                extract_kv(rest, "content-length").and_then(|s| s.parse::<u64>().ok());
            let paired =
                conn_index.and_then(|i| self.pending.get_mut(&i).and_then(|q| q.pop_front()));
            Some(match paired {
                Some(req) => {
                    format_pair(&req.time, &req.method, &req.path, &status, content_length)
                }
                None => format!(
                    "{}  ??????                                          {}  {}B  (orphan)",
                    time,
                    status,
                    content_length.map(|n| n.to_string()).unwrap_or_default()
                ),
            })
        } else {
            None
        }
    }

    fn process_err_request_failed(&mut self, time: &str, rest: &str) -> String {
        let error = extract_kv(rest, "error").unwrap_or_default();
        let dest = extract_kv(rest, "dest").unwrap_or_default();
        let path = url_to_path(&dest).unwrap_or_else(|| dest.clone());
        let conn_index = extract_kv(rest, "connIndex").and_then(|s| s.parse::<u32>().ok());
        // Consume any pending on this conn since it's the one that failed.
        if let Some(i) = conn_index {
            if let Some(q) = self.pending.get_mut(&i) {
                q.pop_front();
            }
        }
        format_error(time, &path, &error)
    }
}

fn is_status_code(s: &str) -> bool {
    s.len() == 3 && s.chars().all(|c| c.is_ascii_digit())
}

// Format one paired request/response line.
fn format_pair(time: &str, method: &str, path: &str, status: &str, bytes: Option<u64>) -> String {
    let bytes_str = bytes.map(format_bytes).unwrap_or_default();
    // Tighter widths so 5-digit byte counts don't wrap in a 60%-of-terminal pane.
    format!(
        "{}  {:<6} {:<40}  {:<3}  {:>7}",
        time,
        method,
        truncate(path, 40),
        status,
        bytes_str,
    )
}

fn format_error(time: &str, path: &str, error: &str) -> String {
    format!(
        "{}  {:<6} {:<40}  ERROR  {}",
        time,
        "???",
        truncate(path, 40),
        truncate(error, 30),
    )
}

// Compact human-readable byte count so 4-KB / 4-MB responses stay short.
fn format_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{}B", n)
    } else if n < 1024 * 1024 {
        format!("{:.1}K", n as f64 / 1024.0)
    } else {
        format!("{:.1}M", n as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_hhmmss_from_iso() {
        assert_eq!(extract_hhmmss("2026-08-01T22:42:33Z").unwrap(), "22:42:33");
        assert_eq!(extract_hhmmss("2026-08-01T00:00:00Z").unwrap(), "00:00:00");
        assert_eq!(extract_hhmmss("garbage"), None);
    }

    #[test]
    fn extract_kv_handles_quoted_and_bare() {
        let s = r#"foo=bar baz="quoted value" num=42"#;
        assert_eq!(extract_kv(s, "foo"), Some("bar".to_string()));
        assert_eq!(extract_kv(s, "baz"), Some("quoted value".to_string()));
        assert_eq!(extract_kv(s, "num"), Some("42".to_string()));
        assert_eq!(extract_kv(s, "missing"), None);
    }

    #[test]
    fn extract_kv_ignores_substring_matches() {
        // "xfoo=bar" should not match key "foo".
        let s = "xfoo=nope foo=yes";
        assert_eq!(extract_kv(s, "foo"), Some("yes".to_string()));
    }

    #[test]
    fn ignores_non_request_lines() {
        let mut f = NgrokDevFilter::new();
        for noise in [
            "2026-08-01T22:42:31Z INF Starting tunnel tunnelID=abc",
            "2026-08-01T22:42:31Z DBG edge discovery: looking up edge SRV record domain=x event=0",
            "2026-08-01T22:42:31Z DBG Tunnel connection options connIndex=0 event=0",
            "2026-08-01T22:42:31Z WRN some warning",
            "2026-08-01T22:42:31Z ERR failed to accept incoming stream requests connIndex=0",
            "garbage that doesn't match anything",
        ] {
            assert!(f.process_line(noise).is_none(), "should ignore: {noise}");
        }
        assert!(
            f.pending.is_empty(),
            "noise must not be stashed as a request"
        );
    }

    #[test]
    fn pairs_post_with_headers_blob() {
        // Real-world request line with a huge headers={...} blob before path=
        let mut f = NgrokDevFilter::new();
        let req = r#"2026-08-01T22:42:33Z DBG POST https://host/api HTTP/1.1 connIndex=2 content-length=1299 event=1 headers={"a":"b"} host=host ingressRule=0 originService=http://local path=/api"#;
        assert!(f.process_line(req).is_none());
        let out = f
            .process_line(
                r#"2026-08-01T22:42:33Z DBG 201 Created connIndex=2 content-length=5 event=1"#,
            )
            .unwrap();
        assert!(out.starts_with("22:42:33  POST"), "bad row: {out}");
        assert!(out.contains("/api"), "missing path: {out}");
        assert!(out.contains("201"), "missing status: {out}");
    }

    #[test]
    fn truncates_long_paths() {
        let mut f = NgrokDevFilter::new();
        let long = "/very/long/path/".to_string() + &"x".repeat(200);
        f.process_line(&format!(
            "2026-08-01T22:42:33Z DBG GET https://h{long} HTTP/1.1 connIndex=1 event=1 path={long}"
        ));
        let out = f
            .process_line(r#"2026-08-01T22:42:33Z DBG 200 OK connIndex=1 content-length=1 event=1"#)
            .unwrap();
        assert!(out.contains('…'), "path not truncated: {out}");
    }

    #[test]
    fn pairs_request_and_response_on_same_conn_index() {
        let mut f = NgrokDevFilter::new();
        // Request comes first — returns None (stashed).
        let req_line = r#"2026-08-01T23:18:38Z DBG GET https://host/live/longpoll HTTP/1.1 connIndex=3 content-length=0 event=1 path=/live/longpoll"#;
        assert!(f.process_line(req_line).is_none());
        // Response follows — returns paired line.
        let resp_line = r#"2026-08-01T23:18:38Z DBG 200 OK connIndex=3 content-length=246 event=1 ingressRule=0 originService=http://local"#;
        let out = f.process_line(resp_line).unwrap();
        assert!(out.contains("GET"), "missing method: {out}");
        assert!(out.contains("/live/longpoll"), "missing path: {out}");
        assert!(out.contains("200"), "missing status: {out}");
        assert!(out.contains("246B"), "missing bytes: {out}");
    }

    #[test]
    fn preserves_fifo_order_within_same_conn_index() {
        let mut f = NgrokDevFilter::new();
        // Two requests on same conn.
        f.process_line(
            r#"2026-08-01T23:18:38Z DBG GET https://h/a HTTP/1.1 connIndex=3 event=1 path=/a"#,
        );
        f.process_line(
            r#"2026-08-01T23:18:38Z DBG POST https://h/b HTTP/1.1 connIndex=3 event=1 path=/b"#,
        );
        // First response — should pair with FIRST request (GET /a).
        let out1 = f
            .process_line(r#"2026-08-01T23:18:38Z DBG 200 OK connIndex=3 content-length=1 event=1"#)
            .unwrap();
        assert!(out1.contains("GET"), "should pair with GET: {out1}");
        assert!(out1.contains("/a"), "should have first path: {out1}");
        // Second response — should pair with SECOND request (POST /b).
        let out2 = f
            .process_line(r#"2026-08-01T23:18:38Z DBG 200 OK connIndex=3 content-length=2 event=1"#)
            .unwrap();
        assert!(out2.contains("POST"), "should pair with POST: {out2}");
        assert!(out2.contains("/b"), "should have second path: {out2}");
    }

    #[test]
    fn flush_pending_drops_stashed_requests_silently() {
        // Unmatched requests (e.g. long-polling / WebSocket that never gets a
        // logged response) are silently dropped rather than displayed as
        // [pending] — that would flood the panel with noise.
        let mut f = NgrokDevFilter::new();
        f.process_line(
            r#"2026-08-01T23:18:38Z DBG GET https://h/a HTTP/1.1 connIndex=3 event=1 path=/a"#,
        );
        assert!(f.flush_pending().is_empty());
    }

    #[test]
    fn error_request_failed_emits_error_line_and_pops_pending() {
        let mut f = NgrokDevFilter::new();
        // Stash a pending request on conn=0.
        f.process_line(
            r#"2026-08-01T23:18:38Z DBG GET https://h/a HTTP/1.1 connIndex=0 event=1 path=/a"#,
        );
        let err_line = r#"2026-08-01T23:18:38Z ERR Request failed error="context canceled" connIndex=0 dest=https://h/a type=http"#;
        let out = f.process_line(err_line).unwrap();
        assert!(out.contains("ERROR"), "missing ERROR: {out}");
        assert!(out.contains("context canceled"));
        // Pending should now be empty for that conn.
        assert!(f.flush_pending().is_empty());
    }

    #[test]
    fn response_without_matching_request_marked_orphan() {
        let mut f = NgrokDevFilter::new();
        let out = f
            .process_line(
                r#"2026-08-01T23:18:38Z DBG 200 OK connIndex=99 content-length=10 event=1"#,
            )
            .unwrap();
        assert!(out.contains("orphan"), "expected orphan marker: {out}");
    }
}
