//! Process-wide network options: an HTTP/SOCKS proxy and DNS-over-HTTPS. Every HTTP client the
//! app builds comes from [`builder`], so one call to [`configure`] at startup covers YouTube,
//! thumbnails, SponsorBlock and the lounge. (The TV's media pipeline fetches video segments
//! itself and uses neither; a proxy for video needs the local segment relay.)

use anyhow::{bail, Result};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetConfig {
    /// `http://host:port`, `https://...`, `socks5://host:port` or `socks5h://...`; empty = direct.
    pub proxy: String,
    /// DoH provider: "" / "off", "cloudflare", "google", "quad9", or an `https://IP/dns-query` URL.
    pub doh: String,
}

static CONFIG: RwLock<Option<NetConfig>> = RwLock::new(None);

/// Validates and installs `cfg` for clients built from now on.
pub fn configure(cfg: NetConfig) -> Result<()> {
    if !cfg.proxy.is_empty() {
        reqwest::Proxy::all(&cfg.proxy)?;
    }
    if !cfg.doh.is_empty() && cfg.doh != "off" {
        doh_endpoint(&cfg.doh)?;
    }
    *CONFIG.write().unwrap() = Some(cfg);
    Ok(())
}

/// A blocking client builder with the configured proxy / DNS. gzip is on.
pub fn builder() -> reqwest::blocking::ClientBuilder {
    let mut b = reqwest::blocking::Client::builder().gzip(true);
    let cfg = CONFIG.read().unwrap().clone().unwrap_or_default();
    if !cfg.proxy.is_empty() {
        if let Ok(p) = reqwest::Proxy::all(&cfg.proxy) {
            b = b.proxy(p);
        }
    }
    if let Ok(url) = doh_endpoint(&cfg.doh) {
        // A proxy that resolves names itself (socks5h, HTTP CONNECT) makes local DNS moot, but
        // the resolver is harmless there.
        b = b.dns_resolver(Arc::new(Doh::new(url)));
    }
    b
}

/// Shorthand: [`builder`] with a timeout, built.
pub fn client(timeout: Duration) -> reqwest::blocking::Client {
    builder().timeout(timeout).build().unwrap_or_default()
}

fn doh_endpoint(name: &str) -> Result<String> {
    Ok(match name {
        "" | "off" => bail!("DoH off"),
        "cloudflare" => "https://1.1.1.1/dns-query".into(),
        "google" => "https://8.8.8.8/resolve".into(),
        "quad9" => "https://9.9.9.9:5053/dns-query".into(),
        u if u.starts_with("https://") => {
            // The endpoint is reached without DNS, so its host must be an IP literal.
            let rest = u.trim_start_matches("https://");
            let host = match rest.strip_prefix('[') {
                Some(v6) => v6.split(']').next().unwrap_or(""),
                None => rest.split(['/', ':']).next().unwrap_or(""),
            };
            if host.parse::<IpAddr>().is_err() {
                bail!("DoH URL must use an IP address, not {host}");
            }
            u.into()
        }
        other => bail!("unknown DoH provider {other}"),
    })
}

/// DNS-over-HTTPS (JSON flavour, `application/dns-json`) with a small TTL cache. Falls back to
/// the system resolver when the DoH server is unreachable, so a bad setting never cuts the app off.
struct Doh {
    url: String,
    http: reqwest::Client,
    cache: Arc<Mutex<HashMap<String, (Vec<IpAddr>, Instant)>>>,
}

impl Doh {
    fn new(url: String) -> Doh {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(5)).no_proxy().build().unwrap_or_default();
        Doh { url, http, cache: Arc::default() }
    }
}

impl Resolve for Doh {
    fn resolve(&self, name: Name) -> Resolving {
        let (url, http, cache) = (self.url.clone(), self.http.clone(), self.cache.clone());
        let host = name.as_str().to_string();
        Box::pin(async move {
            if let Some((ips, until)) = cache.lock().unwrap().get(&host) {
                if Instant::now() < *until {
                    return Ok(addrs(ips.clone()));
                }
            }
            let mut ips = Vec::new();
            let mut ttl = 300u64;
            for ty in ["A", "AAAA"] {
                if let Ok((found, t)) = query(&http, &url, &host, ty).await {
                    ttl = ttl.min(t.max(30));
                    ips.extend(found);
                }
                if !ips.is_empty() {
                    break; // IPv4 is enough; AAAA only when there is no A record.
                }
            }
            if ips.is_empty() {
                // System resolver fallback.
                ips = (host.as_str(), 0).to_socket_addrs()?.map(|a| a.ip()).collect();
            } else {
                cache.lock().unwrap().insert(host, (ips.clone(), Instant::now() + Duration::from_secs(ttl)));
            }
            Ok(addrs(ips))
        })
    }
}

fn addrs(ips: Vec<IpAddr>) -> Addrs {
    Box::new(ips.into_iter().map(|ip| SocketAddr::new(ip, 0)))
}

async fn query(http: &reqwest::Client, url: &str, host: &str, ty: &str) -> Result<(Vec<IpAddr>, u64)> {
    let v: serde_json::Value = http
        .get(url)
        .query(&[("name", host), ("type", ty)])
        .header("accept", "application/dns-json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(parse_answer(&v))
}

/// Addresses and the smallest TTL from a dns-json answer (CNAME chains are skipped).
fn parse_answer(v: &serde_json::Value) -> (Vec<IpAddr>, u64) {
    let mut ttl = u64::MAX;
    let ips = v["Answer"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| matches!(a["type"].as_u64(), Some(1) | Some(28)))
        .filter_map(|a| {
            ttl = ttl.min(a["TTL"].as_u64().unwrap_or(300));
            a["data"].as_str()?.parse().ok()
        })
        .collect();
    (ips, if ttl == u64::MAX { 300 } else { ttl })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dns_json() {
        let v = serde_json::json!({"Status":0,"Answer":[
            {"name":"www.youtube.com","type":5,"TTL":100,"data":"youtube-ui.l.google.com."},
            {"name":"youtube-ui.l.google.com","type":1,"TTL":120,"data":"142.250.1.1"},
            {"name":"youtube-ui.l.google.com","type":1,"TTL":60,"data":"142.250.1.2"}]});
        let (ips, ttl) = parse_answer(&v);
        assert_eq!(ips.len(), 2);
        assert_eq!(ttl, 60);
    }

    #[test]
    fn endpoints() {
        assert!(doh_endpoint("off").is_err());
        assert_eq!(doh_endpoint("cloudflare").unwrap(), "https://1.1.1.1/dns-query");
        assert!(doh_endpoint("https://dns.example/dns-query").is_err());
        assert!(doh_endpoint("https://[2606:4700::1111]/dns-query").is_ok());
        assert!(configure(NetConfig { proxy: "socks5h://127.0.0.1:1080".into(), doh: "off".into() }).is_ok());
        assert!(configure(NetConfig { proxy: "not a url".into(), doh: String::new() }).is_err());
    }

    #[test]
    #[ignore = "network"]
    fn resolves_via_cloudflare() {
        configure(NetConfig { proxy: String::new(), doh: "cloudflare".into() }).unwrap();
        let r = client(Duration::from_secs(10)).get("https://www.youtube.com/robots.txt").send().unwrap();
        assert!(r.status().is_success());
    }
}
