//! The connection of a `cargo rustd` to the user's daemon: finding it, starting it when
//! there is none, and replacing one of another version.

use std::{
    fs::OpenOptions,
    io::{BufRead as _, BufReader, Write as _},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::Context as _;

#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(windows)]
use uds_windows::UnixStream;

use super::{
    daemon::{Envelope, Reply, Request, home, socket_in, version},
    switch, wrapper,
};
use crate::socket::{connect, private_dir, private_file};

/// How long a daemon that was started is waited for, and one that was told to exit.
const PATIENCE: Duration = Duration::from_secs(10);

const POLL: Duration = Duration::from_millis(5);

/// Whether a daemon is started when none listens.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Start {
    IfMissing,
    Never,
}

/// Sends `request` to the daemon. `None` when there is none and none was to be started;
/// `Reply::Restart` when the one there is is of another version and none was to be started.
pub(crate) fn ask(request: Request, start: Start) -> anyhow::Result<Option<Reply>> {
    let home = home()?;
    let socket = socket_in(&home);
    // A daemon of another version exits for a client that starts its own, and for one that
    // tells it to stop.
    let replaces = start == Start::IfMissing || matches!(request, Request::Stop);
    let envelope = serde_json::to_string(&Envelope { version: version(), replaces, request })?;
    let mut stream = match (connect(&socket), start) {
        (Ok(stream), _) => stream,
        (Err(_), Start::Never) => return Ok(None),
        (Err(_), Start::IfMissing) => started(&home)?,
    };
    let mut reply = exchange(&mut stream, &envelope)?;
    if let Reply::Restart { .. } = reply {
        if start == Start::Never {
            return Ok(Some(reply));
        }
        // It exits once it has answered; its socket goes with it.
        let asked = Instant::now();
        while connect(&socket).is_ok() && asked.elapsed() < PATIENCE {
            std::thread::sleep(POLL);
        }
        reply = exchange(&mut started(&home)?, &envelope)?;
    }
    Ok(Some(reply))
}

/// Why no daemon answered: there is none, or one ended without taking its socket with it.
pub(crate) fn why_none() -> String {
    let left = home().map(|it| (socket_in(&it), it.join("daemon.log")));
    match left {
        Ok((socket, log)) if socket.exists() => format!(
            "the daemon is gone without having shut down (killed?), its log is {}",
            log.display()
        ),
        _ => "no daemon is running".to_owned(),
    }
}

fn exchange(stream: &mut UnixStream, envelope: &str) -> anyhow::Result<Reply> {
    stream.write_all(format!("{envelope}\n").as_bytes())?;
    let mut line = String::new();
    let read = BufReader::new(stream).read_line(&mut line);
    if !matches!(read, Ok(1..)) {
        let log = home()?.join("daemon.log");
        anyhow::bail!(
            "the daemon ended before it answered (killed?), its log is {}",
            log.display()
        );
    }
    serde_json::from_str(&line).context("the daemon's answer is not one of this version")
}

/// Starts a daemon in a process group of its own, so that what ends this command or its
/// terminal's foreground does not end the daemon, and waits for it to listen.
fn started(home: &Path) -> anyhow::Result<UnixStream> {
    private_dir(home).with_context(|| format!("the daemon's directory {}", home.display()))?;
    let log = private_file(OpenOptions::new().create(true).append(true), &home.join("daemon.log"))?;
    let mut daemon = Command::new(std::env::current_exe()?);
    daemon.arg("daemon").env_remove(wrapper::DRIVEN).env_remove(switch::VARIABLE);
    daemon.stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut daemon, 0);
    daemon.spawn().context("starting the daemon")?;
    let socket = socket_in(home);
    let asked = Instant::now();
    loop {
        match connect(&socket) {
            Ok(stream) => return Ok(stream),
            Err(error) if asked.elapsed() > PATIENCE => {
                return Err(error).with_context(|| {
                    format!("the daemon did not start, see {}", home.join("daemon.log").display())
                });
            }
            Err(_) => std::thread::sleep(POLL),
        }
    }
}
