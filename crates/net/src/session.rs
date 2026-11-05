use std::time::Duration;

use hext_crypto::PublicKey;
use hext_crypto::pake::{self, HostExchange, SALT_LEN, Verifier, ViewerExchange};
use hext_proto::frame::{self, MAX_FRAME_LEN};
use hext_proto::{
    Control, Datagram, Hello, PREAMBLE_LEN, PROTOCOL_VERSION, PeerInfo, Permissions, RejectReason,
    StreamKind, decode_preamble, encode_preamble,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use zeroize::Zeroizing;

use crate::{Connection, NetError, Result, remote_key};

/// Upper bound for the whole handshake, so silent peers cannot hold a host task.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a peer that is about to fail or leave waits for the other side to
/// close, so its last message is delivered instead of discarded.
const LINGER: Duration = Duration::from_secs(2);

/// How a viewer answers a host that asks for the unattended password.
pub struct PasswordLogin {
    pub password: Zeroizing<String>,
    /// The key of this device: the exchange is tied to it.
    pub viewer: PublicKey,
}

/// Largest chunk read from a stream at once.
const READ_CHUNK: usize = 64 * 1024;

/// Sending half of a stream of length-prefixed postcard messages.
pub struct FramedSend {
    send: iroh::endpoint::SendStream,
    max: usize,
}

impl FramedSend {
    fn new(send: iroh::endpoint::SendStream, max: usize) -> Self {
        Self { send, max }
    }

    pub async fn send<T: Serialize>(&mut self, msg: &T) -> Result<()> {
        self.send
            .write_all(&frame::encode_frame(msg, self.max)?)
            .await?;
        Ok(())
    }

    /// Where this stream stands among the others (0 by default): lower
    /// goes after them. Already-closed streams are fine.
    pub fn set_priority(&mut self, priority: i32) {
        let _ = self.send.set_priority(priority);
    }

    /// Closes our half of the stream. Already-closed streams are fine.
    pub fn finish(&mut self) {
        let _ = self.send.finish();
    }
}

/// Receiving half of a stream of length-prefixed postcard messages.
pub struct FramedRecv {
    recv: iroh::endpoint::RecvStream,
    max: usize,
    /// Bytes received but not yet consumed as a preamble or frame.
    buf: Vec<u8>,
}

impl FramedRecv {
    fn new(recv: iroh::endpoint::RecvStream, max: usize) -> Self {
        Self {
            recv,
            max,
            buf: Vec::new(),
        }
    }

    /// Tells the other side to stop sending on this stream.
    pub fn stop(&mut self) {
        let _ = self.recv.stop(0u32.into());
    }

    /// Receives the next message.
    ///
    /// Cancel-safe, so it can be a `tokio::select!` branch: bytes of a
    /// partially received frame stay buffered for the next call.
    pub async fn recv<T: DeserializeOwned>(&mut self) -> Result<T> {
        loop {
            if let Some(msg) = frame::take_frame(&mut self.buf, self.max)? {
                return Ok(msg);
            }
            self.fill().await?;
        }
    }

    async fn recv_bytes<const N: usize>(&mut self) -> Result<[u8; N]> {
        while self.buf.len() < N {
            self.fill().await?;
        }
        let bytes: [u8; N] = self.buf[..N].try_into().expect("length checked");
        self.buf.drain(..N);
        Ok(bytes)
    }

    /// Appends the next chunk of the stream to `buf`. Cancel-safe.
    async fn fill(&mut self) -> Result<()> {
        match self.recv.read_chunk(READ_CHUNK).await? {
            Some(chunk) => {
                self.buf.extend_from_slice(&chunk);
                Ok(())
            }
            None => Err(NetError::StreamClosed),
        }
    }
}

/// The reliable, ordered control stream of a session.
pub struct ControlStream {
    send: FramedSend,
    recv: FramedRecv,
}

impl ControlStream {
    fn new((send, recv): (iroh::endpoint::SendStream, iroh::endpoint::RecvStream)) -> Self {
        Self {
            send: FramedSend::new(send, MAX_FRAME_LEN),
            recv: FramedRecv::new(recv, MAX_FRAME_LEN),
        }
    }

    pub async fn send(&mut self, msg: &Control) -> Result<()> {
        self.send.send(msg).await
    }

    /// Receives the next message. Cancel-safe (see [`FramedRecv::recv`]).
    pub async fn recv(&mut self) -> Result<Control> {
        self.recv.recv().await
    }

    /// Sends [`Control::Bye`] and waits briefly for the peer to close the
    /// connection, so the Bye is not discarded by our own close.
    pub async fn bye(mut self, conn: &Connection) -> Result<()> {
        self.send(&Control::Bye).await?;
        self.send.finish();
        let _ = tokio::time::timeout(LINGER, conn.closed()).await;
        Ok(())
    }

    async fn send_preamble(&mut self) -> Result<()> {
        self.send
            .send
            .write_all(&encode_preamble(PROTOCOL_VERSION))
            .await?;
        Ok(())
    }

    async fn recv_preamble(&mut self) -> Result<u16> {
        let bytes = self.recv.recv_bytes::<PREAMBLE_LEN>().await?;
        Ok(decode_preamble(bytes)?)
    }
}

/// Client side: opens the control stream and introduces itself. Returns
/// the host, and what it lets this viewer do. `password` answers the host if
/// it asks for the unattended password.
pub async fn client_handshake(
    conn: &Connection,
    hello: Hello,
    password: Option<PasswordLogin>,
) -> Result<(ControlStream, PeerInfo, Permissions)> {
    with_timeout(async {
        let mut control = ControlStream::new(conn.open_bi().await?);
        control.send_preamble().await?;
        let remote = control.recv_preamble().await?;
        if remote != PROTOCOL_VERSION {
            conn.close(0u32.into(), b"incompatible version");
            return Err(NetError::IncompatibleVersion {
                local: PROTOCOL_VERSION,
                remote,
            });
        }
        control.send(&Control::Hello(hello)).await?;
        let mut answer = control.recv().await?;
        if let Control::PasswordChallenge { salt, message } = answer {
            let Some(password) = password else {
                conn.close(0u32.into(), b"password needed");
                return Err(NetError::PasswordNeeded);
            };
            answer = prove_password(conn, &mut control, password, salt, message).await?;
        }
        match answer {
            Control::HelloAck { peer, permissions } => Ok((control, peer, permissions)),
            Control::Reject(reason) => {
                conn.close(0u32.into(), b"rejected");
                Err(NetError::Rejected(reason))
            }
            other => Err(NetError::Unexpected(format!(
                "{other:?} instead of HelloAck"
            ))),
        }
    })
    .await
}

/// The client's side of the password exchange, once the host started it.
/// Returns the host's next message.
async fn prove_password(
    conn: &Connection,
    control: &mut ControlStream,
    login: PasswordLogin,
    salt: [u8; SALT_LEN],
    from_host: Vec<u8>,
) -> Result<Control> {
    let host = remote_key(conn);
    let PasswordLogin { password, viewer } = login;
    // Slow on purpose.
    let secret = tokio::task::spawn_blocking(move || pake::stretch(&password, &salt))
        .await
        .map_err(|e| NetError::Unexpected(e.to_string()))??;
    let (exchange, message) = ViewerExchange::start(&secret, &viewer, &host);
    let key = exchange
        .finish(&from_host)
        .ok_or_else(|| NetError::Unexpected("invalid password exchange".into()))?;
    let proof = key.proof().to_vec();
    control
        .send(&Control::PasswordProof { message, proof })
        .await?;
    match control.recv().await? {
        Control::PasswordOk { confirmation } => {
            // It knows the password, too: holding the host's key is not all
            // the viewer asked for.
            if !key.host_confirmed(&confirmation) {
                conn.close(0u32.into(), b"host not confirmed");
                return Err(NetError::HostNotConfirmed);
            }
            control.recv().await
        }
        other => Ok(other),
    }
}

/// Whether a [`Hello`] gets in.
pub enum Admission {
    Allow(Permissions),
    /// Only with the password behind `verifier`, on the host with key `host`.
    Password {
        permissions: Permissions,
        verifier: Verifier,
        host: PublicKey,
    },
}

/// The host's side of the password exchange: whether the password was right.
async fn check_password(
    conn: &Connection,
    control: &mut ControlStream,
    verifier: &Verifier,
    host: &PublicKey,
) -> Result<bool> {
    let viewer = remote_key(conn);
    let (exchange, message) = HostExchange::start(verifier, &viewer, host);
    control
        .send(&Control::PasswordChallenge {
            salt: *verifier.salt(),
            message,
        })
        .await?;
    let Control::PasswordProof { message, proof } = control.recv().await? else {
        return Ok(false);
    };
    let Some(confirmation) = exchange.finish(&message, &proof) else {
        return Ok(false);
    };
    control
        .send(&Control::PasswordOk {
            confirmation: confirmation.to_vec(),
        })
        .await?;
    Ok(true)
}

/// Host side: accepts the control stream and validates the client.
///
/// `validate` decides whether the [`Hello`] is admitted, and with what
/// permissions; a rejection is sent to the client before the error is
/// returned. A wrong password comes back as
/// [`NetError::Rejected`]`(`[`RejectReason::WrongPassword`]`)`, however long
/// the client stays to hear it.
pub async fn host_handshake(
    conn: &Connection,
    local: PeerInfo,
    validate: impl FnOnce(&Hello) -> Result<Admission, RejectReason>,
) -> Result<(ControlStream, Hello)> {
    let (mut control, hello, admitted) = with_timeout(async {
        let mut control = ControlStream::new(conn.accept_bi().await?);
        let remote = control.recv_preamble().await?;
        // Always answer with our version, so the client can report a mismatch.
        control.send_preamble().await?;
        if remote != PROTOCOL_VERSION {
            control.send.finish();
            let _ = tokio::time::timeout(LINGER, conn.closed()).await;
            return Err(NetError::IncompatibleVersion {
                local: PROTOCOL_VERSION,
                remote,
            });
        }
        let hello = match control.recv().await? {
            Control::Hello(hello) => hello,
            other => return Err(NetError::Unexpected(format!("{other:?} instead of Hello"))),
        };
        let admitted = match validate(&hello) {
            Ok(Admission::Allow(permissions)) => Ok(permissions),
            Ok(Admission::Password {
                permissions,
                verifier,
                host,
            }) => match check_password(conn, &mut control, &verifier, &host).await? {
                true => Ok(permissions),
                false => Err(RejectReason::WrongPassword),
            },
            Err(reason) => Err(reason),
        };
        Ok((control, hello, admitted))
    })
    .await?;
    match admitted {
        Ok(permissions) => {
            let ack = Control::HelloAck {
                peer: local,
                permissions,
            };
            with_timeout(control.send(&ack)).await?;
            Ok((control, hello))
        }
        Err(reason) => {
            // The decision is made: a client that left, or that stays until
            // the deadline, must not turn a wrong guess into something else.
            let _ = tokio::time::timeout(LINGER, async {
                let _ = control.send(&Control::Reject(reason)).await;
                control.send.finish();
                conn.closed().await;
            })
            .await;
            Err(NetError::Rejected(reason))
        }
    }
}

async fn with_timeout<T>(fut: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, fut)
        .await
        .map_err(|_| NetError::Timeout)?
}

/// Opens a unidirectional stream of the given kind.
pub async fn open_uni(conn: &Connection, kind: StreamKind) -> Result<FramedSend> {
    let mut send = conn.open_uni().await?;
    send.write_all(&[kind as u8]).await?;
    Ok(FramedSend::new(send, kind.max_frame_len()))
}

/// Accepts the next unidirectional stream opened by the peer.
pub async fn accept_uni(conn: &Connection) -> Result<(StreamKind, FramedRecv)> {
    let recv = conn.accept_uni().await?;
    let mut framed = FramedRecv::new(recv, frame::MAX_FRAME_LEN);
    let [byte] = framed.recv_bytes::<1>().await?;
    let kind = StreamKind::from_byte(byte)
        .ok_or_else(|| NetError::Unexpected(format!("unknown stream kind {byte}")))?;
    framed.max = kind.max_frame_len();
    Ok((kind, framed))
}

pub fn send_datagram(conn: &Connection, msg: &Datagram) -> Result<()> {
    conn.send_datagram(hext_proto::encode(msg)?.into())?;
    Ok(())
}

/// Receives the next datagram. A [`NetError::Codec`] error means only that
/// datagram was malformed; the connection is still usable.
pub async fn recv_datagram(conn: &Connection) -> Result<Datagram> {
    let bytes = conn.read_datagram().await?;
    Ok(hext_proto::decode(&bytes)?)
}
