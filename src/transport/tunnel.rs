//! SSH port forwarding for a shell tab: local (`-L`), remote (`-R`) and
//! dynamic SOCKS5 (`-D`) rules from the profile. They start once the shell is
//! open and stop with it. A rule that fails is reported by name and leaves
//! the shell alone.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use russh::Channel;
use russh::client::Msg;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

use super::Sink;
use super::ssh::Session;
use crate::network::NetworkAccess;
use crate::profiles::{Tunnel, TunnelKind};

/// What one rule is doing, for the Tunnels panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelStatus {
    pub label: String,
    pub state: TunnelState,
    /// Connections open through it right now.
    pub connections: usize,
    /// Why the last connection through it failed, if one did.
    pub last_error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelState {
    Starting,
    Listening,
    Failed(String),
}

/// Shared rule states; every change goes to the sink as a fresh list.
struct Board {
    rows: Mutex<Vec<TunnelStatus>>,
    sink: Arc<dyn Sink>,
}

impl Board {
    fn update(&self, index: usize, change: impl FnOnce(&mut TunnelStatus)) {
        let rows = {
            let mut rows = self.rows.lock();
            change(&mut rows[index]);
            rows.clone()
        };
        self.sink.tunnels(rows);
    }

    fn fail(&self, index: usize, reason: String) {
        let label = self.rows.lock()[index].label.clone();
        self.sink.notice(format!("Tunnel {label}: {reason}"));
        self.update(index, |row| row.state = TunnelState::Failed(reason));
    }

    /// Count a connection until the returned guard drops.
    fn open(self: &Arc<Self>, index: usize) -> Open {
        self.update(index, |row| row.connections += 1);
        Open(self.clone(), index)
    }
}

struct Open(Arc<Board>, usize);

impl Drop for Open {
    fn drop(&mut self) {
        self.0.update(self.1, |row| row.connections = row.connections.saturating_sub(1));
    }
}

/// Where a remote forward's connections go on this computer.
#[derive(Clone)]
pub(super) struct RemoteTarget {
    server_port: u32,
    bind: &'static str,
    host: String,
    port: u16,
    access: NetworkAccess,
    board: Arc<Board>,
    index: usize,
}

/// The remote forwards a connection accepts channels for. Empty = reject.
pub(super) type RemoteForwards = Arc<Mutex<Vec<RemoteTarget>>>;

/// The remote forward for a server-opened forwarded-tcpip channel.
pub(super) fn remote_target(forwards: &RemoteForwards, server_port: u32) -> Option<RemoteTarget> {
    forwards.lock().iter().find(|t| t.server_port == server_port).cloned()
}

/// Connect a forwarded-tcpip channel to its local destination. The connect is
/// Snekkie's own, so it goes through offline mode.
pub(super) async fn deliver(channel: Channel<Msg>, target: RemoteTarget) {
    let _open = target.board.open(target.index);
    match super::connect_tcp(&target.host, target.port, &target.access).await {
        Ok(mut socket) => {
            let _ = tokio::io::copy_bidirectional(&mut channel.into_stream(), &mut socket).await;
        }
        Err(e) => {
            target.board.update(target.index, |row| row.last_error = e);
            let _ = channel.close().await;
        }
    }
}

/// Running rules. Dropping this stops them.
pub(super) struct Running {
    tasks: Vec<JoinHandle<()>>,
    remote: RemoteForwards,
    handle: Arc<Session>,
}

impl Drop for Running {
    fn drop(&mut self) {
        let forwards = std::mem::take(&mut *self.remote.lock());
        for task in &self.tasks {
            task.abort();
        }
        // An explorer sharing the login keeps it open, so free the server's ports
        // or a reconnect could not forward them again.
        if !forwards.is_empty()
            && !self.handle.is_closed()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let handle = self.handle.clone();
            runtime.spawn(async move {
                for target in forwards {
                    let _ = handle.cancel_tcpip_forward(target.bind, target.server_port).await;
                }
            });
        }
    }
}

fn bind_address(tunnel: &Tunnel) -> &'static str {
    if tunnel.allow_others { "0.0.0.0" } else { "127.0.0.1" }
}

/// Start every enabled rule.
pub(super) fn start(
    handle: &Arc<Session>,
    rules: &[Tunnel],
    remote: &RemoteForwards,
    access: &NetworkAccess,
    sink: Arc<dyn Sink>,
) -> Running {
    let rules: Vec<Tunnel> = rules.iter().filter(|t| t.enabled).cloned().collect();
    let rows = rules
        .iter()
        .map(|t| TunnelStatus {
            label: t.label(),
            state: TunnelState::Starting,
            connections: 0,
            last_error: String::new(),
        })
        .collect();
    let board = Arc::new(Board { rows: Mutex::new(rows), sink });
    if !board.rows.lock().is_empty() {
        let rows = board.rows.lock().clone();
        board.sink.tunnels(rows);
    }
    let mut tasks = Vec::new();
    for (index, tunnel) in rules.into_iter().enumerate() {
        if let Some(problem) = tunnel.problem() {
            board.fail(index, problem.into());
            continue;
        }
        let (handle, board) = (handle.clone(), board.clone());
        let task = match tunnel.kind {
            TunnelKind::Local | TunnelKind::Dynamic => tokio::spawn(listen(handle, tunnel, board, index)),
            TunnelKind::Remote => {
                tokio::spawn(forward_remote(handle, tunnel, board, index, remote.clone(), access.clone()))
            }
        };
        tasks.push(task);
    }
    Running { tasks, remote: remote.clone(), handle: handle.clone() }
}

async fn forward_remote(
    handle: Arc<Session>,
    tunnel: Tunnel,
    board: Arc<Board>,
    index: usize,
    remote: RemoteForwards,
    access: NetworkAccess,
) {
    let server_port = u32::from(tunnel.listen_port);
    // Registered first, so a connection that arrives with the reply isn't refused.
    remote.lock().push(RemoteTarget {
        server_port,
        bind: bind_address(&tunnel),
        host: tunnel.host.trim().into(),
        port: tunnel.port,
        access,
        board: board.clone(),
        index,
    });
    match handle.tcpip_forward(bind_address(&tunnel), server_port).await {
        Ok(_) => board.update(index, |row| row.state = TunnelState::Listening),
        Err(e) => {
            remote.lock().retain(|t| !(t.server_port == server_port && t.index == index));
            board.fail(index, format!("the server refused to forward its port {server_port}: {e}"));
        }
    }
}

async fn listen(handle: Arc<Session>, tunnel: Tunnel, board: Arc<Board>, index: usize) {
    let bind = bind_address(&tunnel);
    let listener = match TcpListener::bind((bind, tunnel.listen_port)).await {
        Ok(listener) => listener,
        Err(e) => return board.fail(index, format!("could not listen on {bind}:{}: {e}", tunnel.listen_port)),
    };
    board.update(index, |row| row.state = TunnelState::Listening);
    // Aborting this task drops the set, which ends its connections too.
    let mut connections = JoinSet::new();
    loop {
        let (socket, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(_) => {
                // Out of file handles and the like: don't spin.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        while connections.try_join_next().is_some() {}
        let _ = socket.set_nodelay(true);
        let (handle, board, tunnel) = (handle.clone(), board.clone(), tunnel.clone());
        connections.spawn(async move {
            let _open = board.open(index);
            let result = match tunnel.kind {
                TunnelKind::Dynamic => socks(socket, &handle, peer).await,
                _ => forward_local(socket, &handle, tunnel.host.trim(), tunnel.port, peer).await,
            };
            if let Err(e) = result {
                board.update(index, |row| row.last_error = e);
            }
        });
    }
}

async fn open_channel(
    handle: &Session,
    host: &str,
    port: u16,
    peer: std::net::SocketAddr,
) -> Result<Channel<Msg>, String> {
    handle
        .channel_open_direct_tcpip(host, port.into(), peer.ip().to_string(), peer.port().into())
        .await
        .map_err(|e| format!("the server could not open {host}:{port}: {e}"))
}

async fn forward_local(
    mut socket: TcpStream,
    handle: &Session,
    host: &str,
    port: u16,
    peer: std::net::SocketAddr,
) -> Result<(), String> {
    let channel = open_channel(handle, host, port, peer).await?;
    let _ = tokio::io::copy_bidirectional(&mut socket, &mut channel.into_stream()).await;
    Ok(())
}

/// A minimal SOCKS5 server (RFC 1928): no authentication, CONNECT only.
async fn socks(mut socket: TcpStream, handle: &Session, peer: std::net::SocketAddr) -> Result<(), String> {
    let request = tokio::time::timeout(Duration::from_secs(10), socks_request(&mut socket)).await;
    let (host, port) = match request {
        Ok(Ok(target)) => target,
        failed => {
            // Unread request bytes would turn the close into a reset, which can lose the reply.
            let _ = socket.shutdown().await;
            let _ =
                tokio::time::timeout(Duration::from_millis(200), tokio::io::copy(&mut socket, &mut tokio::io::sink()))
                    .await;
            return Err(match failed {
                Ok(Err(e)) => format!("SOCKS request failed: {e}"),
                _ => "SOCKS handshake timed out".into(),
            });
        }
    };
    let channel = match open_channel(handle, &host, port, peer).await {
        Ok(channel) => channel,
        Err(e) => {
            // 5 = connection refused.
            let _ = socket.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).await;
            return Err(e);
        }
    };
    // The bound address is the server's business; report 0.0.0.0:0 as OpenSSH does.
    socket.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.map_err(|e| e.to_string())?;
    let _ = tokio::io::copy_bidirectional(&mut socket, &mut channel.into_stream()).await;
    Ok(())
}

/// Read the greeting and CONNECT request, answering the greeting.
async fn socks_request(socket: &mut TcpStream) -> std::io::Result<(String, u16)> {
    use std::io::{Error, ErrorKind};
    let refuse = |what: &str| Error::new(ErrorKind::InvalidData, what.to_string());
    let mut head = [0u8; 2];
    socket.read_exact(&mut head).await?;
    if head[0] != 5 {
        return Err(refuse("not SOCKS5"));
    }
    let mut methods = vec![0u8; head[1].into()];
    socket.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        socket.write_all(&[5, 0xff]).await?;
        return Err(refuse("the client wants authentication"));
    }
    socket.write_all(&[5, 0]).await?;
    let mut request = [0u8; 4];
    socket.read_exact(&mut request).await?;
    if request[0] != 5 || request[1] != 1 {
        // 7 = command not supported.
        socket.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        return Err(refuse("only CONNECT is supported"));
    }
    let host = match request[3] {
        1 => {
            let mut ip = [0u8; 4];
            socket.read_exact(&mut ip).await?;
            Ipv4Addr::from(ip).to_string()
        }
        4 => {
            let mut ip = [0u8; 16];
            socket.read_exact(&mut ip).await?;
            Ipv6Addr::from(ip).to_string()
        }
        3 => {
            let mut length = [0u8; 1];
            socket.read_exact(&mut length).await?;
            let mut name = vec![0u8; length[0].into()];
            socket.read_exact(&mut name).await?;
            match String::from_utf8(name) {
                Ok(name) => name,
                Err(_) => {
                    // 1 = general failure.
                    socket.write_all(&[5, 1, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
                    return Err(refuse("bad host name"));
                }
            }
        }
        _ => {
            // 8 = address type not supported.
            socket.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            return Err(refuse("unknown address type"));
        }
    };
    let mut port = [0u8; 2];
    socket.read_exact(&mut port).await?;
    Ok((host, u16::from_be_bytes(port)))
}
