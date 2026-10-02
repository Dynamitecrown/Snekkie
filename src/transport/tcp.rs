//! Telnet and raw TCP: a plain socket, with the telnet protocol layered on
//! top for telnet sessions. Raw is for console servers that hand you a
//! device's console line on a TCP port and expect bytes passed through
//! untouched.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{Instant, Interval, MissedTickBehavior};

use super::telnet::{self, Telnet};
use super::{Command, Link, Sink, connect_tcp, keepalive_interval};
use crate::profiles::{Kind, Profile};

pub fn description(profile: &Profile) -> String {
    let kind = profile.kind();
    let port = if kind.default_port() == Some(profile.port) { String::new() } else { format!(":{}", profile.port) };
    format!("{}  {}{port}", kind.label(), profile.host)
}

/// Start connecting in the background. Everything after this is reported
/// through the sink.
pub fn start(runtime: &tokio::runtime::Handle, profile: Profile, size: (u16, u16), sink: Arc<dyn Sink>) -> Link {
    start_with_access(runtime, profile, size, sink, crate::network::NetworkAccess::default())
}

pub fn start_with_access(
    runtime: &tokio::runtime::Handle,
    profile: Profile,
    size: (u16, u16),
    sink: Arc<dyn Sink>,
    access: crate::network::NetworkAccess,
) -> Link {
    let (link, commands) = Link::new();
    runtime.spawn(async move {
        let reason = run(profile, size, sink.clone(), commands, access).await;
        sink.closed(reason);
    });
    link
}

/// Let TCP itself keep the connection alive: raw sessions have no way to
/// send one in-band without it reaching the device.
fn set_tcp_keepalive(stream: &TcpStream, every: Duration) -> std::io::Result<()> {
    let keepalive = socket2::TcpKeepalive::new().with_time(every).with_interval(every);
    socket2::SockRef::from(stream).set_tcp_keepalive(&keepalive)
}

async fn tick(interval: &mut Option<Interval>) {
    match interval {
        Some(interval) => {
            interval.tick().await;
        }
        None => std::future::pending().await,
    }
}

async fn run(
    profile: Profile,
    mut size: (u16, u16),
    sink: Arc<dyn Sink>,
    mut commands: UnboundedReceiver<Command>,
    access: crate::network::NetworkAccess,
) -> Option<String> {
    let host = profile.host.trim().to_string();
    // Keep an eye on the command queue while connecting, so closing the tab
    // cancels a slow connect and a resize during it isn't lost.
    let stream = {
        let connecting = connect_tcp(&host, profile.port, &access);
        tokio::pin!(connecting);
        loop {
            tokio::select! {
                result = &mut connecting => match result {
                    Ok(stream) => break stream,
                    Err(message) => return Some(message),
                },
                command = commands.recv() => match command {
                    Some(Command::Resize { columns, lines }) => size = (columns, lines),
                    Some(Command::Close) | None => return None,
                    Some(_) => {}
                },
            }
        }
    };

    let every = keepalive_interval(profile.keepalive);
    let mut telnet = (profile.kind() == Kind::Telnet).then(|| Telnet::new(size.0, size.1));
    if telnet.is_none()
        && let Some(every) = every
        && let Err(e) = set_tcp_keepalive(&stream, every)
    {
        sink.notice(format!("Could not turn on keepalives: {e}"));
    }
    // Telnet sends a no-op instead, which also tells us sooner if the far
    // end has gone.
    let mut keepalive = every.filter(|_| telnet.is_some()).map(|every| {
        let mut interval = tokio::time::interval_at(Instant::now() + every, every);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        interval
    });

    let (mut reader, mut writer) = stream.into_split();
    if let Some(telnet) = telnet.as_mut()
        && let Err(e) = writer.write_all(&telnet.start()).await
    {
        return Some(format!("Write failed: {e}"));
    }
    sink.connected();

    let mut remote_echo = false;
    let mut buf = vec![0u8; 8192];
    loop {
        let outgoing = tokio::select! {
            read = reader.read(&mut buf) => match read {
                Ok(0) => return Some("Connection closed by remote host".into()),
                Ok(n) => match telnet.as_mut() {
                    Some(telnet) => {
                        let received = telnet.receive(&buf[..n]);
                        if telnet.remote_echo() != remote_echo {
                            remote_echo = telnet.remote_echo();
                            sink.remote_echo(remote_echo);
                        }
                        if !received.data.is_empty() {
                            sink.data(&received.data);
                        }
                        received.reply
                    }
                    None => {
                        sink.data(&buf[..n]);
                        Vec::new()
                    }
                },
                Err(e) => return Some(format!("Connection lost: {e}")),
            },
            command = commands.recv() => match command {
                Some(Command::Write(bytes)) => match &telnet {
                    Some(telnet) => telnet.encode(&bytes),
                    None => bytes,
                },
                Some(Command::Resize { columns, lines }) => {
                    telnet.as_mut().map(|t| t.resize(columns, lines)).unwrap_or_default()
                }
                Some(Command::Break) if telnet.is_some() => {
                    if let Err(e) = writer.write_all(&telnet::BREAK).await {
                        return Some(format!("Write failed: {e}"));
                    }
                    sink.notice("Break sent".into());
                    Vec::new()
                }
                Some(Command::Break) => {
                    sink.notice("Raw TCP sessions do not support break".into());
                    Vec::new()
                }
                Some(Command::Close) | None => {
                    let _ = writer.shutdown().await;
                    return None;
                }
            },
            () = tick(&mut keepalive) => telnet::KEEPALIVE.to_vec(),
        };
        if !outgoing.is_empty()
            && let Err(e) = writer.write_all(&outgoing).await
        {
            return Some(format!("Write failed: {e}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn description_leaves_out_the_usual_port() {
        let telnet = Profile { kind: "telnet".into(), host: "r1".into(), port: 23, ..Profile::default() };
        assert_eq!(description(&telnet), "Telnet  r1");
        assert_eq!(description(&Profile { port: 2003, ..telnet }), "Telnet  r1:2003");
        let raw = Profile { kind: "raw".into(), host: "cs1".into(), port: 4001, ..Profile::default() };
        assert_eq!(description(&raw), "Raw TCP  cs1:4001");
    }
}
