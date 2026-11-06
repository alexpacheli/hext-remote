//! Video over QUIC datagrams, with forward error correction.
//!
//! [`Packetizer`] cuts a serialized [`VideoFrame`] into datagram-sized
//! shards and adds Reed-Solomon parity per block, so a few lost packets are
//! rebuilt on arrival instead of retransmitted (which would hold back every
//! later frame, as on a stream). Packets are interleaved across blocks so a
//! burst of losses spreads over several of them. Keyframes, which are
//! large and expensive to lose (the viewer stops until the next one), carry
//! more parity.
//!
//! [`VideoSender`] paces the packets ([`Pacer`]): a large frame sent at
//! once arrives as a burst at the slowest link on the path, which overflows
//! its buffer and loses more packets than FEC can rebuild.
//!
//! [`Reassembler`] rebuilds frames and hands them out in order. A frame
//! that cannot be rebuilt breaks the chain of predicted frames after it, and
//! a keyframe costs ten times a frame just when the network is struggling.
//! So the viewer first asks for the packets it lacks ([`Event::Resend`]),
//! which the host keeps for a moment ([`VideoSender::resend`]). Only if
//! they don't come is the frame reported as [`Event::Lost`] (the viewer
//! then asks for a keyframe) and later frames are dropped until a keyframe
//! arrives. While the screen is still, the host repeats the number of its last frame
//! ([`VideoSender::send_idle`]), so a frame that lost every packet is
//! noticed even with nothing after it.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use bytes::Bytes;
use hext_proto::{Datagram, MAX_VIDEO_FRAME_LEN, VideoFrame, VideoPacket};
use iroh::endpoint::{Connection, SendDatagramError};
use reed_solomon_erasure::galois_8::ReedSolomon;
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::{NetError, Result};

/// Data shards per FEC block. Reed-Solomon over GF(256) allows 256 shards
/// in all; smaller blocks keep each loss local and the coding cheap.
pub const BLOCK_SIZE: usize = 64;

/// Parity shards per data shard (at least one per block).
pub const PARITY_RATIO: f32 = 0.2;

/// Parity shards per data shard in a keyframe.
pub const KEYFRAME_PARITY_RATIO: f32 = 0.4;

/// Packets go out at up to this many times the video bitrate: a typical
/// frame leaves within a few milliseconds, a keyframe (often 10× larger)
/// is spread over a few frame intervals instead of leaving in one burst.
pub const PACING_HEADROOM: u32 = 4;

/// A frame's first this long of sending goes out at once after a pause (a
/// typical frame, unpaced); longer frames are paced.
pub const PACING_BURST: Duration = Duration::from_millis(4);

/// Sending a wake-up this late may make up for at once (a timer tick on
/// Windows by default); beyond that the frame is sent slower.
const PACING_CATCH_UP: Duration = Duration::from_millis(16);

/// A send that waited this long was held back by congestion control.
const SEND_STALL: Duration = Duration::from_millis(1);

/// How long the frame due next may go without a packet before its missing
/// packets are asked for. Packets of a frame seldom arrive further apart
/// (34 ms at most in the field), and asking in vain costs little.
pub const RESEND_AFTER: Duration = Duration::from_millis(20);

/// How many times the missing packets are asked for.
pub const RESEND_TRIES: u32 = 2;

/// How long an answer may take, at the least and at the most; in between,
/// two round trips and a margin. Without a round trip known yet, the most
/// likely one.
const RESEND_WAIT_MIN: Duration = Duration::from_millis(60);
const RESEND_WAIT_MAX: Duration = Duration::from_millis(400);
const RESEND_WAIT_DEFAULT: Duration = Duration::from_millis(120);

/// Most packets asked for at once: enough for a large keyframe. What is
/// left out goes in the next try.
const MAX_RESEND_PACKETS: usize = 1024;

/// Frames the host keeps for resending (half a second at 60 fps), and their
/// most bytes.
const HISTORY_FRAMES: usize = 32;
const HISTORY_BYTES: usize = 8 * 1024 * 1024;

/// Complete frames that may wait behind a missing one before it counts as
/// lost, however recent: about a quarter of a second at 60 fps. They reach
/// the decoder in one go, so its queue must hold them.
pub const MAX_WAITING: usize = 16;

/// While waiting for a keyframe (the first one, or one asked for, was lost
/// too), the request is repeated this often.
pub const KEYFRAME_RETRY: Duration = Duration::from_millis(500);

/// Complete frames kept while waiting for a keyframe (packets may arrive
/// out of order, so the keyframe they follow can still come).
const MAX_KEPT: usize = 16;

/// Frames being rebuilt at once; older ones are given up first.
const MAX_PARTIAL: usize = 32;

/// Smallest shard accepted: no real path has a smaller datagram.
const MIN_SHARD: usize = 64;

/// Reports kept until read, and frames given up on that are still watched
/// for late packets.
const MAX_REPORTS: usize = 64;
const MAX_GIVEN_UP: usize = 16;

/// How often the sender reports what it did.
const SEND_REPORT_EVERY: Duration = Duration::from_secs(1);

/// Where the video measurements go: `RUST_LOG=info,hext_video=debug` shows
/// every second, not only those in which something went wrong.
pub const LOG_TARGET: &str = "hext_video";

/// Bytes a [`VideoPacket`] adds to its shard once serialized, at most.
const PACKET_OVERHEAD: usize = 32;

/// Largest video datagram sent. A connection can have several paths (relay
/// and direct, Tailscale's MTU of 1280…) and the reported maximum may be a
/// larger path's: datagrams too big for the path in use are dropped
/// silently (seen in the `iroh` spike). Every QUIC path carries 1200-byte
/// packets, so this always fits.
pub const MAX_DATAGRAM: usize = 1100;

/// Parity shards for a block of `k` data shards.
fn parity_for(k: usize, keyframe: bool) -> usize {
    let ratio = if keyframe {
        KEYFRAME_PARITY_RATIO
    } else {
        PARITY_RATIO
    };
    ((k as f32 * ratio).ceil() as usize).max(1)
}

/// Cuts frames into packets, reusing the Reed-Solomon codecs.
#[derive(Default)]
pub struct Packetizer {
    codecs: HashMap<(usize, usize), ReedSolomon>,
}

impl Packetizer {
    /// Packets of one frame, ready to send (data and parity, interleaved).
    pub fn packetize(
        &mut self,
        frame: u64,
        bytes: &[u8],
        shard: usize,
        keyframe: bool,
    ) -> Vec<VideoPacket> {
        let shard = shard.clamp(MIN_SHARD, u16::MAX as usize);
        let data_shards = bytes.len().div_ceil(shard).max(1);
        let blocks = data_shards.div_ceil(BLOCK_SIZE);

        let mut per_block: Vec<Vec<VideoPacket>> = Vec::with_capacity(blocks);
        for block in 0..blocks {
            let first = block * BLOCK_SIZE;
            let k = (data_shards - first).min(BLOCK_SIZE);
            let parity = parity_for(k, keyframe);
            let mut shards: Vec<Vec<u8>> = (0..k + parity)
                .map(|i| {
                    let mut piece = vec![0; shard];
                    if i < k {
                        let start = (first + i) * shard;
                        let end = (start + shard).min(bytes.len());
                        if start < end {
                            piece[..end - start].copy_from_slice(&bytes[start..end]);
                        }
                    }
                    piece
                })
                .collect();
            self.codecs
                .entry((k, parity))
                .or_insert_with(|| ReedSolomon::new(k, parity).expect("valid shard counts"))
                .encode(&mut shards)
                .expect("shards of equal size");
            per_block.push(
                shards
                    .into_iter()
                    .enumerate()
                    .map(|(index, data)| VideoPacket {
                        frame,
                        len: bytes.len() as u32,
                        shard: shard as u16,
                        block_size: BLOCK_SIZE as u16,
                        block: block as u16,
                        index: index as u16,
                        parity: parity as u16,
                        data,
                    })
                    .collect(),
            );
        }

        // Interleave: packet 0 of every block, then packet 1, …
        let longest = per_block.iter().map(Vec::len).max().unwrap_or(0);
        let mut iters: Vec<_> = per_block.into_iter().map(Vec::into_iter).collect();
        let mut out = Vec::new();
        for _ in 0..longest {
            out.extend(iters.iter_mut().filter_map(Iterator::next));
        }
        out
    }
}

/// Spaces packets so they leave at a steady rate.
///
/// Each packet is given a send time: when the packets before it have had
/// time to leave at the pacing rate. A frame after a pause may send
/// [`PACING_BURST`] worth right away. A late wake-up (coarse timers) sends
/// what is due at once, so the average rate holds.
#[derive(Debug, Clone)]
pub struct Pacer {
    bytes_per_sec: f64,
    /// When the next packet may leave.
    next: Option<Instant>,
}

impl Pacer {
    pub fn new(bits_per_sec: u64) -> Self {
        Self {
            bytes_per_sec: (bits_per_sec.max(1) as f64) / 8.0,
            next: None,
        }
    }

    /// Pacing for a video `bitrate_bps` ([`PACING_HEADROOM`] times it).
    pub fn for_bitrate(bitrate_bps: u32) -> Self {
        Self::new(u64::from(bitrate_bps) * u64::from(PACING_HEADROOM))
    }

    /// A new frame starts: time not used since the last one is not saved
    /// up, except for [`PACING_BURST`].
    pub fn begin_frame(&mut self, now: Instant) {
        let earliest = now.checked_sub(PACING_BURST).unwrap_or(now);
        self.next = Some(self.next.map_or(earliest, |next| next.max(earliest)));
    }

    /// When a packet of `len` bytes may leave (at or before `now` when it
    /// may leave right away); reserves its time.
    pub fn reserve(&mut self, len: usize, now: Instant) -> Instant {
        let earliest = now.checked_sub(PACING_CATCH_UP).unwrap_or(now);
        let at = self.next.unwrap_or(now).max(earliest);
        self.next = Some(at + Duration::from_secs_f64(len as f64 / self.bytes_per_sec));
        at
    }

    /// Sending was held back (congestion) until `now`: the time lost is
    /// not made up, which would be a burst into a congested path.
    pub fn stalled(&mut self, now: Instant) {
        self.next = Some(self.next.map_or(now, |next| next.max(now)));
    }
}

/// Sends frames as video datagrams (host side).
pub struct VideoSender {
    packetizer: Packetizer,
    pacer: Pacer,
    last_frame: Option<u64>,
    /// Held while frames flow, released while the screen is still.
    timers: Option<crate::timer::FineTimers>,
    report: SendReport,
    /// The last frames sent, oldest first, for [`Self::resend`].
    history: VecDeque<SentFrame>,
    history_bytes: usize,
}

/// A frame as it went out.
struct SentFrame {
    frame: u64,
    packets: Vec<SentPacket>,
}

impl SentFrame {
    fn bytes(&self) -> usize {
        self.packets.iter().map(|p| p.datagram.len()).sum()
    }
}

#[derive(Clone)]
struct SentPacket {
    /// Where it stands in its frame: (block, index).
    place: (u16, u16),
    datagram: Bytes,
}

/// The viewer lacks these packets of `frame` (all of them, if none is
/// named).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResendRequest {
    pub frame: u64,
    pub packets: Vec<(u16, u16)>,
}

/// What went out in the current report's second, and the connection's
/// totals when it began (QUIC only knows about loss on the sending side).
struct SendReport {
    since: Instant,
    frames: u32,
    keyframes: u32,
    bytes: u64,
    largest: usize,
    /// Time spent sending frames, and the longest one.
    busy: Duration,
    longest: Duration,
    /// Of that, time held back by congestion control.
    held: Duration,
    /// Packets sent again, and requests for frames no longer kept.
    resent: u32,
    too_old: u32,
    lost_packets: u64,
    lost_bytes: u64,
    sent_packets: u64,
    congestion_events: u64,
}

impl SendReport {
    fn new() -> Self {
        Self {
            since: Instant::now(),
            frames: 0,
            keyframes: 0,
            bytes: 0,
            largest: 0,
            busy: Duration::ZERO,
            longest: Duration::ZERO,
            held: Duration::ZERO,
            resent: 0,
            too_old: 0,
            lost_packets: 0,
            lost_bytes: 0,
            sent_packets: 0,
            congestion_events: 0,
        }
    }

    /// Logs the last second, if it is over: at `info` when packets were lost
    /// or congestion held the video back, else at `debug`.
    fn roll(&mut self, conn: &Connection) {
        if self.since.elapsed() < SEND_REPORT_EVERY {
            return;
        }
        let stats = conn.stats();
        let paths = conn.paths();
        let selected = paths.iter().find(|p| p.is_selected());
        let path = selected.map(|p| p.stats());
        let (cwnd, congestion_events) = path
            .as_ref()
            .map_or((0, 0), |p| (p.cwnd, p.congestion_events));
        let lost = stats.lost_packets.saturating_sub(self.lost_packets);
        let congestion = congestion_events.saturating_sub(self.congestion_events);
        let line = format!(
            "envio: {} quadros ({} keyframes), {:.2} Mbps, maior {} kB, enviando {} ms (maior {} ms), \
             preso no controle de congestionamento {} ms, {} pacotes reenviados ({} pedidos \
             tarde demais) | QUIC: {lost} de {} pacotes perdidos ({} kB), \
             {congestion} eventos de congestionamento, cwnd {} kB",
            self.frames,
            self.keyframes,
            self.bytes as f64 * 8.0 / self.since.elapsed().as_secs_f64() / 1_000_000.0,
            self.largest / 1000,
            self.busy.as_millis(),
            self.longest.as_millis(),
            self.held.as_millis(),
            self.resent,
            self.too_old,
            stats.sent_packets.saturating_sub(self.sent_packets),
            stats.lost_bytes.saturating_sub(self.lost_bytes) / 1000,
            cwnd / 1000,
        );
        let eventful = self.resent > 0 || self.too_old > 0;
        if lost > 0 || congestion > 0 || eventful || self.held > Duration::from_millis(50) {
            info!(target: LOG_TARGET, "{line}");
        } else {
            debug!(target: LOG_TARGET, "{line}");
        }
        *self = Self {
            lost_packets: stats.lost_packets,
            lost_bytes: stats.lost_bytes,
            sent_packets: stats.sent_packets,
            congestion_events,
            ..Self::new()
        };
    }
}

impl VideoSender {
    /// A sender for video of about `bitrate_bps`, which sets the pacing.
    pub fn new(bitrate_bps: u32) -> Self {
        Self {
            packetizer: Packetizer::default(),
            pacer: Pacer::for_bitrate(bitrate_bps),
            last_frame: None,
            timers: None,
            report: SendReport::new(),
            history: VecDeque::new(),
            history_bytes: 0,
        }
    }

    /// Keeps a frame for [`Self::resend`], letting go of the oldest ones.
    fn remember(&mut self, sent: SentFrame) {
        self.history_bytes += sent.bytes();
        self.history.push_back(sent);
        while self.history.len() > HISTORY_FRAMES || self.history_bytes > HISTORY_BYTES {
            let Some(oldest) = self.history.pop_front() else {
                break;
            };
            self.history_bytes -= oldest.bytes();
        }
    }

    /// Packets of a kept frame the viewer asked for.
    fn wanted(&self, request: &ResendRequest) -> Option<Vec<Bytes>> {
        let sent = self.history.iter().find(|s| s.frame == request.frame)?;
        let places: HashSet<(u16, u16)> = request.packets.iter().copied().collect();
        Some(
            sent.packets
                .iter()
                .filter(|p| places.is_empty() || places.contains(&p.place))
                .map(|p| p.datagram.clone())
                .collect(),
        )
    }

    /// Sends one packet when the pacing allows; `false` if the path has
    /// become too small for it.
    async fn paced(&mut self, conn: &Connection, datagram: Bytes) -> Result<bool> {
        let now = Instant::now();
        let at = self.pacer.reserve(datagram.len(), now);
        if at > now {
            tokio::time::sleep_until(at.into()).await;
        }
        let before = Instant::now();
        let sent = conn.send_datagram_wait(datagram).await;
        if before.elapsed() > SEND_STALL {
            self.report.held += before.elapsed();
            self.pacer.stalled(Instant::now());
        }
        match sent {
            Ok(()) => Ok(true),
            Err(SendDatagramError::TooLarge) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Sends again the packets the viewer lacks, if the frame is still kept
    /// (if not, the viewer ends up asking for a keyframe).
    pub async fn resend(&mut self, conn: &Connection, request: &ResendRequest) -> Result<()> {
        let Some(packets) = self.wanted(request) else {
            self.report.too_old += 1;
            return Ok(());
        };
        self.pacer.begin_frame(Instant::now());
        for datagram in packets {
            self.report.resent += 1;
            if !self.paced(conn, datagram).await? {
                break;
            }
        }
        Ok(())
    }

    /// Sends one frame, paced. Waits when the connection is congested, so
    /// a slow network slows the host down instead of dropping packets here.
    /// Requests in `resends` go first: a viewer waiting for them is stopped,
    /// and a large frame takes tens of milliseconds to leave.
    pub async fn send(
        &mut self,
        conn: &Connection,
        frame: &VideoFrame,
        resends: &mut mpsc::Receiver<ResendRequest>,
    ) -> Result<()> {
        let bytes = hext_proto::encode(frame)?;
        let max = conn
            .max_datagram_size()
            .ok_or_else(|| NetError::Unexpected("o outro lado não aceita datagramas".into()))?;
        let shard = max.min(MAX_DATAGRAM).saturating_sub(PACKET_OVERHEAD);
        self.last_frame = Some(frame.seq);
        let packets = self
            .packetizer
            .packetize(frame.seq, &bytes, shard, frame.keyframe);
        self.timers
            .get_or_insert_with(crate::timer::FineTimers::new);
        let started = Instant::now();
        self.pacer.begin_frame(started);
        self.report.frames += 1;
        self.report.keyframes += u32::from(frame.keyframe);
        self.report.bytes += bytes.len() as u64;
        self.report.largest = self.report.largest.max(bytes.len());
        let packets = packets
            .into_iter()
            .map(|packet| {
                let place = (packet.block, packet.index);
                let datagram = hext_proto::encode(&Datagram::Video(packet))?;
                Ok(SentPacket {
                    place,
                    datagram: datagram.into(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        self.remember(SentFrame {
            frame: frame.seq,
            packets: packets.clone(),
        });
        for packet in packets {
            while let Ok(request) = resends.try_recv() {
                self.resend(conn, &request).await?;
            }
            // The path MTU shrank mid-frame: this frame is lost (the viewer
            // asks for a keyframe), the next one uses the new size.
            if !self.paced(conn, packet.datagram).await? {
                return Ok(());
            }
        }
        let took = started.elapsed();
        self.report.busy += took;
        self.report.longest = self.report.longest.max(took);
        self.report.roll(conn);
        Ok(())
    }

    /// While nothing changes on screen: tells the viewer which frame was the
    /// last, so it notices if that one never arrived. Call every few
    /// hundred milliseconds without frames.
    pub async fn send_idle(&mut self, conn: &Connection) -> Result<()> {
        self.timers = None;
        self.report.roll(conn);
        let Some(last_frame) = self.last_frame else {
            return Ok(());
        };
        let datagram = hext_proto::encode(&Datagram::VideoIdle { last_frame })?;
        conn.send_datagram_wait(datagram.into()).await?;
        Ok(())
    }
}

/// What [`Reassembler`] produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The next frame, in order.
    Frame(VideoFrame),
    /// The next frame lacks packets: ask the host for them.
    Resend(ResendRequest),
    /// A frame could not be rebuilt: ask the host for a keyframe. Frames
    /// are dropped until one arrives.
    Lost,
}

/// Totals since the session started, for the stats line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// FEC blocks rebuilt from parity (each hid one or more lost packets).
    pub recovered: u64,
    /// Frames lost despite FEC (or dropped behind a slow decoder).
    pub lost: u64,
    /// Of `lost`: its packets were asked for in vain.
    pub lost_quiet: u64,
    /// Of `lost`: given up on with [`MAX_WAITING`] complete frames behind.
    pub lost_overtaken: u64,
    /// Of `lost`: the decoder's queue was full.
    pub lost_decoder: u64,
    /// Of `lost_quiet` and `lost_overtaken`: not a single packet had arrived.
    pub never_arrived: u64,
    /// Frames given up on too soon: enough packets came afterwards.
    pub premature: u64,
    /// Packets of frames already given up on.
    pub late_packets: u64,
    /// Frames saved by asking for their packets, each sparing a keyframe.
    pub resent: u64,
}

/// Why a frame was given up on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LossTrigger {
    /// Its packets were asked for, [`RESEND_TRIES`] times, in vain.
    Quiet,
    /// [`MAX_WAITING`] complete frames waiting behind it.
    Overtaken,
    /// The decoder could not keep up ([`Reassembler::resync`]).
    Decoder,
}

impl std::fmt::Display for LossTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Quiet => "reenvio sem resposta",
            Self::Overtaken => "MAX_WAITING",
            Self::Decoder => "fila do decodificador",
        })
    }
}

/// Something worth writing down about the video, as it happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Report {
    /// A frame was saved by asking for its packets.
    Resent {
        frame: u64,
        /// How long it took from the first request, and how many there were.
        after: Duration,
        tries: u32,
    },
    /// A frame was given up on.
    Lost {
        frame: u64,
        trigger: LossTrigger,
        /// Not one packet of it had arrived.
        never_arrived: bool,
        /// Packets still needed to rebuild it (unknown if none arrived).
        missing: usize,
        /// FEC blocks of it left unfinished.
        blocks: usize,
        /// Complete frames waiting behind it.
        waiting: usize,
        /// How long it had gone without a packet.
        quiet: Duration,
    },
    /// Enough packets of a frame given up on arrived after all: waiting
    /// longer would have saved it.
    Premature {
        frame: u64,
        /// How long after it was given up on.
        after: Duration,
    },
    /// The picture is back, at a keyframe.
    Resumed {
        trigger: LossTrigger,
        /// How long the picture stood still.
        stalled: Duration,
        /// Frames skipped.
        skipped: u64,
        keyframe_bytes: usize,
        /// From the keyframe's first packet to its last.
        keyframe_transfer: Duration,
    },
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Lost {
                frame,
                trigger,
                never_arrived,
                missing,
                blocks,
                waiting,
                quiet,
            } => {
                write!(f, "quadro {frame} perdido por {trigger}: ")?;
                if never_arrived {
                    write!(f, "nenhum pacote chegou")?;
                } else {
                    write!(f, "faltavam {missing} pacotes em {blocks} blocos")?;
                }
                write!(
                    f,
                    ", {waiting} quadros prontos atrás, {} ms sem pacote",
                    quiet.as_millis()
                )
            }
            Self::Resent {
                frame,
                after,
                tries,
            } => write!(
                f,
                "quadro {frame} salvo por reenvio em {} ms ({tries} pedidos)",
                after.as_millis()
            ),
            Self::Premature { frame, after } => write!(
                f,
                "quadro {frame} perdido cedo demais: os pacotes chegaram {} ms depois",
                after.as_millis()
            ),
            Self::Resumed {
                trigger,
                stalled,
                skipped,
                keyframe_bytes,
                keyframe_transfer,
            } => write!(
                f,
                "imagem voltou após {} ms ({trigger}): {skipped} quadros pulados, keyframe de \
                 {} kB em {} ms",
                stalled.as_millis(),
                keyframe_bytes / 1000,
                keyframe_transfer.as_millis()
            ),
        }
    }
}

/// How packets have been arriving, since this was last read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Arrival {
    /// Longest gap between two packets of one frame: under
    /// [`RESEND_AFTER`], no packets are asked for needlessly.
    pub max_gap: Duration,
    /// Longest a frame took, from its first packet to the last one needed.
    pub max_transfer: Duration,
    /// Most complete frames that overtook a frame still receiving packets:
    /// what [`MAX_WAITING`] must tolerate.
    pub max_overtaken: usize,
}

/// Rebuilds frames from packets and hands them out in order.
pub struct Reassembler {
    /// Next frame to hand out; `None` until the first keyframe.
    next: Option<u64>,
    /// After a loss: drop frames until a keyframe.
    awaiting_keyframe: bool,
    partial: BTreeMap<u64, Partial>,
    complete: BTreeMap<u64, VideoFrame>,
    /// Newest frame number seen (in a packet or an idle notice).
    newest: Option<u64>,
    /// Since when the next frame is known to be missing.
    blocked_since: Option<Instant>,
    /// When a keyframe was last asked for (or the wait for one began).
    keyframe_asked: Option<Instant>,
    codecs: HashMap<(usize, usize), ReedSolomon>,
    pub counters: Counters,
    /// The picture stopped: since when, why, and at which frame.
    stall: Option<(Instant, LossTrigger, u64)>,
    /// Frames given up on, watched for late packets.
    given_up: BTreeMap<u64, GivenUp>,
    /// How long the last complete frame took to arrive.
    last_transfer: (u64, Duration),
    arrival: Arrival,
    reports: Vec<Report>,
    /// The packets asked for last, and how long an answer may take.
    asked: Option<Asked>,
    resend_wait: Duration,
}

/// Packets of a frame were asked for.
struct Asked {
    frame: u64,
    first: Instant,
    last: Instant,
    tries: u32,
}

/// What to do about the frame due next, which is missing.
enum Missing {
    Wait,
    Ask,
    GiveUp(LossTrigger),
}

/// A frame given up on.
struct GivenUp {
    at: Instant,
    /// Packets that would have rebuilt it; 0 once that many came.
    missing: usize,
}

struct Partial {
    len: usize,
    shard: usize,
    blocks: Vec<Block>,
    /// When the first and the last packet of this frame arrived.
    first_packet: Instant,
    last_packet: Instant,
}

struct Block {
    k: usize,
    shards: Vec<Option<Vec<u8>>>,
    received: usize,
    done: bool,
}

impl Default for Reassembler {
    fn default() -> Self {
        Self::new()
    }
}

impl Reassembler {
    pub fn new() -> Self {
        Self {
            next: None,
            awaiting_keyframe: true,
            partial: BTreeMap::new(),
            complete: BTreeMap::new(),
            newest: None,
            blocked_since: None,
            keyframe_asked: None,
            codecs: HashMap::new(),
            counters: Counters::default(),
            stall: None,
            given_up: BTreeMap::new(),
            last_transfer: (u64::MAX, Duration::ZERO),
            arrival: Arrival::default(),
            reports: Vec::new(),
            asked: None,
            resend_wait: RESEND_WAIT_DEFAULT,
        }
    }

    /// The connection's round trip: how long resent packets take to come.
    pub fn set_rtt(&mut self, rtt: Duration) {
        self.resend_wait = (rtt * 2 + RESEND_AFTER).clamp(RESEND_WAIT_MIN, RESEND_WAIT_MAX);
    }

    /// What happened since the last call.
    pub fn take_reports(&mut self) -> Vec<Report> {
        std::mem::take(&mut self.reports)
    }

    /// How packets arrived since the last call.
    pub fn take_arrival(&mut self) -> Arrival {
        std::mem::take(&mut self.arrival)
    }

    fn report(&mut self, report: Report) {
        if self.reports.len() < MAX_REPORTS {
            self.reports.push(report);
        }
    }

    /// Counts a packet of a frame given up on; `true` if it was one.
    fn late(&mut self, frame: u64, now: Instant) -> bool {
        let Some(given_up) = self.given_up.get_mut(&frame) else {
            return false;
        };
        self.counters.late_packets += 1;
        if given_up.missing > 0 {
            given_up.missing -= 1;
            if given_up.missing == 0 {
                let after = now.duration_since(given_up.at);
                self.counters.premature += 1;
                self.report(Report::Premature { frame, after });
            }
        }
        true
    }

    /// Adds a packet; returns the frames (and losses) it made available.
    pub fn push(&mut self, packet: VideoPacket, now: Instant) -> Vec<Event> {
        if self.late(packet.frame, now) {
            return Vec::new();
        }
        if packet.frame == u64::MAX
            || self.next.is_some_and(|next| packet.frame < next)
            || self.complete.contains_key(&packet.frame)
        {
            return Vec::new(); // already handed out, or parity of a done frame
        }
        let frame = packet.frame;
        if let Some(partial) = self.partial.get(&frame) {
            let gap = now.duration_since(partial.last_packet);
            self.arrival.max_gap = self.arrival.max_gap.max(gap);
            let overtaken = self.complete.range(frame..).count();
            self.arrival.max_overtaken = self.arrival.max_overtaken.max(overtaken);
        }
        if let Some(complete) = self.add(packet, now) {
            self.complete.insert(complete.seq, complete);
        }
        self.saw(frame);
        self.drain(now)
    }

    /// The host's last frame, sent while its screen is still.
    pub fn idle(&mut self, last_frame: u64, now: Instant) -> Vec<Event> {
        if last_frame != u64::MAX {
            self.saw(last_frame);
        }
        self.drain(now)
    }

    /// Call periodically (e.g. every 10 ms): losses are noticed on time
    /// even when no packet arrives.
    pub fn tick(&mut self, now: Instant) -> Vec<Event> {
        self.drain(now)
    }

    /// Gives up on the frames in hand and waits for a keyframe (the caller
    /// could not keep up, e.g. a slow decoder); ask the host for one.
    pub fn resync(&mut self, now: Instant) {
        self.counters.lost += 1;
        self.counters.lost_decoder += 1;
        let frame = self.next.unwrap_or(0);
        self.stall.get_or_insert((now, LossTrigger::Decoder, frame));
        self.awaiting_keyframe = true;
        self.keyframe_asked = Some(now);
        self.blocked_since = None;
        self.asked = None;
        self.complete.clear();
        self.partial.clear();
    }

    fn saw(&mut self, frame: u64) {
        self.newest = Some(self.newest.map_or(frame, |n| n.max(frame)));
    }

    /// Stores the packet; returns the frame if it is now complete.
    fn add(&mut self, packet: VideoPacket, now: Instant) -> Option<VideoFrame> {
        let (len, shard) = (packet.len as usize, packet.shard as usize);
        let block_size = packet.block_size as usize;
        let parity = packet.parity as usize;
        // Check the packet against its own layout before allocating anything.
        if len > MAX_VIDEO_FRAME_LEN
            || shard < MIN_SHARD
            || !(16..=256).contains(&block_size)
            || packet.data.len() != shard
        {
            return None;
        }
        let data_shards = len.div_ceil(shard).max(1);
        let block_count = data_shards.div_ceil(block_size);
        let block = packet.block as usize;
        if block >= block_count {
            return None;
        }
        let k = (data_shards - block * block_size).min(block_size);
        let total = k + parity;
        if parity == 0 || total > 256 || packet.index as usize >= total {
            return None;
        }

        if !self.partial.contains_key(&packet.frame) && self.partial.len() >= MAX_PARTIAL {
            self.partial.pop_first();
        }
        let partial = self.partial.entry(packet.frame).or_insert_with(|| Partial {
            len,
            shard,
            blocks: (0..block_count)
                .map(|b| Block {
                    k: (data_shards - b * block_size).min(block_size),
                    shards: Vec::new(),
                    received: 0,
                    done: false,
                })
                .collect(),
            first_packet: now,
            last_packet: now,
        });
        // Packets of one frame must agree on its layout.
        if (partial.len, partial.shard) != (len, shard) || partial.blocks.len() != block_count {
            return None;
        }
        partial.last_packet = now;
        let block = &mut partial.blocks[block];
        if block.done {
            return None;
        }
        if block.shards.is_empty() {
            block.shards = vec![None; total];
        } else if block.shards.len() != total {
            return None;
        }
        let slot = &mut block.shards[packet.index as usize];
        if slot.is_some() {
            return None; // duplicate
        }
        *slot = Some(packet.data);
        block.received += 1;
        if block.received < block.k {
            return None;
        }

        // Enough shards: rebuild missing data shards from parity.
        if block.shards[..block.k].iter().any(Option::is_none) {
            let codec = self
                .codecs
                .entry((block.k, parity))
                .or_insert_with(|| ReedSolomon::new(block.k, parity).expect("valid shard counts"));
            if codec.reconstruct_data(&mut block.shards).is_err() {
                return None;
            }
            self.counters.recovered += 1;
        }
        block.done = true;
        if !partial.blocks.iter().all(|b| b.done) {
            return None;
        }

        let partial = self.partial.remove(&packet.frame)?;
        let transfer = now.duration_since(partial.first_packet);
        self.arrival.max_transfer = self.arrival.max_transfer.max(transfer);
        self.last_transfer = (packet.frame, transfer);
        let mut bytes = Vec::with_capacity(data_shards * partial.shard);
        for block in partial.blocks {
            for shard in block.shards.into_iter().take(block.k) {
                bytes.extend_from_slice(&shard?);
            }
        }
        bytes.truncate(partial.len);
        // A frame whose contents don't decode is as good as lost.
        let frame: VideoFrame = hext_proto::decode(&bytes).ok()?;
        (frame.seq == packet.frame).then_some(frame)
    }

    /// Hands out complete frames in order, and detects losses.
    fn drain(&mut self, now: Instant) -> Vec<Event> {
        let mut events = Vec::new();
        loop {
            if self.awaiting_keyframe {
                // Resume at the newest complete keyframe; older frames are useless.
                let Some(&key) = self
                    .complete
                    .iter()
                    .rev()
                    .find_map(|(seq, frame)| frame.keyframe.then_some(seq))
                else {
                    // Their keyframe may still be on its way; keep the newest.
                    while self.complete.len() > MAX_KEPT {
                        self.complete.pop_first();
                    }
                    // Frames exist but no keyframe is arriving: ask (again).
                    if self.newest.is_some() && !self.progressing(now) {
                        let asked = *self.keyframe_asked.get_or_insert(now);
                        if now.duration_since(asked) > KEYFRAME_RETRY {
                            events.push(Event::Lost);
                            self.keyframe_asked = Some(now);
                        }
                    }
                    break;
                };
                self.complete = self.complete.split_off(&key);
                self.partial = self.partial.split_off(&key);
                if let Some((since, trigger, frame)) = self.stall.take() {
                    self.report(Report::Resumed {
                        trigger,
                        stalled: now.duration_since(since),
                        skipped: key.saturating_sub(frame),
                        keyframe_bytes: self.complete[&key].data.len(),
                        keyframe_transfer: match self.last_transfer {
                            (seq, transfer) if seq == key => transfer,
                            _ => Duration::ZERO,
                        },
                    });
                }
                self.next = Some(key);
                self.awaiting_keyframe = false;
                self.keyframe_asked = None;
                self.blocked_since = None;
            }
            let next = self.next.expect("set once a keyframe arrived");
            if let Some(frame) = self.complete.remove(&next) {
                events.push(Event::Frame(frame));
                self.next = Some(next + 1);
                self.blocked_since = None;
                self.saved(next, now);
                continue;
            }
            match self.missing(next, now) {
                Missing::Wait => break,
                Missing::Ask => {
                    let packets = self.packets_lacking(next);
                    events.push(Event::Resend(ResendRequest {
                        frame: next,
                        packets,
                    }));
                    break;
                }
                Missing::GiveUp(trigger) => {
                    events.push(Event::Lost);
                    self.give_up(next, trigger, now);
                    self.awaiting_keyframe = true;
                    self.keyframe_asked = Some(now);
                    self.blocked_since = None;
                    self.asked = None;
                    self.partial = self.partial.split_off(&(next + 1));
                    // A keyframe may already be waiting.
                }
            }
        }
        // Drop partial frames nothing has added to for long.
        self.partial
            .retain(|_, p| now.duration_since(p.last_packet) < Duration::from_secs(2));
        events
    }

    /// Whether some frame is still receiving packets.
    fn progressing(&self, now: Instant) -> bool {
        self.partial
            .values()
            .any(|p| now.duration_since(p.last_packet) <= RESEND_AFTER * 2)
    }

    /// `frame` was handed out: if its packets had been asked for, that
    /// spared a keyframe.
    fn saved(&mut self, frame: u64, now: Instant) {
        if let Some(asked) = self.asked.take_if(|asked| asked.frame == frame) {
            self.counters.resent += 1;
            self.report(Report::Resent {
                frame,
                after: now.duration_since(asked.first),
                tries: asked.tries,
            });
        }
    }

    /// The packets that would complete `frame` (none named: all of them,
    /// none has arrived).
    fn packets_lacking(&self, frame: u64) -> Vec<(u16, u16)> {
        let Some(partial) = self.partial.get(&frame) else {
            return Vec::new();
        };
        let mut lacking = Vec::new();
        for (number, block) in partial.blocks.iter().enumerate() {
            if block.done {
                continue;
            }
            let number = number as u16;
            if block.shards.is_empty() {
                lacking.extend((0..block.k as u16).map(|index| (number, index)));
            } else {
                let free = block.shards.iter().enumerate();
                let free = free.filter(|(_, shard)| shard.is_none());
                let free = free.map(|(index, _)| (number, index as u16));
                lacking.extend(free.take(block.k.saturating_sub(block.received)));
            }
        }
        lacking.truncate(MAX_RESEND_PACKETS);
        lacking
    }

    /// Writes down why `frame` was given up on, and watches for its late
    /// packets.
    fn give_up(&mut self, frame: u64, trigger: LossTrigger, now: Instant) {
        let partial = self.partial.get(&frame);
        let unfinished = partial
            .into_iter()
            .flat_map(|p| &p.blocks)
            .filter(|b| !b.done);
        let (missing, blocks) = unfinished.fold((0, 0), |(missing, blocks), b| {
            (missing + b.k.saturating_sub(b.received), blocks + 1)
        });
        let since = self.blocked_since.unwrap_or(now);
        let quiet = partial.map_or(since, |p| p.last_packet.max(since));
        let never_arrived = partial.is_none();
        self.counters.lost += 1;
        match trigger {
            LossTrigger::Quiet => self.counters.lost_quiet += 1,
            LossTrigger::Overtaken => self.counters.lost_overtaken += 1,
            LossTrigger::Decoder => self.counters.lost_decoder += 1,
        }
        self.counters.never_arrived += u64::from(never_arrived);
        self.report(Report::Lost {
            frame,
            trigger,
            never_arrived,
            missing,
            blocks,
            waiting: self.complete.len(),
            quiet: now.duration_since(quiet),
        });
        self.stall.get_or_insert((now, trigger, frame));
        if !never_arrived {
            if self.given_up.len() >= MAX_GIVEN_UP {
                self.given_up.pop_first();
            }
            let given_up = GivenUp { at: now, missing };
            self.given_up.insert(frame, given_up);
        }
    }

    /// What to do about `next`, the frame due, which is not complete.
    fn missing(&mut self, next: u64, now: Instant) -> Missing {
        // Only frames known to exist can be missing.
        if !self.newest.is_some_and(|newest| newest >= next) {
            return Missing::Wait;
        }
        if self.complete.len() >= MAX_WAITING {
            return Missing::GiveUp(LossTrigger::Overtaken);
        }
        // Only a frame that stopped receiving packets is missing some: a
        // large one still arriving on a slow link keeps receiving them.
        let since = *self.blocked_since.get_or_insert(now);
        let last_packet = self.partial.get(&next).map(|p| p.last_packet);
        let quiet_since = last_packet.map_or(since, |t| t.max(since));
        if now.duration_since(quiet_since) <= RESEND_AFTER {
            return Missing::Wait;
        }
        match self.asked.as_mut().filter(|asked| asked.frame == next) {
            None => {
                self.asked = Some(Asked {
                    frame: next,
                    first: now,
                    last: now,
                    tries: 1,
                });
                Missing::Ask
            }
            Some(asked) if now.duration_since(asked.last) <= self.resend_wait => Missing::Wait,
            Some(asked) if asked.tries >= RESEND_TRIES => Missing::GiveUp(LossTrigger::Quiet),
            Some(asked) => {
                asked.tries += 1;
                asked.last = now;
                Missing::Ask
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(seq: u64, keyframe: bool, size: usize) -> VideoFrame {
        VideoFrame {
            seq,
            width: 1920,
            height: 1080,
            keyframe,
            capture_micros: 1,
            encode_micros: 2,
            hardware: true,
            data: (0..size).map(|i| (i * 7 + seq as usize) as u8).collect(),
        }
    }

    fn packets(f: &VideoFrame) -> Vec<VideoPacket> {
        Packetizer::default().packetize(f.seq, &hext_proto::encode(f).unwrap(), 1100, f.keyframe)
    }

    fn frames(events: &[Event]) -> Vec<u64> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Frame(f) => Some(f.seq),
                _ => None,
            })
            .collect()
    }

    /// Lets time pass until the missing frame is given up on: returns the
    /// requests made on the way, and when it was.
    fn until_lost(r: &mut Reassembler, from: Instant) -> (Vec<ResendRequest>, Instant) {
        let mut requests = Vec::new();
        for tick in 1..200 {
            let at = from + Duration::from_millis(10) * tick;
            for event in r.tick(at) {
                match event {
                    Event::Resend(request) => requests.push(request),
                    Event::Lost => return (requests, at),
                    Event::Frame(f) => panic!("frame {} arrived", f.seq),
                }
            }
        }
        panic!("never given up on");
    }

    #[test]
    fn layout_and_interleaving() {
        let f = frame(0, true, 200_000);
        let bytes = hext_proto::encode(&f).unwrap().len();
        let pkts = packets(&f);
        let data_shards = bytes.div_ceil(1100);
        let blocks = data_shards.div_ceil(BLOCK_SIZE);
        assert_eq!(blocks, 3);
        let expected: usize = (0..blocks)
            .map(|b| {
                let k = (data_shards - b * BLOCK_SIZE).min(BLOCK_SIZE);
                k + parity_for(k, true)
            })
            .sum();
        assert_eq!(pkts.len(), expected);
        // Packet 0 of every block first: a burst hits different blocks.
        for (i, p) in pkts.iter().take(blocks).enumerate() {
            assert_eq!((p.block as usize, p.index), (i, 0));
        }
        assert!(
            pkts.iter()
                .all(|p| p.data.len() == 1100 && p.len as usize == bytes)
        );
        // 20% parity (40% in keyframes), at least one per block.
        assert_eq!(parity_for(64, false), 13);
        assert_eq!(parity_for(64, true), 26);
        assert_eq!(parity_for(1, false), 1);
    }

    #[test]
    fn delivers_in_order_without_loss() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let mut out = Vec::new();
        for seq in 0..5 {
            for p in packets(&frame(seq, seq == 0, 5_000)) {
                out.extend(r.push(p, now));
            }
        }
        assert_eq!(frames(&out), vec![0, 1, 2, 3, 4]);
        assert_eq!(r.counters, Counters::default());
    }

    /// Feeds `f`'s packets except those `lose` picks; returns the events.
    /// Delivers `f` (after the keyframe it follows, if it is not one)
    /// without the packets `lose` picks.
    fn deliver_with_loss(f: &VideoFrame, lose: impl Fn(usize) -> bool) -> (Vec<Event>, Counters) {
        let mut r = Reassembler::new();
        let now = Instant::now();
        if !f.keyframe {
            for p in packets(&frame(f.seq - 1, true, 1_000)) {
                r.push(p, now);
            }
        }
        let events = packets(f)
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !lose(*i))
            .flat_map(|(_, p)| r.push(p, now))
            .collect();
        (events, r.counters)
    }

    #[test]
    fn fec_rebuilds_random_loss() {
        let f = frame(1, false, 300_000);
        // ~10% loss, deterministic (a simple LCG).
        let lose = |i: usize| (i.wrapping_mul(2_654_435_761) >> 7).is_multiple_of(10);
        let (events, counters) = deliver_with_loss(&f, lose);
        assert_eq!(events, vec![Event::Frame(f)]);
        assert!(counters.recovered > 0);
        assert_eq!(counters.lost, 0);
    }

    #[test]
    fn interleaving_spreads_a_burst() {
        // 20 packets in a row: one block alone could not rebuild that many
        // (13 parity), but interleaved they hit every block, 4-5 each.
        let f = frame(1, false, 300_000);
        let (events, counters) = deliver_with_loss(&f, |i| (100..120).contains(&i));
        assert_eq!(events, vec![Event::Frame(f)]);
        // Blocks that only lost parity need no rebuilding.
        assert!(counters.recovered >= 4, "{counters:?}");
    }

    #[test]
    fn too_much_loss_is_not_a_frame() {
        for keyframe in [false, true] {
            let f = frame(1, keyframe, 300_000);
            let (events, _) = deliver_with_loss(&f, |i| i.is_multiple_of(2));
            assert!(events.is_empty(), "keyframe: {keyframe}");
        }
    }

    #[test]
    fn reordered_packets_and_frames() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let mut all: Vec<VideoPacket> = (0..4)
            .flat_map(|seq| packets(&frame(seq, seq == 0, 3_000)))
            .collect();
        all.reverse();
        let out: Vec<Event> = all.into_iter().flat_map(|p| r.push(p, now)).collect();
        assert_eq!(frames(&out), vec![0, 1, 2, 3]);
    }

    #[test]
    fn duplicates_are_ignored() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let pkts = packets(&frame(0, true, 3_000));
        let mut out = Vec::new();
        for p in pkts.iter().chain(&pkts) {
            out.extend(r.push(p.clone(), now));
        }
        assert_eq!(frames(&out), vec![0]);
    }

    #[test]
    fn unrecoverable_loss_waits_for_a_keyframe() {
        let mut r = Reassembler::new();
        let t0 = Instant::now();
        let mut out = Vec::new();
        out.extend(
            packets(&frame(0, true, 3_000))
                .into_iter()
                .flat_map(|p| r.push(p, t0)),
        );
        // Frame 1 loses all its packets; 2 and 3 arrive.
        for seq in 2..4 {
            out.extend(
                packets(&frame(seq, false, 3_000))
                    .into_iter()
                    .flat_map(|p| r.push(p, t0)),
            );
        }
        assert_eq!(frames(&out), vec![0], "2 and 3 wait for 1");
        // After the wait, 1 is lost: 2 and 3 are dropped.
        let (_, later) = until_lost(&mut r, t0);
        assert_eq!(r.counters.lost, 1);
        // P-frames are dropped until the keyframe (5) arrives.
        let p4: Vec<Event> = packets(&frame(4, false, 3_000))
            .into_iter()
            .flat_map(|p| r.push(p, later))
            .collect();
        assert!(p4.is_empty());
        let k5: Vec<Event> = packets(&frame(5, true, 3_000))
            .into_iter()
            .flat_map(|p| r.push(p, later))
            .collect();
        assert_eq!(frames(&k5), vec![5]);
        let p6: Vec<Event> = packets(&frame(6, false, 3_000))
            .into_iter()
            .flat_map(|p| r.push(p, later))
            .collect();
        assert_eq!(frames(&p6), vec![6]);
    }

    #[test]
    fn many_waiting_frames_declare_the_loss_at_once() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let mut out: Vec<Event> = packets(&frame(0, true, 2_000))
            .into_iter()
            .flat_map(|p| r.push(p, now))
            .collect();
        for seq in 2..2 + MAX_WAITING as u64 {
            out.extend(
                packets(&frame(seq, false, 2_000))
                    .into_iter()
                    .flat_map(|p| r.push(p, now)),
            );
        }
        assert!(out.contains(&Event::Lost));
    }

    /// Keyframe 0 delivered, then frame 1 loses packets past what FEC can
    /// rebuild: returns the reassembler and the packets that did not arrive.
    fn frame_one_cut_short(at: Instant) -> (Reassembler, Vec<VideoPacket>) {
        let mut r = Reassembler::new();
        for p in packets(&frame(0, true, 2_000)) {
            r.push(p, at);
        }
        let mut pkts = packets(&frame(1, false, 50_000));
        let rest = pkts.split_off(10);
        for p in pkts {
            r.push(p, at);
        }
        (r, rest)
    }

    #[test]
    fn a_loss_says_why_and_how_much_was_missing() {
        let t0 = Instant::now();
        let (mut r, _) = frame_one_cut_short(t0);
        let needed = hext_proto::encode(&frame(1, false, 50_000))
            .unwrap()
            .len()
            .div_ceil(1100);
        let (_, later) = until_lost(&mut r, t0);
        assert_eq!(
            r.take_reports(),
            vec![Report::Lost {
                frame: 1,
                trigger: LossTrigger::Quiet,
                never_arrived: false,
                missing: needed - 10,
                blocks: 1,
                waiting: 0,
                quiet: later.duration_since(t0),
            }]
        );
        assert_eq!(r.counters.lost_quiet, 1);
        assert!(r.take_reports().is_empty());
    }

    #[test]
    fn frames_behind_a_missing_one_say_so() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        for seq in [0].into_iter().chain(2..2 + MAX_WAITING as u64) {
            for p in packets(&frame(seq, seq == 0, 2_000)) {
                r.push(p, now);
            }
        }
        assert_eq!(r.counters.lost_overtaken, 1);
        assert_eq!(r.counters.never_arrived, 1);
        let reports = r.take_reports();
        assert!(
            matches!(
                reports[0],
                Report::Lost {
                    frame: 1,
                    trigger: LossTrigger::Overtaken,
                    never_arrived: true,
                    waiting: MAX_WAITING,
                    ..
                }
            ),
            "{reports:?}"
        );
    }

    #[test]
    fn the_stall_is_measured_up_to_the_keyframe() {
        let t0 = Instant::now();
        let (mut r, _) = frame_one_cut_short(t0);
        let (_, lost_at) = until_lost(&mut r, t0);
        r.take_reports();
        let key = frame(5, true, 20_000);
        let resumed_at = lost_at + Duration::from_millis(200);
        let pkts = packets(&key);
        let first = resumed_at - Duration::from_millis(30);

        for (i, p) in pkts.into_iter().enumerate() {
            r.push(p, if i == 0 { first } else { resumed_at });
        }
        assert_eq!(
            r.take_reports(),
            vec![Report::Resumed {
                trigger: LossTrigger::Quiet,
                stalled: Duration::from_millis(200),
                skipped: 4,
                keyframe_bytes: 20_000,
                keyframe_transfer: Duration::from_millis(30),
            }]
        );
    }

    #[test]
    fn a_slow_decoder_counts_apart_from_the_network() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        for p in packets(&frame(0, true, 2_000)) {
            r.push(p, now);
        }
        r.resync(now);
        assert_eq!((r.counters.lost, r.counters.lost_decoder), (1, 1));
        for p in packets(&frame(3, true, 2_000)) {
            r.push(p, now);
        }
        let reports = r.take_reports();
        assert!(
            matches!(
                reports[0],
                Report::Resumed {
                    trigger: LossTrigger::Decoder,
                    skipped: 2,
                    ..
                }
            ),
            "{reports:?}"
        );
    }

    #[test]
    fn packets_after_the_loss_show_it_was_premature() {
        let t0 = Instant::now();
        let (mut r, rest) = frame_one_cut_short(t0);
        let (_, lost_at) = until_lost(&mut r, t0);
        let Report::Lost { missing, .. } = r.take_reports()[0] else {
            panic!("a loss was reported");
        };
        let late = lost_at + Duration::from_millis(15);
        for p in rest.into_iter().take(missing) {
            assert!(r.push(p, late).is_empty());
        }
        assert_eq!(r.counters.late_packets, missing as u64);
        assert_eq!(r.counters.premature, 1);
        assert_eq!(
            r.take_reports(),
            vec![Report::Premature {
                frame: 1,
                after: Duration::from_millis(15),
            }]
        );
    }

    #[test]
    fn arrival_shows_what_the_limits_must_tolerate() {
        let mut r = Reassembler::new();
        let t0 = Instant::now();
        for p in packets(&frame(0, true, 2_000)) {
            r.push(p, t0);
        }
        // Frame 1 starts, 2 and 3 finish first, then 1 comes 25 ms later.
        let mut slow = packets(&frame(1, false, 3_000)).into_iter();
        r.push(slow.next().unwrap(), t0);
        for seq in [2, 3] {
            for p in packets(&frame(seq, false, 2_000)) {
                r.push(p, t0);
            }
        }
        let later = t0 + Duration::from_millis(25);
        for p in slow {
            r.push(p, later);
        }
        assert_eq!(r.counters.lost, 0);
        let arrival = r.take_arrival();
        assert_eq!(arrival.max_gap, Duration::from_millis(25));
        assert_eq!(arrival.max_transfer, Duration::from_millis(25));
        assert_eq!(arrival.max_overtaken, 2);
        assert_eq!(r.take_arrival(), Arrival::default());
    }

    #[test]
    fn the_packets_lacking_are_asked_for_and_save_the_frame() {
        let t0 = Instant::now();
        let (mut r, rest) = frame_one_cut_short(t0);
        assert!(r.tick(t0 + RESEND_AFTER).is_empty(), "may still be coming");

        let asked_at = t0 + RESEND_AFTER * 2;
        let [Event::Resend(request)] = &r.tick(asked_at)[..] else {
            panic!("the packets are asked for");
        };
        assert_eq!(request.frame, 1);
        assert_eq!(r.tick(asked_at + RESEND_AFTER), vec![], "once");

        // Exactly what it takes, none of what arrived.
        let needed = hext_proto::encode(&frame(1, false, 50_000))
            .unwrap()
            .len()
            .div_ceil(1100);
        assert_eq!(request.packets.len(), needed - 10);
        let answered_at = asked_at + Duration::from_millis(45);
        let mut out = Vec::new();
        for (block, index) in &request.packets {
            let p = rest
                .iter()
                .find(|p| (p.block, p.index) == (*block, *index))
                .expect("a packet that had not arrived");
            out.extend(r.push(p.clone(), answered_at));
        }
        assert_eq!(frames(&out), vec![1]);
        assert_eq!(r.counters.resent, 1);
        assert_eq!(r.counters.lost, 0);
        assert_eq!(
            r.take_reports(),
            vec![Report::Resent {
                frame: 1,
                after: Duration::from_millis(45),
                tries: 1,
            }]
        );

        // The chain of frames goes on, with no keyframe in between.
        let next: Vec<Event> = packets(&frame(2, false, 2_000))
            .into_iter()
            .flat_map(|p| r.push(p, answered_at))
            .collect();
        assert_eq!(frames(&next), vec![2]);
    }

    #[test]
    fn frames_behind_wait_for_the_resent_packets() {
        let t0 = Instant::now();
        let (mut r, rest) = frame_one_cut_short(t0);
        for seq in 2..6 {
            for p in packets(&frame(seq, false, 2_000)) {
                assert!(frames(&r.push(p, t0)).is_empty());
            }
        }
        let Some(Event::Resend(request)) = r.tick(t0 + RESEND_AFTER * 2).pop() else {
            panic!("the packets are asked for");
        };
        let mut out = Vec::new();
        for p in rest {
            if request.packets.contains(&(p.block, p.index)) {
                out.extend(r.push(p, t0 + RESEND_AFTER * 4));
            }
        }
        assert_eq!(frames(&out), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_keyframe_is_the_last_resort() {
        let t0 = Instant::now();
        let (mut r, _) = frame_one_cut_short(t0);
        r.set_rtt(Duration::from_millis(40));
        let (requests, lost_at) = until_lost(&mut r, t0);
        assert_eq!(requests.len(), RESEND_TRIES as usize);
        // The first request after RESEND_AFTER, then an answer's wait each.
        let waited = lost_at.duration_since(t0);
        assert!(waited >= Duration::from_millis(240), "{waited:?}");
        assert!(waited <= Duration::from_millis(260), "{waited:?}");
    }

    #[test]
    fn the_wait_for_an_answer_follows_the_round_trip() {
        let mut r = Reassembler::new();
        r.set_rtt(Duration::from_millis(1));
        assert_eq!(r.resend_wait, RESEND_WAIT_MIN);
        r.set_rtt(Duration::from_millis(50));
        assert_eq!(r.resend_wait, Duration::from_millis(120));
        r.set_rtt(Duration::from_secs(1));
        assert_eq!(r.resend_wait, RESEND_WAIT_MAX);
    }

    #[test]
    fn the_host_keeps_recent_frames_within_bounds() {
        let sent = |frame, len: usize| SentFrame {
            frame,
            packets: (0..3)
                .map(|index| SentPacket {
                    place: (0, index),
                    datagram: Bytes::from(vec![index as u8; len]),
                })
                .collect(),
        };
        let mut sender = VideoSender::new(8_000_000);
        for frame in 0..HISTORY_FRAMES as u64 + 5 {
            sender.remember(sent(frame, 10));
        }
        let ask = |frame, packets| ResendRequest { frame, packets };
        assert!(sender.wanted(&ask(0, vec![])).is_none(), "too old");
        let newest = HISTORY_FRAMES as u64 + 4;
        assert_eq!(sender.wanted(&ask(newest, vec![])).unwrap().len(), 3);
        let one = sender.wanted(&ask(newest, vec![(0, 2), (9, 9)])).unwrap();
        assert_eq!(one, vec![Bytes::from(vec![2u8; 10])]);

        // Large frames go sooner.
        sender.remember(sent(100, HISTORY_BYTES / 3 + 1));
        sender.remember(sent(101, 10));
        assert!(sender.history_bytes <= HISTORY_BYTES);
        assert_eq!(sender.history.len(), 1);
    }

    #[test]
    fn a_frame_stuck_alone_is_lost_after_a_while() {
        // The last update before a still screen loses too many packets.
        let mut r = Reassembler::new();
        let t0 = Instant::now();
        let _ = packets(&frame(0, true, 2_000))
            .into_iter()
            .flat_map(|p| r.push(p, t0))
            .count();
        let pkts = packets(&frame(1, false, 50_000));
        for p in pkts.into_iter().take(10) {
            assert!(r.push(p, t0).is_empty());
        }
        assert!(
            r.tick(t0 + RESEND_AFTER).is_empty(),
            "still within the wait"
        );
        until_lost(&mut r, t0);
    }

    #[test]
    fn a_lost_first_keyframe_is_asked_for_again() {
        let mut r = Reassembler::new();
        let t0 = Instant::now();
        // Keyframe 0 never arrives; P-frames do.
        let push = |r: &mut Reassembler, seq, at| -> Vec<Event> {
            packets(&frame(seq, false, 2_000))
                .into_iter()
                .flat_map(|p| r.push(p, at))
                .collect()
        };
        assert!(push(&mut r, 1, t0).is_empty());
        assert!(push(&mut r, 2, t0 + KEYFRAME_RETRY / 2).is_empty());
        assert_eq!(push(&mut r, 3, t0 + KEYFRAME_RETRY * 2), vec![Event::Lost]);
        // Not again right away.
        assert!(push(&mut r, 4, t0 + KEYFRAME_RETRY * 2).is_empty());
    }

    #[test]
    fn a_large_frame_arriving_slowly_is_not_lost() {
        // A 300 KB keyframe over ~400 ms (a slow link): packets keep coming.
        let mut r = Reassembler::new();
        let t0 = Instant::now();
        let f = frame(0, true, 300_000);
        let pkts = packets(&f);
        let step = Duration::from_millis(400) / pkts.len() as u32;
        let mut out = Vec::new();
        for (i, p) in pkts.into_iter().enumerate() {
            let at = t0 + step * i as u32;
            out.extend(r.tick(at));
            out.extend(r.push(p, at));
        }
        assert_eq!(out, vec![Event::Frame(f)]);
    }

    #[test]
    fn idle_notice_reveals_a_frame_lost_entirely() {
        let mut r = Reassembler::new();
        let t0 = Instant::now();
        let out: Vec<Event> = packets(&frame(0, true, 2_000))
            .into_iter()
            .flat_map(|p| r.push(p, t0))
            .collect();
        assert_eq!(frames(&out), vec![0]);
        // Frame 1 loses every packet, then the screen stays still.
        assert!(
            r.tick(t0 + RESEND_AFTER * 20).is_empty(),
            "nothing hints at 1 yet"
        );
        let t1 = t0 + RESEND_AFTER * 20;
        assert!(r.idle(1, t1).is_empty());
        let (requests, _) = until_lost(&mut r, t1);
        assert_eq!(requests.len(), RESEND_TRIES as usize);
        assert!(requests[0].packets.is_empty(), "all of it");
    }

    #[test]
    fn a_lost_keyframe_on_a_still_screen_is_asked_for_again() {
        // The very first keyframe loses every packet; only idle notices follow.
        let mut r = Reassembler::new();
        let t0 = Instant::now();
        assert!(r.idle(0, t0).is_empty());
        assert!(r.tick(t0 + KEYFRAME_RETRY / 2).is_empty());
        assert_eq!(r.tick(t0 + KEYFRAME_RETRY * 2), vec![Event::Lost]);
    }

    #[test]
    fn resync_drops_frames_until_a_keyframe() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let feed = |r: &mut Reassembler, seq, key| -> Vec<u64> {
            let events: Vec<Event> = packets(&frame(seq, key, 2_000))
                .into_iter()
                .flat_map(|p| r.push(p, now))
                .collect();
            frames(&events)
        };
        assert_eq!(feed(&mut r, 0, true), vec![0]);
        r.resync(now);
        assert!(feed(&mut r, 1, false).is_empty());
        assert_eq!(feed(&mut r, 2, true), vec![2]);
        assert_eq!(r.counters.lost, 1);
    }

    #[test]
    fn hostile_layouts_allocate_nothing() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let mut p = packets(&frame(0, true, 2_000)).remove(0);
        // 16 MiB in 1-shard blocks would mean a million blocks.
        p.len = MAX_VIDEO_FRAME_LEN as u32;
        p.block_size = 1;
        assert!(r.push(p.clone(), now).is_empty());
        assert!(r.partial.is_empty());
        // Many frame numbers: the partial frames kept stay bounded.
        p.block_size = BLOCK_SIZE as u16;
        for frame in 0..1000 {
            p.frame = frame;
            let _ = r.push(p.clone(), now);
        }
        assert!(r.partial.len() <= MAX_PARTIAL);
        p.frame = u64::MAX;
        assert!(r.push(p, now).is_empty());
    }

    #[test]
    fn nothing_before_the_first_keyframe() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let out: Vec<Event> = packets(&frame(3, false, 2_000))
            .into_iter()
            .flat_map(|p| r.push(p, now))
            .collect();
        assert!(out.is_empty());
        let out: Vec<Event> = packets(&frame(4, true, 2_000))
            .into_iter()
            .flat_map(|p| r.push(p, now))
            .collect();
        assert_eq!(frames(&out), vec![4]);
    }

    #[test]
    fn malformed_packets_are_ignored() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let mut p = packets(&frame(0, true, 2_000)).remove(0);
        p.data.truncate(10);
        assert!(r.push(p, now).is_empty());
        let mut p = packets(&frame(0, true, 2_000)).remove(0);
        p.index = 999;
        assert!(r.push(p, now).is_empty());
        let mut p = packets(&frame(0, true, 2_000)).remove(0);
        p.len = u32::MAX;
        assert!(r.push(p, now).is_empty());
    }

    #[test]
    fn keyframes_carry_more_parity() {
        let packets_of = |keyframe| packets(&frame(0, keyframe, 50_000)).len();
        let data = (hext_proto::encode(&frame(0, false, 50_000)).unwrap().len()).div_ceil(1100);
        assert_eq!(packets_of(false), data + parity_for(data, false));
        assert_eq!(packets_of(true), data + parity_for(data, true));

        // A quarter of the packets lost: a keyframe survives, a frame with
        // the usual parity does not.
        let lose = |i: usize| i.is_multiple_of(4);
        let (events, counters) = deliver_with_loss(&frame(1, true, 50_000), lose);
        assert_eq!(frames(&events), vec![1]);
        assert!(counters.recovered > 0);
        let (events, _) = deliver_with_loss(&frame(1, false, 50_000), lose);
        assert!(frames(&events).is_empty());
    }

    #[test]
    fn pacer_sends_a_small_frame_at_once() {
        let mut pacer = Pacer::new(32_000_000); // 4 MB/s: 4 ms is 16 kB
        let now = Instant::now() + Duration::from_secs(1);
        pacer.begin_frame(now);
        for _ in 0..14 {
            assert!(pacer.reserve(1100, now) <= now);
        }
    }

    #[test]
    fn pacer_spreads_a_large_frame() {
        let mut pacer = Pacer::new(32_000_000);
        let now = Instant::now() + Duration::from_secs(1);
        // 200 kB: 50 ms at 4 MB/s, the first 4 ms of it at once.
        pacer.begin_frame(now);
        let last = (0..200).map(|_| pacer.reserve(1000, now)).last().unwrap();
        let spread = last.duration_since(now);
        assert!(
            spread > Duration::from_millis(44) && spread < Duration::from_millis(47),
            "{spread:?}"
        );
    }

    #[test]
    fn pacer_catches_up_after_a_late_wakeup() {
        let mut pacer = Pacer::new(8_000_000); // 1 MB/s: 1 ms per kB
        let start = Instant::now() + Duration::from_secs(1);
        pacer.begin_frame(start);
        let first = (0..10).map(|_| pacer.reserve(1000, start)).last().unwrap();
        assert!(first > start);
        // Woken 10 ms late: what was due goes out now, the rest stays paced.
        let late = first + Duration::from_millis(10);
        let due = (0..30)
            .map(|_| pacer.reserve(1000, late))
            .filter(|&at| at <= late)
            .count();
        assert_eq!(due, 10);
        // Much later (the connection was congested): only some is made up.
        let stalled = late + Duration::from_secs(1);
        let due = (0..30)
            .map(|_| pacer.reserve(1000, stalled))
            .filter(|&at| at <= stalled)
            .count();
        assert_eq!(due, 17, "16 ms at 1 kB/ms, plus the packet due now");
    }

    #[test]
    fn pacer_does_not_burst_after_congestion() {
        let mut pacer = Pacer::new(8_000_000);
        let start = Instant::now() + Duration::from_secs(1);
        pacer.begin_frame(start);
        pacer.reserve(1000, start);
        // The connection held the next packet back for 30 ms.
        let unblocked = start + Duration::from_millis(30);
        pacer.stalled(unblocked);
        let due = (0..30)
            .map(|_| pacer.reserve(1000, unblocked))
            .filter(|&at| at <= unblocked)
            .count();
        assert_eq!(due, 1);
    }

    #[test]
    fn pacer_credit_does_not_build_up() {
        let mut pacer = Pacer::new(8_000_000);
        let start = Instant::now() + Duration::from_secs(1);
        pacer.begin_frame(start);
        pacer.reserve(1000, start);
        // A long pause gives only the burst credit, not the whole pause.
        let later = start + Duration::from_secs(10);
        pacer.begin_frame(later);
        let at_once = (0..100)
            .map(|_| pacer.reserve(1000, later))
            .filter(|&at| at <= later)
            .count();
        assert_eq!(
            at_once, 5,
            "4 ms credit at 1 kB/ms, plus the packet at `later`"
        );
    }
}
