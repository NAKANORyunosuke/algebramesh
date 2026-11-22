use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};

use crate::error::OxError;
use crate::listener::ReverseConnectListener;
use crate::session::{OxSession, SessionOptions};

/// Paths for the `SpawnLocal` method (design §5.2): `ox_launch` and the server it
/// starts (for example `ox_asir`) on this host.
#[derive(Debug, Clone)]
pub struct LocalLauncher {
    pub ox_launch: PathBuf,
    pub server: PathBuf,
}

/// The process group of a launcher (Issue #30). The launcher starts the server
/// and may start helpers; `kill_on_drop` stops only the launcher itself. On
/// Unix the launcher leads a group of its own, and the whole group is killed
/// when this is dropped: after a failed start, a cancelled one, a kill, and
/// the end of a session. (A group ID is not reused while any process of the
/// group is alive.) Elsewhere only the launcher is stopped; Windows would need
/// a job object, which is not implemented.
struct ProcessGroup {
    #[cfg(unix)]
    pgid: Option<i32>,
}

impl ProcessGroup {
    fn of(child: &Child) -> Self {
        #[cfg(unix)]
        {
            Self {
                pgid: child.id().and_then(|p| i32::try_from(p).ok()),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = child;
            Self {}
        }
    }

    fn kill(&self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            use nix::sys::signal::{Signal, killpg};
            use nix::unistd::Pid;
            let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

/// A session whose launcher process is a child of this process.
pub struct LocalSession {
    pub session: OxSession,
    child: Child,
    group: ProcessGroup,
}

impl LocalSession {
    /// Shuts the server down and waits for the launcher to exit, killing it if it
    /// does not exit within `grace`.
    pub async fn shutdown(mut self, grace: Duration) -> Result<(), OxError> {
        let result = self.session.shutdown().await;
        if tokio::time::timeout(grace, self.child.wait())
            .await
            .is_err()
        {
            self.group.kill();
            let _ = self.child.kill().await;
        }
        // Helpers the server left behind go with the group.
        self.group.kill();
        result
    }

    /// Sends `SM_control_kill` and stops the launcher process and its group.
    pub async fn kill(mut self, grace: Duration) -> Result<(), OxError> {
        let result = self.session.kill().await;
        if tokio::time::timeout(grace, self.child.wait())
            .await
            .is_err()
        {
            self.group.kill();
            let _ = self.child.kill().await;
        }
        self.group.kill();
        result
    }
}

/// Binds loopback listeners on free ports, starts
/// `ox_launch 127.0.0.1 0 <control> <data> <server> 0` and accepts its connections.
/// The launcher is killed if this process drops the [`LocalSession`].
pub async fn spawn_local(
    launcher: &LocalLauncher,
    accept_timeout: Duration,
    options: SessionOptions,
) -> Result<LocalSession, OxError> {
    let listener =
        ReverseConnectListener::bind(IpAddr::V4(Ipv4Addr::LOCALHOST), 0, 0, options).await?;
    let args = listener.launch_args("127.0.0.1", &launcher.server.to_string_lossy());
    let mut command = Command::new(&launcher.ox_launch);
    command.args(&args).stdin(Stdio::null()).kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(OxError::Spawn)?;
    // From here on, a failure or a cancellation (this future dropped) kills
    // the launcher's whole group.
    let group = ProcessGroup::of(&child);
    match listener.accept(accept_timeout).await {
        Ok(session) => Ok(LocalSession {
            session,
            child,
            group,
        }),
        Err(e) => {
            group.kill();
            let _ = child.kill().await;
            Err(e)
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// A launcher that starts a child and a grandchild, records their PIDs, and
    /// never connects (Issue #30).
    fn fake_launcher(dir: &std::path::Path) -> LocalLauncher {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("ox_launch");
        let pids = dir.join("pids");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 sleep 600 &\n\
                 echo $! >> {pids}\n\
                 sh -c 'sleep 600 & echo $! >> {pids}; wait' &\n\
                 sleep 600\n",
                pids = pids.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        LocalLauncher {
            ox_launch: script,
            server: "unused".into(),
        }
    }

    fn alive(pid: i32) -> bool {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
    }

    async fn helpers(dir: &std::path::Path) -> Vec<i32> {
        for _ in 0..100 {
            let pids: Vec<i32> = std::fs::read_to_string(dir.join("pids"))
                .unwrap_or_default()
                .lines()
                .filter_map(|l| l.trim().parse().ok())
                .collect();
            if pids.len() == 2 {
                return pids;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the fake launcher did not start its helpers");
    }

    async fn all_gone(pids: &[i32]) -> bool {
        for _ in 0..100 {
            if pids.iter().all(|&p| !alive(p)) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    #[tokio::test]
    async fn a_failed_start_kills_the_launchers_descendants() {
        let dir = std::env::temp_dir().join(format!("am-launch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let launcher = fake_launcher(&dir);
        let result = spawn_local(
            &launcher,
            Duration::from_millis(500),
            SessionOptions::default(),
        )
        .await;
        assert!(result.is_err());
        let pids = helpers(&dir).await;
        assert!(all_gone(&pids).await, "helpers left: {pids:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_cancelled_start_kills_the_launchers_descendants() {
        let dir = std::env::temp_dir().join(format!("am-launch-c-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let launcher = fake_launcher(&dir);
        // The start is abandoned (its future dropped) while it waits.
        let waited = tokio::time::timeout(
            Duration::from_millis(500),
            spawn_local(
                &launcher,
                Duration::from_secs(60),
                SessionOptions::default(),
            ),
        )
        .await;
        assert!(waited.is_err());
        let pids = helpers(&dir).await;
        assert!(all_gone(&pids).await, "helpers left: {pids:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
