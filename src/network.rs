//! Shared connection policy. Approval applies to one concrete socket address;
//! hostnames are resolved once, so DNS cannot change the approved destination.

use std::net::{IpAddr, SocketAddr};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::oneshot;

pub struct NetworkQuestion {
    pub host: String,
    pub port: u16,
    /// None asks for permission to resolve a hostname, before any DNS request.
    pub address: Option<SocketAddr>,
    pub reply: oneshot::Sender<bool>,
}

impl NetworkQuestion {
    pub fn text(&self) -> String {
        match self.address {
            Some(address) => format!(
                "Offline mode is enabled. {address} is outside private/local IP ranges. Connect to {} once? Reconnecting or duplicating the session will ask again.",
                self.host
            ),
            None => format!(
                "Offline mode is enabled. Resolving {} may contact your system's DNS servers. Allow this lookup once? Any resulting non-local connection will need separate approval. Use a private IP address to avoid DNS.",
                self.host
            ),
        }
    }
}

pub type NetworkAsker = Arc<dyn Fn(NetworkQuestion) + Send + Sync>;

#[derive(Clone)]
pub struct NetworkAccess {
    offline: Arc<AtomicBool>,
    ask: NetworkAsker,
}

impl Default for NetworkAccess {
    fn default() -> Self {
        Self::new(false, Arc::new(|_| {}))
    }
}

impl NetworkAccess {
    pub fn new(offline: bool, ask: NetworkAsker) -> Self {
        Self { offline: Arc::new(AtomicBool::new(offline)), ask }
    }
    pub fn set_offline(&self, offline: bool) {
        self.offline.store(offline, Ordering::SeqCst);
    }
    pub fn with_asker(&self, ask: NetworkAsker) -> Self {
        Self { offline: self.offline.clone(), ask }
    }
    pub fn offline(&self) -> bool {
        self.offline.load(Ordering::SeqCst)
    }
    async fn approve(&self, host: &str, port: u16, address: Option<SocketAddr>) -> Result<(), String> {
        if !self.offline() {
            return Ok(());
        }
        let (reply, answer) = oneshot::channel();
        (self.ask)(NetworkQuestion { host: host.into(), port, address, reply });
        if answer.await.unwrap_or(false) { Ok(()) } else { Err("Connection canceled in offline mode.".into()) }
    }
    pub async fn approve_lookup(&self, host: &str, port: u16) -> Result<(), String> {
        self.approve(host, port, None).await
    }
    pub async fn approve_address(&self, host: &str, address: SocketAddr) -> Result<(), String> {
        if is_local(address.ip()) {
            return Ok(());
        }
        self.approve(host, address.port(), Some(address)).await
    }
}

/// Private, loopback and link-local addresses only. Other special-use ranges
/// also ask, rather than claiming that an address is reachable only locally.
pub fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or_else(
            || ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
            |v4| is_local(IpAddr::V4(v4)),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classifies_local_ranges_without_mapped_ipv6_bypasses() {
        for local in [
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.254",
            "192.168.1.1",
            "127.2.3.4",
            "169.254.1.2",
            "::1",
            "fc00::1",
            "fdff::1",
            "fe80::1",
            "::ffff:192.168.1.1",
        ] {
            assert!(is_local(local.parse().unwrap()), "{local}");
        }
        for remote in [
            "8.8.8.8",
            "172.15.0.1",
            "172.32.0.1",
            "192.169.1.1",
            "100.64.0.1",
            "192.0.2.1",
            "0.0.0.0",
            "224.0.0.1",
            "2001:4860::1",
            "2001:db8::1",
            "::",
            "::ffff:8.8.8.8",
            "64:ff9b::a00:1",
        ] {
            assert!(!is_local(remote.parse().unwrap()), "{remote}");
        }
    }

    #[test]
    fn approval_is_required_each_time_and_denial_precedes_network_io() {
        use std::sync::atomic::AtomicUsize;
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let access = NetworkAccess::new(
            true,
            Arc::new(move |q| {
                seen.fetch_add(1, Ordering::SeqCst);
                assert!(q.address.is_none() || q.address.unwrap().ip() == "192.0.2.1".parse::<IpAddr>().unwrap());
                let _ = q.reply.send(false);
            }),
        );
        runtime.block_on(async {
            for _ in 0..2 {
                assert!(
                    crate::transport::connect_tcp("192.0.2.1", 22, &access).await.unwrap_err().contains("canceled")
                );
            }
            assert!(
                crate::transport::connect_tcp("should-never-resolve.invalid", 22, &access)
                    .await
                    .unwrap_err()
                    .contains("canceled")
            );
            assert_eq!(calls.load(Ordering::SeqCst), 3);
            access.approve_address("local", "127.0.0.1:22".parse().unwrap()).await.unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 3);
            let accepting = NetworkAccess::new(
                true,
                Arc::new(|q| {
                    let _ = q.reply.send(true);
                }),
            );
            accepting.approve_address("example", "192.0.2.1:22".parse().unwrap()).await.unwrap();
            // This only approves an address; the test never connects externally.
        });
    }
}
