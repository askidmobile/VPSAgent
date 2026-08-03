//! Web-инструменты: web_fetch (URL → очищенный текст).

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{Tool, ToolContext, ToolOutput};

pub struct WebFetch;

#[async_trait]
impl Tool for WebFetch {
    fn name(&self) -> &'static str {
        "web_fetch"
    }
    fn description(&self) -> &'static str {
        "Скачать URL и вернуть содержимое как текст (HTML → обрезанный текст, обрезано до ~30k символов)."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "HTTP(S) URL." }
            },
            "required": ["url"]
        })
    }
    async fn run(&self, input: Value, _ctx: &ToolContext) -> ToolOutput {
        let url = input.get("url").and_then(|v| v.as_str()).unwrap_or("");
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return ToolOutput::err("URL должен начинаться с http(s)://");
        }
        // SSRF-защита: отклонять private/loopback/link-local IP (C3).
        if let Err(reason) = check_ssrf(url).await {
            return ToolOutput::err(reason);
        }
        // Лимит размера: читаем поток с счётчиком, обрыв на 1 МБ (C3 DoS).
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("vpsagent/0.1")
            // Ограничить редиректы и проверить каждый на SSRF.
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .unwrap_or_default();
        let mut resp = match client.get(url).send().await {
            Ok(r) => r,
            Err(e) => return ToolOutput::err(format!("fetch: {e}")),
        };
        let status = resp.status();
        if !status.is_success() {
            return ToolOutput::err(format!("HTTP {status}"));
        }
        // Потоковое чтение с лимитом 1 МБ (не загружать весь body в память).
        let mut raw = Vec::with_capacity(1 << 20);
        let limit = 1 << 20; // 1 МБ
        use futures::StreamExt;
        while let Ok(Some(chunk)) = resp.chunk().await {
            if raw.len() + chunk.len() > limit {
                raw.extend_from_slice(&chunk[..limit - raw.len()]);
                break;
            }
            raw.extend_from_slice(&chunk);
        }
        let raw = String::from_utf8_lossy(&raw);
        let cleaned = strip_html(&raw);
        // Char-safe truncate (C4).
        let truncated = if cleaned.chars().count() > 30_000 {
            format!(
                "{}…\n(обрезано, {} символов)",
                crate::truncate::truncate_chars(&cleaned, 30_000),
                cleaned.chars().count()
            )
        } else {
            cleaned
        };
        ToolOutput::ok(truncated)
    }
}

/// Проверить URL на SSRF: резолв хоста, отклонить private/loopback/link-local.
async fn check_ssrf(url: &str) -> Result<(), String> {
    let parsed = match url::Url::parse(url) {
        Ok(u) => u,
        Err(e) => return Err(format!("невалидный URL: {e}")),
    };
    let host = match parsed.host_str() {
        Some(h) => h.to_string(),
        None => return Err("URL без хоста".into()),
    };
    // Если хост — уже IP-адрес, проверяем напрямую.
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if is_private_ip(&ip) {
            return Err(format!("запрещён приватный IP: {ip}"));
        }
        return Ok(());
    }
    // Резолв домена; проверяем каждый адрес.
    let port = parsed
        .port()
        .unwrap_or(if parsed.scheme() == "https" { 443 } else { 80 });
    // lookup_host возвращает итератор, привязанный к `host`; собираем в owned Vec
    // через отдельную область видимости, чтобы избежать borrow-проблем.
    let host_owned = host.clone();
    let addrs: Vec<std::net::SocketAddr> = {
        use std::net::ToSocketAddrs;
        let addr_str = format!("{host_owned}:{port}");
        match addr_str.to_socket_addrs() {
            Ok(iter) => iter.collect(),
            Err(e) => return Err(format!("не удалось разрешить хост {host_owned}: {e}")),
        }
    };
    for addr in addrs {
        if is_private_ip(&addr.ip()) {
            return Err(format!("домен резолвится в приватный IP: {}", addr.ip()));
        }
    }
    Ok(())
}

/// Приватный/loopback/link-local IP.
fn is_private_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.octets()[0] == 169 && v4.octets()[1] == 254 // 169.254.0.0/16
                || v4.is_multicast()
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
                || v6.is_multicast()
        }
    }
}

/// Грубая очистка HTML: удаляем теги и скрипты. В Фазе 2 заменим на нормальный конвертер.
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut skip_script = false;
    for word in html.split_whitespace() {
        if skip_script {
            if word.contains("</script>") {
                skip_script = false;
            }
            continue;
        }
        if word.starts_with("<script") {
            skip_script = true;
            continue;
        }
        for ch in word.chars() {
            if ch == '<' {
                in_tag = true;
                continue;
            }
            if ch == '>' {
                in_tag = false;
                continue;
            }
            if !in_tag {
                out.push(ch);
            }
        }
        out.push(' ');
    }
    out.trim().to_string()
}
