//! Files between the two ends of a session: one QUIC stream per file, with
//! its size and hash, either way.
//!
//! What arrives goes to a temporary name in the Downloads folder and takes
//! its own name only once the size and the hash check out. Nothing here ever
//! runs a file. On a host the work is done with the rights of the user at the
//! computer, never the service's ([`FileAccess::as_user`]): the folder is that
//! user's to redirect, and the names come from the other side.

use std::fmt;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hext_net::{Connection, FramedRecv, NetError};
use hext_proto::{FileMessage, FileOutcome, MAX_FILE_CHUNK, StreamKind};
use ring::digest;
use ring::rand::{SecureRandom, SystemRandom};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// How often a transfer says how far it got.
const PROGRESS_EVERY: Duration = Duration::from_millis(500);
/// Behind input and the rest of the session, on the same connection.
const FILE_PRIORITY: i32 = -1;
/// What a file starts at, in bytes per second.
const START_RATE: f64 = 1024.0 * 1024.0;
/// What a file never goes below: it is slow, but it ends.
const MIN_RATE: f64 = 128.0 * 1024.0;
/// How often the pace is looked at again.
const PACE_EVERY: Duration = Duration::from_millis(250);
/// Round-trip time this far above the lowest seen means a queue is forming
/// on the way, which is where the video and the mouse wait too.
const QUEUE_DELAY: Duration = Duration::from_millis(30);
/// The lowest round-trip time is forgotten after one to two of these: the
/// path may have changed (direct to relay).
const FLOOR_WINDOW: Duration = Duration::from_secs(30);
/// Sending a wake-up this late may make up for at once (a timer tick on
/// Windows by default); beyond that the time is lost.
const PACE_CATCH_UP: Duration = Duration::from_millis(16);
/// The longest name accepted, in bytes (room left for " (12)").
const MAX_NAME: usize = 240;
/// Temporary files end with this.
const PARTIAL: &str = "hextpart";

/// Where files go and whose rights they are handled with.
pub trait FileAccess: Send + Sync {
    /// The Downloads folder; `None`: nobody is logged on to have one.
    fn downloads(&self) -> Option<PathBuf>;
    /// Runs `work` with the rights of the user of this computer, on this
    /// thread, and only if there is one.
    fn as_user(&self, work: &mut dyn FnMut()) -> io::Result<()>;
    /// The same, held to who is here now, for one file: a file begun for a
    /// user is finished or removed as that user, even if they have left.
    /// `None`: nobody is logged on.
    fn for_one_file(&self) -> Option<Arc<dyn FileAccess>>;
}

/// A process's own folder and rights (it is the user's, or a viewer).
pub struct OwnFiles(pub Option<PathBuf>);

impl FileAccess for OwnFiles {
    fn downloads(&self) -> Option<PathBuf> {
        self.0.clone()
    }

    fn as_user(&self, work: &mut dyn FnMut()) -> io::Result<()> {
        work();
        Ok(())
    }

    fn for_one_file(&self) -> Option<Arc<dyn FileAccess>> {
        Some(Arc::new(Self(self.0.clone())))
    }
}

/// `work` as the user, on a thread that may wait.
async fn as_user<T: Send + 'static>(
    access: &Arc<dyn FileAccess>,
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    let access = access.clone();
    tokio::task::spawn_blocking(move || {
        let (mut work, mut done) = (Some(work), None);
        access.as_user(&mut || done = work.take().map(|work| work()))?;
        done.unwrap_or_else(|| Err(io::Error::other("sem resultado")))
    })
    .await
    .map_err(io::Error::other)?
}

/// Which way a file goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Sending,
    Receiving,
}

/// Where a transfer stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    Going { percent: u8 },
    Done,
    Failed(String),
}

/// What is happening to a file, to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub direction: Direction,
    pub name: String,
    pub stage: Stage,
}

impl Transfer {
    /// More than the percentage moved on.
    pub fn is_news(&self) -> bool {
        !matches!(self.stage, Stage::Going { percent } if percent > 0)
    }
}

impl fmt::Display for Transfer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = &self.name;
        match (&self.stage, self.direction) {
            (Stage::Going { percent }, Direction::Sending) => {
                write!(f, "Enviando {name}: {percent}%")
            }
            (Stage::Going { percent }, Direction::Receiving) => {
                write!(f, "Recebendo {name}: {percent}%")
            }
            (Stage::Done, Direction::Sending) => write!(f, "Enviado: {name}"),
            (Stage::Done, Direction::Receiving) => write!(f, "Recebido em Downloads: {name}"),
            (Stage::Failed(why), Direction::Sending) => write!(f, "Não enviou {name}: {why}"),
            (Stage::Failed(why), Direction::Receiving) => write!(f, "Não recebeu {name}: {why}"),
        }
    }
}

/// Takes each piece of news about the transfers.
pub type Tell = Arc<dyn Fn(Transfer) + Send + Sync>;

/// Asks the person here whether to take a file (its name, already checked,
/// and its announced size). The other side's permission is not enough where
/// that side is the one who gave it.
pub type Confirm =
    Arc<dyn Fn(&str, u64) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

/// `bytes` as a person reads them.
pub fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut size = bytes as f64 / 1024.0;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit]).replace('.', ",")
}

/// One end of a session's file traffic.
pub struct FileEnd {
    pub conn: Connection,
    /// Whether the session has the permission. Without it, what arrives is
    /// refused, and nothing goes.
    pub allowed: bool,
    /// `None`: this end has nowhere to take files.
    pub access: Option<Arc<dyn FileAccess>>,
    /// Files to send, one after the other.
    pub paths: mpsc::UnboundedReceiver<PathBuf>,
    /// What the other side said of the files sent.
    pub answers: mpsc::UnboundedReceiver<(u64, FileOutcome)>,
    /// What to tell the other side of the files received.
    pub outcomes: mpsc::UnboundedSender<(u64, FileOutcome)>,
    pub tell: Tell,
    /// Asked before taking each file; `None`: having the permission is enough.
    pub confirm: Option<Confirm>,
}

/// Sends and receives until the connection ends. Never cancelled midway: a
/// temporary file must not stay behind, and the connection ending is what
/// ends this.
pub async fn run(end: FileEnd) {
    let FileEnd {
        conn,
        allowed,
        access,
        paths,
        answers,
        outcomes,
        tell,
        confirm,
    } = end;
    let access = access.filter(|_| allowed);
    let sending = send_all(&conn, access.clone(), paths, answers, &tell);
    let receiving = receive_all(&conn, access, outcomes, &tell, confirm);
    tokio::join!(sending, receiving);
}

// ------------------------------------------------------------------ names

/// Whether `name` is a plain file name, good on every system here: no path,
/// nothing Windows would read as something else, nothing that hides what the
/// name says. The other side chose it.
pub fn safe_name(name: &str) -> bool {
    // (Windows also reads the superscript digits as ports, and the console
    // and the clock as devices.)
    const WINDOWS: [&str; 31] = [
        "CON",
        "PRN",
        "AUX",
        "NUL",
        "COM1",
        "COM2",
        "COM3",
        "COM4",
        "COM5",
        "COM6",
        "COM7",
        "COM8",
        "COM9",
        "LPT1",
        "LPT2",
        "LPT3",
        "LPT4",
        "LPT5",
        "LPT6",
        "LPT7",
        "LPT8",
        "LPT9",
        "COM\u{b9}",
        "COM\u{b2}",
        "COM\u{b3}",
        "LPT\u{b9}",
        "LPT\u{b2}",
        "LPT\u{b3}",
        "CONIN$",
        "CONOUT$",
        "CLOCK$",
    ];
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    !name.is_empty()
        && name.len() <= MAX_NAME
        && !name.ends_with(['.', ' '])
        && !name.starts_with(' ')
        && !name.chars().any(|c| {
            c.is_control()
                || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
                || matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        })
        && !WINDOWS.iter().any(|w| stem.eq_ignore_ascii_case(w))
        && !name.to_lowercase().ends_with(&format!(".{PARTIAL}"))
}

/// Takes `name` in `dir`, numbered if taken, with an empty file for what will
/// replace it: creating is what cannot replace anything already there.
fn claim_name(dir: &Path, name: &str) -> io::Result<PathBuf> {
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (name, String::new()),
    };
    let numbered = (1u32..).map(|n| dir.join(format!("{stem} ({n}){extension}")));
    for path in std::iter::once(dir.join(name)).chain(numbered) {
        match std::fs::File::create_new(&path) {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    unreachable!("a free number")
}

// ---------------------------------------------------------------- receive

async fn receive_all(
    conn: &Connection,
    access: Option<Arc<dyn FileAccess>>,
    outcomes: mpsc::UnboundedSender<(u64, FileOutcome)>,
    tell: &Tell,
    confirm: Option<Confirm>,
) {
    loop {
        let mut stream = match hext_net::accept_uni(conn).await {
            Ok((StreamKind::File, stream)) => stream,
            Ok((kind, _)) => {
                debug!("stream inesperado: {kind:?}");
                continue;
            }
            Err(NetError::Codec(e)) => {
                debug!("stream inválido: {e}");
                continue;
            }
            Err(_) => return,
        };
        let Ok(FileMessage::Start { id, name, size }) = stream.recv().await else {
            stream.stop();
            continue;
        };
        let outcome = match &access {
            Some(access) if safe_name(&name) => {
                receive_one(&mut stream, access, &name, size, tell, confirm.as_ref()).await
            }
            _ => FileOutcome::Refused,
        };
        if outcome != FileOutcome::Saved {
            stream.stop();
        }
        let _ = outcomes.send((id, outcome));
    }
}

/// A file arriving, under its temporary name.
struct Partial {
    file: File,
    path: PathBuf,
    dir: PathBuf,
}

/// Makes the temporary file. The whole size is asked for and given back: a
/// disk without room says so now, not after the transfer, and a number from
/// the other side does not keep the disk full.
async fn create(access: &Arc<dyn FileAccess>, size: u64) -> io::Result<Option<Partial>> {
    let Some(dir) = access.downloads() else {
        return Ok(None);
    };
    let mut random = [0; 8];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| io::Error::other("sem fonte de números aleatórios"))?;
    let path = dir.join(format!("{:016x}.{PARTIAL}", u64::from_le_bytes(random)));
    let (folder, partial) = (dir.clone(), path.clone());
    let file = as_user(access, move || {
        std::fs::create_dir_all(&folder)?;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)?;
        if let Err(e) = file.set_len(size).and_then(|()| file.set_len(0)) {
            drop(file);
            let _ = std::fs::remove_file(&partial);
            return Err(e);
        }
        Ok(file)
    })
    .await?;
    Ok(Some(Partial {
        file: File::from_std(file),
        path,
        dir,
    }))
}

async fn receive_one(
    stream: &mut FramedRecv,
    access: &Arc<dyn FileAccess>,
    name: &str,
    size: u64,
    tell: &Tell,
    confirm: Option<&Confirm>,
) -> FileOutcome {
    let mut progress = Progress::new(Direction::Receiving, name, size, tell.clone());
    if let Some(confirm) = confirm
        && !confirm(name, size).await
    {
        progress.failed("recusado");
        return FileOutcome::Refused;
    }
    let Some(access) = access.for_one_file() else {
        progress.failed("ninguém entrou no computador");
        return FileOutcome::NoUser;
    };
    let access = &access;
    let partial = match create(access, size).await {
        Ok(Some(partial)) => partial,
        Ok(None) => {
            progress.failed("ninguém entrou no computador");
            return FileOutcome::NoUser;
        }
        Err(e) => {
            warn!("arquivo: {e}");
            progress.failed("não foi possível gravar");
            return FileOutcome::Failed;
        }
    };
    let Partial {
        mut file,
        path,
        dir,
    } = partial;
    let result = write(stream, &mut file, size, &mut progress).await;
    // (Closed before anything else happens to it: Windows would refuse.)
    let closed = file.flush().await;
    let _ = file.into_std().await;
    let result = result.and_then(|()| closed.map_err(|e| e.to_string()));
    let (final_name, partial) = (name.to_owned(), path.clone());
    let saved = match result {
        Ok(()) => {
            as_user(access, move || {
                let target = claim_name(&dir, &final_name)?;
                if let Err(e) = std::fs::rename(&partial, &target) {
                    let _ = std::fs::remove_file(&target);
                    return Err(e);
                }
                mark_as_downloaded(&target);
                Ok(())
            })
            .await
        }
        Err(why) => {
            discard(access, path.clone()).await;
            progress.failed(&why);
            return FileOutcome::Failed;
        }
    };
    match saved {
        Ok(()) => {
            progress.done();
            FileOutcome::Saved
        }
        Err(e) => {
            warn!("arquivo: {e}");
            discard(access, path).await;
            progress.failed("não foi possível gravar");
            FileOutcome::Failed
        }
    }
}

/// Writes down what arrives, checking it against what was announced.
async fn write(
    stream: &mut FramedRecv,
    file: &mut File,
    size: u64,
    progress: &mut Progress,
) -> Result<(), String> {
    let mut hash = digest::Context::new(&digest::SHA256);
    let mut got = 0u64;
    loop {
        let message = stream.recv::<FileMessage>().await;
        match message.map_err(|_| "a conexão caiu".to_owned())? {
            FileMessage::Chunk(chunk) => {
                got += chunk.len() as u64;
                if got > size {
                    return Err("maior do que o anunciado".into());
                }
                hash.update(&chunk);
                file.write_all(&chunk)
                    .await
                    .map_err(|_| "não foi possível gravar".to_owned())?;
                progress.moved(got);
            }
            FileMessage::End { sha256 } if got == size => {
                return if hash.finish().as_ref() == sha256 {
                    Ok(())
                } else {
                    Err("o conteúdo chegou diferente".into())
                };
            }
            _ => return Err("transferência inválida".into()),
        }
    }
}

async fn discard(access: &Arc<dyn FileAccess>, path: PathBuf) {
    if let Err(e) = as_user(access, move || std::fs::remove_file(&path)).await {
        warn!("apagar o arquivo temporário: {e}");
    }
}

/// Files that came from another computer are treated by Windows as such: it
/// warns before running one. (Where the disk has no room for the mark, the
/// file simply goes without it.)
#[cfg(windows)]
fn mark_as_downloaded(path: &Path) {
    let mut mark = path.as_os_str().to_owned();
    mark.push(":Zone.Identifier");
    let _ = std::fs::write(mark, "[ZoneTransfer]\r\nZoneId=3\r\n");
}

#[cfg(not(windows))]
fn mark_as_downloaded(_path: &Path) {}

// ------------------------------------------------------------------- send

async fn send_all(
    conn: &Connection,
    access: Option<Arc<dyn FileAccess>>,
    mut paths: mpsc::UnboundedReceiver<PathBuf>,
    mut answers: mpsc::UnboundedReceiver<(u64, FileOutcome)>,
    tell: &Tell,
) {
    let mut next_id = 0;
    loop {
        let path = tokio::select! {
            path = paths.recv() => path,
            _ = conn.closed() => None,
        };
        let Some(path) = path else {
            // What was waiting its turn is told about too.
            paths.close();
            while let Ok(path) = paths.try_recv() {
                Progress::new(Direction::Sending, &shown(&path), 0, tell.clone())
                    .failed("a conexão caiu");
            }
            return;
        };
        let name = shown(&path);
        let mut progress = Progress::new(Direction::Sending, &name, 0, tell.clone());
        let Some(access) = &access else {
            progress.failed("sem permissão");
            continue;
        };
        let id = next_id;
        next_id += 1;
        match send_one(conn, id, &path, &name, access, &mut answers, &mut progress).await {
            Ok(()) => progress.done(),
            Err(why) => progress.failed(&why),
        }
    }
}

fn shown(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| "?".to_owned(), |n| n.to_string_lossy().into_owned())
}

async fn send_one(
    conn: &Connection,
    id: u64,
    path: &Path,
    name: &str,
    access: &Arc<dyn FileAccess>,
    answers: &mut mpsc::UnboundedReceiver<(u64, FileOutcome)>,
    progress: &mut Progress,
) -> Result<(), String> {
    if !safe_name(name) {
        return Err("nome de arquivo não aceito".into());
    }
    let source = path.to_owned();
    let opened = as_user(access, move || {
        // (Windows will not open a folder at all.)
        if !std::fs::metadata(&source)?.is_file() {
            return Ok(None);
        }
        let file = std::fs::File::open(&source)?;
        let size = file.metadata()?.len();
        Ok(Some((file, size)))
    })
    .await;
    let opened = opened.map_err(|e| {
        warn!("abrir {}: {e}", path.display());
        "não foi possível abrir".to_owned()
    })?;
    let Some((file, size)) = opened else {
        return Err("pastas não podem ser enviadas".into());
    };
    progress.size = size;
    // A write failing here is the other side saying no: its answer says why.
    let sent = stream_file(conn, id, File::from_std(file), name, size, progress).await;
    if let Err(Failure::Here(why)) = sent {
        return Err(why);
    }
    match answer(answers, id).await {
        Some(FileOutcome::Saved) => Ok(()),
        Some(FileOutcome::Refused) => Err("recusado".into()),
        Some(FileOutcome::NoUser) => Err("ninguém entrou no computador".into()),
        Some(FileOutcome::Failed) => Err("o outro lado não conseguiu gravar".into()),
        None => Err("a conexão caiu".into()),
    }
}

enum Failure {
    /// This side could not go on.
    Here(String),
    /// The stream ended under it.
    Stream,
}

impl From<NetError> for Failure {
    fn from(_: NetError) -> Self {
        Self::Stream
    }
}

async fn stream_file(
    conn: &Connection,
    id: u64,
    mut file: File,
    name: &str,
    size: u64,
    progress: &mut Progress,
) -> Result<(), Failure> {
    let mut stream = hext_net::open_uni(conn, StreamKind::File).await?;
    stream.set_priority(FILE_PRIORITY);
    let start = FileMessage::Start {
        id,
        name: name.to_owned(),
        size,
    };
    stream.send(&start).await?;
    let mut hash = digest::Context::new(&digest::SHA256);
    let mut buffer = vec![0; MAX_FILE_CHUNK];
    let mut sent = 0;
    let mut pace = Pace::new(Instant::now());
    while sent < size {
        let room = (size - sent).min(MAX_FILE_CHUNK as u64) as usize;
        let read = match file.read(&mut buffer[..room]).await {
            Ok(0) => return Err(Failure::Here("o arquivo mudou durante o envio".into())),
            Ok(read) => read,
            Err(_) => return Err(Failure::Here("não foi possível ler".into())),
        };
        hash.update(&buffer[..read]);
        stream
            .send(&FileMessage::Chunk(buffer[..read].to_vec()))
            .await?;
        sent += read as u64;
        progress.moved(sent);
        let now = Instant::now();
        if pace.due(now)
            && let Some(path) = hext_net::path(conn)
        {
            pace.look(path.rtt, now);
        }
        let at = pace.spend(read, now);
        if at > now {
            tokio::time::sleep_until(at.into()).await;
        }
    }
    let sha256 = hash
        .finish()
        .as_ref()
        .try_into()
        .expect("SHA-256 has 32 bytes");
    stream.send(&FileMessage::End { sha256 }).await?;
    stream.finish();
    Ok(())
}

/// How fast a file goes. The connection's congestion control fills whatever
/// the network will queue, and the video, sent as it happens, would wait
/// behind that queue: a file takes what is left over, watching the round-trip
/// time and giving way as soon as it climbs.
struct Pace {
    /// Bytes per second.
    rate: f64,
    /// When the next chunk may go.
    next: Instant,
    /// When the pace was last looked at, and the bytes sent since.
    looked: Instant,
    spent: usize,
    /// The lowest round-trip time of this window and of the one before.
    floor: Option<Duration>,
    last_floor: Option<Duration>,
    window_ends: Instant,
    /// A queue already formed: the limit of the path is near, so the rate
    /// climbs carefully from then on.
    near_limit: bool,
}

impl Pace {
    fn new(now: Instant) -> Self {
        Self {
            rate: START_RATE,
            next: now,
            looked: now,
            spent: 0,
            floor: None,
            last_floor: None,
            window_ends: now + FLOOR_WINDOW,
            near_limit: false,
        }
    }

    /// Whether it is time for [`Pace::look`].
    fn due(&self, now: Instant) -> bool {
        now.duration_since(self.looked) >= PACE_EVERY
    }

    /// `len` bytes went out: when the next ones may.
    fn spend(&mut self, len: usize, now: Instant) -> Instant {
        self.spent += len;
        let earliest = now.checked_sub(PACE_CATCH_UP).unwrap_or(now);
        self.next = self.next.max(earliest) + Duration::from_secs_f64(len as f64 / self.rate);
        self.next
    }

    /// Sets the rate by the round-trip time now.
    fn look(&mut self, rtt: Duration, now: Instant) {
        if now >= self.window_ends {
            self.last_floor = self.floor.take();
            self.window_ends = now + FLOOR_WINDOW;
        }
        self.floor = Some(self.floor.map_or(rtt, |floor| floor.min(rtt)));
        let floor = self.floor.into_iter().chain(self.last_floor).min();
        let queued = floor.is_some_and(|floor| rtt > floor + QUEUE_DELAY);

        // By what really went, or a rate the disk or the network never
        // reached would keep climbing, and take as long to come down.
        let elapsed = now.duration_since(self.looked).as_secs_f64();
        let moved = self.spent as f64 / elapsed.max(f64::EPSILON);
        self.rate = if queued {
            self.near_limit = true;
            self.rate.min(moved) * 0.75
        } else {
            let climb = if self.near_limit { 1.05 } else { 1.25 };
            (self.rate * climb).min(moved * 2.0)
        }
        .max(MIN_RATE);
        debug!(rate = self.rate, ?rtt, ?floor, "ritmo do arquivo");
        self.looked = now;
        self.spent = 0;
    }
}

/// What the other side said about file `id`; `None`: the connection ended
/// first. No time limit: a person may be deciding, and the end of the file may
/// still be on its way on a slow network.
async fn answer(
    answers: &mut mpsc::UnboundedReceiver<(u64, FileOutcome)>,
    id: u64,
) -> Option<FileOutcome> {
    while let Some((answered, outcome)) = answers.recv().await {
        if answered == id {
            return Some(outcome);
        }
    }
    None
}

// --------------------------------------------------------------- progress

/// Tells how a transfer goes, without telling too often.
struct Progress {
    direction: Direction,
    name: String,
    size: u64,
    tell: Tell,
    told: Option<Instant>,
}

impl Progress {
    fn new(direction: Direction, name: &str, size: u64, tell: Tell) -> Self {
        Self {
            direction,
            // Named by the other side, on its way to the screen.
            name: name.chars().filter(|c| !c.is_control()).collect(),
            size,
            tell,
            told: None,
        }
    }

    fn say(&self, stage: Stage) {
        (self.tell)(Transfer {
            direction: self.direction,
            name: self.name.clone(),
            stage,
        });
    }

    fn moved(&mut self, bytes: u64) {
        if self
            .told
            .is_some_and(|told| told.elapsed() < PROGRESS_EVERY)
        {
            return;
        }
        let first = self.told.is_none();
        self.told = Some(Instant::now());
        let percent = (u128::from(bytes) * 100 / u128::from(self.size.max(1))).min(99) as u8;
        // The first one is the news: the transfer started.
        self.say(Stage::Going {
            percent: if first { 0 } else { percent.max(1) },
        });
    }

    fn done(&self) {
        self.say(Stage::Done);
    }

    fn failed(&self, why: &str) {
        self.say(Stage::Failed(why.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sends at full pace for `looks` looks, with this round-trip time.
    fn run(pace: &mut Pace, now: &mut Instant, rtt: Duration, looks: u32) {
        for _ in 0..looks {
            let bytes = pace.rate * PACE_EVERY.as_secs_f64();
            pace.spent += bytes as usize;
            *now += PACE_EVERY;
            pace.look(rtt, *now);
        }
    }

    const CLEAR: Duration = Duration::from_millis(20);
    const QUEUED: Duration = Duration::from_millis(200);

    #[test]
    fn a_clear_path_lets_a_file_climb() {
        let mut now = Instant::now();
        let mut pace = Pace::new(now);
        run(&mut pace, &mut now, CLEAR, 20);
        assert!(pace.rate > 50.0 * START_RATE, "{}", pace.rate);
    }

    #[test]
    fn a_queue_slows_a_file_at_once() {
        let mut now = Instant::now();
        let mut pace = Pace::new(now);
        run(&mut pace, &mut now, CLEAR, 12);
        let before = pace.rate;
        run(&mut pace, &mut now, QUEUED, 4);
        assert!(pace.rate < before / 3.0, "{before} -> {}", pace.rate);
    }

    #[test]
    fn a_file_never_stops() {
        let mut now = Instant::now();
        let mut pace = Pace::new(now);
        run(&mut pace, &mut now, CLEAR, 1);
        run(&mut pace, &mut now, QUEUED, 40);
        assert_eq!(pace.rate, MIN_RATE);
    }

    #[test]
    fn after_a_queue_a_file_climbs_carefully() {
        let mut now = Instant::now();
        let mut pace = Pace::new(now);
        run(&mut pace, &mut now, CLEAR, 8);
        run(&mut pace, &mut now, QUEUED, 2);
        let before = pace.rate;
        run(&mut pace, &mut now, CLEAR, 4);
        assert!(pace.rate < before * 1.3, "{before} -> {}", pace.rate);
    }

    #[test]
    fn a_rate_never_reached_does_not_climb() {
        let mut now = Instant::now();
        let mut pace = Pace::new(now);
        for _ in 0..40 {
            pace.spent += 250 * 1024;
            now += PACE_EVERY;
            pace.look(CLEAR, now);
        }
        assert!(pace.rate <= 2.0 * 1024.0 * 1024.0 + 1.0, "{}", pace.rate);
    }

    #[test]
    fn a_slower_path_stops_counting_as_a_queue() {
        let mut now = Instant::now();
        let mut pace = Pace::new(now);
        run(&mut pace, &mut now, CLEAR, 4);
        // Direct to relay: higher for good, not a queue.
        let windows = (2 * FLOOR_WINDOW).as_millis() / PACE_EVERY.as_millis();
        run(&mut pace, &mut now, QUEUED, windows as u32 + 30);
        assert!(pace.rate > MIN_RATE * 2.0, "{}", pace.rate);
    }

    #[test]
    fn chunks_are_spaced_by_the_rate() {
        let now = Instant::now();
        let mut pace = Pace::new(now);
        pace.rate = 1000.0;
        assert_eq!(pace.spend(500, now), now + Duration::from_millis(500));
        // Waking up a little late is made up for, a long pause is not.
        let late = now + Duration::from_millis(510);
        assert_eq!(pace.spend(500, late), now + Duration::from_secs(1));
        let later = now + Duration::from_secs(9);
        assert_eq!(
            pace.spend(500, later),
            later - PACE_CATCH_UP + Duration::from_millis(500)
        );
    }

    #[test]
    fn plain_names_pass() {
        for name in [
            "relatório final.pdf",
            ".gitignore",
            "a.tar.gz",
            "console.txt",
            "COM10.txt",
            "conin.txt",
        ] {
            assert!(safe_name(name), "{name}");
        }
    }

    #[test]
    fn names_that_are_more_than_names_do_not() {
        for name in [
            "",
            "..",
            ".",
            "../segredo.txt",
            "..\\segredo.txt",
            "/etc/passwd",
            "C:\\Windows\\win.ini",
            "C:evil.txt",
            "a.txt:oculto",
            "NUL",
            "con.txt",
            "COM1.tar.gz",
            "CONIN$",
            "conout$.txt",
            "Clock$",
            "COM\u{b9}",
            "com\u{b2}.log",
            "lpt\u{b3}",
            "LPT\u{b9}.txt",
            "ponto.",
            "espaço ",
            " espaço",
            "linha\nnova",
            "abc\u{202E}gnp.exe",
            "x.hextpart",
            "x.HEXTPART",
        ] {
            assert!(!safe_name(name), "{name:?}");
        }
        assert!(!safe_name(&"x".repeat(MAX_NAME + 1)));
    }

    #[test]
    fn nothing_already_there_is_replaced() {
        let dir = std::env::temp_dir().join(format!("hext-free-name-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "meu").unwrap();
        assert_eq!(claim_name(&dir, "a.txt").unwrap(), dir.join("a (1).txt"));
        assert_eq!(claim_name(&dir, "a.txt").unwrap(), dir.join("a (2).txt"));
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"meu");
        assert_eq!(claim_name(&dir, "LEIAME").unwrap(), dir.join("LEIAME"));
        assert_eq!(claim_name(&dir, "LEIAME").unwrap(), dir.join("LEIAME (1)"));
        assert_eq!(claim_name(&dir, ".env").unwrap(), dir.join(".env"));
        assert_eq!(claim_name(&dir, ".env").unwrap(), dir.join(".env (1)"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sizes_read_as_people_say_them() {
        assert_eq!(size_text(0), "0 bytes");
        assert_eq!(size_text(1023), "1023 bytes");
        assert_eq!(size_text(1536), "1,5 KB");
        assert_eq!(size_text(1024 * 1024 * 1024), "1,0 GB");
        assert_eq!(size_text(u64::MAX), "16777216,0 TB");
    }

    #[test]
    fn what_a_transfer_says() {
        let transfer = |direction, stage| Transfer {
            direction,
            name: "a.pdf".into(),
            stage,
        };
        let going = Stage::Going { percent: 40 };
        assert_eq!(
            transfer(Direction::Sending, going).to_string(),
            "Enviando a.pdf: 40%"
        );
        assert_eq!(
            transfer(Direction::Receiving, Stage::Done).to_string(),
            "Recebido em Downloads: a.pdf"
        );
        assert!(!transfer(Direction::Sending, Stage::Going { percent: 40 }).is_news());
        assert!(transfer(Direction::Sending, Stage::Going { percent: 0 }).is_news());
        assert!(transfer(Direction::Sending, Stage::Done).is_news());
    }
}
