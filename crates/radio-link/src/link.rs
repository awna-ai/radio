use crate::{
    EndpointId, SecretKey,
    admit::{Admission, Gate, Limits, Standing},
    computer_record::ComputerRecord,
    endpoint::{self, ALPN, JOIN_ALPN},
    frame::{self, CONTROL_CAP},
    membership::SignedRecord,
    wire::{self, Answer, Hello, LINK_MAJOR, LINK_MINOR, Protocol, Request},
};
use iroh::{
    Endpoint, EndpointAddr, TransportAddr,
    endpoint::{Connection, RecvStream, SendStream},
};
use std::{net::SocketAddr, sync::Arc};
use tokio::{
    sync::{Semaphore, mpsc},
    task::{JoinHandle, JoinSet},
    time::{Instant, timeout, timeout_at},
};

pub const REFUSED: u32 = 1;
pub const IDLE: u32 = 2;
pub const BAD_FRAME: u32 = 3;
const BAD_BODY: &str = "the file body is not valid";
const UNANSWERED: &str = "the computer did not answer";

#[derive(Clone)]
pub struct Local {
    pub name: String,
    pub sha: String,
    pub clients: Option<std::path::PathBuf>,
}

#[derive(Debug)]
pub enum Event {
    Arrived {
        from: EndpointId,
        app: String,
        name: String,
        size: u64,
    },
    Computer {
        from: EndpointId,
        record: ComputerRecord,
    },
    Record {
        from: EndpointId,
        record: SignedRecord,
    },
    Hello {
        from: EndpointId,
        hello: Hello,
    },
    App {
        from: EndpointId,
        app: String,
        body: String,
    },
    Join {
        from: EndpointId,
        connection: Connection,
    },
}

pub struct Link {
    endpoint: Endpoint,
    gate: Arc<Gate>,
    local: Local,
}

impl Link {
    pub async fn bind(
        key: SecretKey,
        addrs: &[SocketAddr],
        admission: Arc<dyn Admission>,
        limits: Limits,
        local: Local,
    ) -> Result<Self, &'static str> {
        if !wire::member_name(&local.name) {
            return Err("the computer's name is not usable");
        }
        if !wire::hex64(&local.sha) {
            return Err("the build's hash is not usable");
        }
        let gate = Arc::new(Gate::new(admission, limits));
        let endpoint = endpoint::bind(key, addrs, gate.clone()).await?;
        Ok(Self {
            endpoint,
            gate,
            local,
        })
    }

    pub fn id(&self) -> EndpointId {
        self.endpoint.id()
    }

    pub fn name(&self) -> &str {
        &self.local.name
    }

    pub(crate) fn limits(&self) -> &Limits {
        &self.gate.limits
    }

    pub fn addrs(&self) -> Vec<SocketAddr> {
        self.endpoint.bound_sockets()
    }

    pub fn serve(&self, events: mpsc::Sender<Event>) -> JoinHandle<()> {
        let endpoint = self.endpoint.clone();
        let gate = self.gate.clone();
        let local = self.local.clone();
        tokio::spawn(async move {
            let permits = Arc::new(Semaphore::new(gate.limits.handshakes));
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    incoming = endpoint.accept() => {
                        let Some(incoming) = incoming else { break };
                        let Ok(permit) = permits.clone().try_acquire_owned() else {
                            incoming.refuse();
                            continue;
                        };
                        let gate = gate.clone();
                        let local = local.clone();
                        let events = events.clone();
                        let id = endpoint.id();
                        connections.spawn(async move {
                            let Ok(accepting) = incoming.accept() else { return };
                            let result = timeout(gate.limits.handshake, accepting).await;
                            drop(permit);
                            let Ok(Ok(connection)) = result else { return };
                            if connection.alpn() == JOIN_ALPN {
                                let Some(deadline) = gate.join_deadline(&connection) else { return };
                                let weak = connection.weak_handle();
                                let permit = tokio::select! {
                                    _ = connection.closed() => return,
                                    _ = tokio::time::sleep_until(deadline) => {
                                        connection.close(IDLE.into(), &[]);
                                        return;
                                    },
                                    permit = events.reserve() => {
                                        let Ok(permit) = permit else { return };
                                        permit
                                    },
                                };
                                if Instant::now() >= deadline {
                                    connection.close(IDLE.into(), &[]);
                                    return;
                                }
                                if gate.admission.standing(&connection.remote_id()) == Standing::Refused
                                    || !gate.admission.join_open()
                                {
                                    connection.close(REFUSED.into(), &[]);
                                    return;
                                }
                                permit.send(Event::Join {
                                    from: connection.remote_id(), connection,
                                });
                                tokio::select! {
                                    _ = weak.closed() => return,
                                    _ = tokio::time::sleep_until(deadline) => {},
                                }
                                if let Some(connection) = weak.upgrade() {
                                    connection.close(IDLE.into(), &[]);
                                }
                            } else {
                                serve_connection(connection, gate, local, id, events).await;
                            }
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {},
                }
            }
        })
    }

    pub async fn dial(&self, peer: EndpointId, addrs: &[SocketAddr]) -> Result<Peer, &'static str> {
        if self.gate.admission.standing(&peer) != Standing::Accepted {
            return Err("the computer is not let in");
        }
        if addrs.is_empty() {
            return Err(endpoint::NO_ADDRESS);
        }
        let addrs = endpoint::usable(addrs)?;
        let addr = EndpointAddr::from_parts(peer, addrs.into_iter().map(TransportAddr::Ip));
        let connection = timeout(self.gate.limits.dial, self.endpoint.connect(addr, ALPN))
            .await
            .map_err(|_| "the computer could not be reached")?
            .map_err(|_| "the computer could not be reached")?;
        Ok(Peer {
            connection,
            gate: self.gate.clone(),
        })
    }

    pub async fn dial_join(
        &self,
        peer: EndpointId,
        addrs: &[SocketAddr],
    ) -> Result<Connection, &'static str> {
        if self.gate.admission.standing(&peer) == Standing::Refused {
            return Err("the computer is not let in");
        }
        if addrs.is_empty() {
            return Err(endpoint::NO_ADDRESS);
        }
        let addrs = endpoint::usable(addrs)?;
        let addr = EndpointAddr::from_parts(peer, addrs.into_iter().map(TransportAddr::Ip));
        timeout(
            self.gate.limits.dial,
            self.endpoint.connect(addr, JOIN_ALPN),
        )
        .await
        .map_err(|_| "the computer could not be reached")?
        .map_err(|_| "the computer could not be reached")
    }

    pub fn close_all(&self) {
        self.gate.close_all();
    }

    pub fn close_key(&self, key: &EndpointId) {
        self.gate.close_key(key);
    }

    pub async fn close(&self) {
        self.endpoint.close().await;
    }
}

async fn serve_connection(
    connection: Connection,
    gate: Arc<Gate>,
    local: Local,
    id: EndpointId,
    events: mpsc::Sender<Event>,
) {
    let permits = Arc::new(Semaphore::new(endpoint::STREAMS as usize));
    let mut streams = JoinSet::new();
    let mut idle_at = Instant::now() + gate.limits.idle;
    loop {
        tokio::select! {
            stream = connection.accept_bi() => {
                let Ok((mut send, mut recv)) = stream else { break };
                let started = Instant::now();
                if gate.admission.standing(&connection.remote_id()) != Standing::Accepted {
                    connection.close(REFUSED.into(), &[]);
                    break;
                }
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    let _ = recv.stop(BAD_FRAME.into());
                    let _ = send.reset(BAD_FRAME.into());
                    continue;
                };
                let connection = connection.clone();
                let gate = gate.clone();
                let local = local.clone();
                let events = events.clone();
                streams.spawn(async move {
                    let _permit = permit;
                    serve_stream(connection, gate, local, id, events, (send, recv), started).await;
                });
            }
            _ = streams.join_next(), if !streams.is_empty() => {
                if streams.is_empty() {
                    idle_at = Instant::now() + gate.limits.idle;
                }
            }
            _ = tokio::time::sleep_until(idle_at), if streams.is_empty() => {
                connection.close(IDLE.into(), &[]);
                break;
            }
        }
    }
}

async fn serve_stream(
    connection: Connection,
    gate: Arc<Gate>,
    local: Local,
    id: EndpointId,
    events: mpsc::Sender<Event>,
    stream: (SendStream, RecvStream),
    started: Instant,
) {
    let (mut send, mut recv) = stream;
    let head = timeout_at(
        started + gate.limits.frame.min(gate.limits.file),
        frame::read_head::<Request, _>(&mut recv, CONTROL_CAP),
    )
    .await;
    let Ok(Ok(request)) = head else {
        let _ = recv.stop(BAD_FRAME.into());
        let _ = send.reset(BAD_FRAME.into());
        return;
    };
    if let Request::File {
        to,
        app,
        name,
        size,
    } = request
    {
        let result = if wire::valid_file(&to, &app, &name, size, &id) {
            receive_file(
                &connection,
                &gate,
                &local,
                events,
                (&mut send, &mut recv),
                (app, name, size),
                started + gate.limits.file,
            )
            .await
        } else {
            Err(UNANSWERED)
        };
        if result.is_err() {
            let _ = recv.stop(BAD_FRAME.into());
            let _ = send.reset(BAD_FRAME.into());
        }
        return;
    }
    let deadline = started + gate.limits.frame;
    let result = timeout_at(deadline, async {
        if recv.read(&mut [0]).await.map_err(|_| UNANSWERED)?.is_some() {
            return Err(UNANSWERED);
        }
        let from = connection.remote_id();
        match request {
            Request::File { .. } => unreachable!(),
            Request::Hello { hello } => {
                if !wire::valid_hello(&hello) {
                    return Err(UNANSWERED);
                }
                let fleet = gate.admission.fleet();
                let Some(fleet) = fleet.filter(|fleet| *fleet == hello.fleet) else {
                    connection.close(REFUSED.into(), &[]);
                    return Ok(());
                };
                let answer = Answer::Hello {
                    hello: Hello {
                        protocol: Protocol {
                            major: LINK_MAJOR,
                            minor: LINK_MINOR,
                        },
                        fleet,
                        sha: local.sha,
                        arch: std::env::consts::ARCH.into(),
                        name: local.name,
                    },
                };
                let permit = events.reserve().await.map_err(|_| UNANSWERED)?;
                if gate.admission.standing(&from) != Standing::Accepted
                    || connection.close_reason().is_some()
                {
                    if connection.close_reason().is_none() {
                        connection.close(REFUSED.into(), &[]);
                    }
                    drop(permit);
                    return Ok(());
                }
                permit.send(Event::Hello { from, hello });
                frame::write(&mut send, &answer, CONTROL_CAP).await?;
                send.finish().map_err(|_| UNANSWERED)?;
            }
            Request::App { to, app, body } => {
                if !wire::valid_app(&to, &app, &id) {
                    return Err(UNANSWERED);
                }
                let permit = events.reserve().await.map_err(|_| UNANSWERED)?;
                if gate.admission.standing(&from) != Standing::Accepted
                    || connection.close_reason().is_some()
                {
                    if connection.close_reason().is_none() {
                        connection.close(REFUSED.into(), &[]);
                    }
                    drop(permit);
                    return Ok(());
                }
                permit.send(Event::App { from, app, body });
                frame::write(&mut send, &Answer::Ok, CONTROL_CAP).await?;
                send.finish().map_err(|_| UNANSWERED)?;
            }
            Request::Record { record } => {
                if record.verify().is_err() {
                    return Err(UNANSWERED);
                }
                let permit = events.reserve().await.map_err(|_| UNANSWERED)?;
                if gate.admission.standing(&from) != Standing::Accepted
                    || connection.close_reason().is_some()
                {
                    if connection.close_reason().is_none() {
                        connection.close(REFUSED.into(), &[]);
                    }
                    drop(permit);
                    return Ok(());
                }
                permit.send(Event::Record { from, record });
                frame::write(&mut send, &Answer::Ok, CONTROL_CAP).await?;
                send.finish().map_err(|_| UNANSWERED)?;
            }
            Request::Computer { record } => {
                if record.verify().is_err() {
                    return Err(UNANSWERED);
                }
                let permit = events.reserve().await.map_err(|_| UNANSWERED)?;
                if gate.admission.standing(&from) != Standing::Accepted
                    || connection.close_reason().is_some()
                {
                    if connection.close_reason().is_none() {
                        connection.close(REFUSED.into(), &[]);
                    }
                    drop(permit);
                    return Ok(());
                }
                permit.send(Event::Computer { from, record });
                frame::write(&mut send, &Answer::Ok, CONTROL_CAP).await?;
                send.finish().map_err(|_| UNANSWERED)?;
            }
        }
        Ok(())
    })
    .await;
    if !matches!(result, Ok(Ok(()))) {
        let _ = recv.stop(BAD_FRAME.into());
        let _ = send.reset(BAD_FRAME.into());
    }
}

async fn receive_file(
    connection: &Connection,
    gate: &Arc<Gate>,
    local: &Local,
    events: mpsc::Sender<Event>,
    stream: (&mut SendStream, &mut RecvStream),
    head: (String, String, u64),
    deadline: Instant,
) -> Result<(), &'static str> {
    let (send, recv) = stream;
    let (app, name, size) = head;
    let from = connection.remote_id();
    if gate.admission.standing(&from) != Standing::Accepted || connection.close_reason().is_some() {
        connection.close(REFUSED.into(), &[]);
        return Ok(());
    }
    let reservation = timeout_at(deadline, {
        let gate = gate.clone();
        let clients = local.clients.clone();
        let app = app.clone();
        let name = name.clone();
        tokio::task::spawn_blocking(move || {
            if !crate::delivery::installed(clients.as_deref(), &app) {
                Err(wire::NO_FOLDER)
            } else if let Some(folder) = gate.admission.delivery_folder(&app) {
                gate.reserve_delivery(from, &folder, &name, size)
            } else {
                Err(wire::NO_FOLDER)
            }
        })
    })
    .await
    .map_err(|_| BAD_BODY)?
    .map_err(|_| BAD_BODY)?;
    let (dir, reservation) = match reservation {
        Ok(reservation) => reservation,
        Err(reason) => {
            return timeout(gate.limits.frame, refuse_file(send, reason))
                .await
                .map_err(|_| UNANSWERED)?;
        }
    };
    let event = timeout_at(deadline, events.reserve_owned())
        .await
        .map_err(|_| BAD_BODY)?
        .map_err(|_| UNANSWERED)?;
    if gate.admission.standing(&from) != Standing::Accepted || connection.close_reason().is_some() {
        connection.close(REFUSED.into(), &[]);
        return Ok(());
    }
    let (sender, mut chunks) = mpsc::channel::<Vec<u8>>(2);
    let arrival = Event::Arrived {
        from,
        app,
        name: name.clone(),
        size,
    };
    let worker = tokio::task::spawn_blocking(move || {
        let _reservation = reservation;
        let written = crate::delivery::write_open(&dir, &name, |file| {
            use std::io::Write;
            loop {
                let bytes = chunks.blocking_recv().ok_or(BAD_BODY)?;
                if bytes.is_empty() {
                    return Ok(());
                }
                file.write_all(&bytes).map_err(|_| wire::WRITE)?;
            }
        });
        if written.is_ok() {
            event.send(arrival);
        }
        written
    });
    let written = finish_file(
        worker,
        feed_file(recv, sender, size, gate.limits.stall),
        deadline,
    )
    .await;
    timeout(gate.limits.frame, async {
        match written {
            Ok(()) => {
                frame::write(send, &Answer::Ok, CONTROL_CAP).await?;
                send.finish().map_err(|_| UNANSWERED)
            }
            Err(BAD_BODY) => Err(BAD_BODY),
            Err(reason) => refuse_file(send, reason).await,
        }
    })
    .await
    .map_err(|_| UNANSWERED)?
}

async fn finish_file(
    mut worker: JoinHandle<Result<(), &'static str>>,
    feed: impl std::future::Future<Output = Result<(), &'static str>>,
    deadline: Instant,
) -> Result<(), &'static str> {
    tokio::select! {
        written = &mut worker => written.map_err(|_| UNANSWERED)?,
        fed = timeout_at(deadline, feed) => {
            let written = worker.await.map_err(|_| UNANSWERED)?;
            fed.map_err(|_| BAD_BODY)??;
            written
        }
    }
}

async fn feed_file<R: tokio::io::AsyncRead + Unpin>(
    recv: &mut R,
    sender: mpsc::Sender<Vec<u8>>,
    size: u64,
    stall: std::time::Duration,
) -> Result<(), &'static str> {
    use tokio::io::AsyncReadExt;
    let mut remaining = size;
    let mut buffer = vec![0; 64 * 1024];
    while remaining > 0 {
        let take = remaining.min(buffer.len() as u64) as usize;
        let count = timeout(stall, recv.read(&mut buffer[..take]))
            .await
            .map_err(|_| BAD_BODY)?
            .map_err(|_| BAD_BODY)?;
        if count == 0 {
            return Err(BAD_BODY);
        }
        remaining -= count as u64;
        sender
            .send(buffer[..count].to_vec())
            .await
            .map_err(|_| UNANSWERED)?;
    }
    if timeout(stall, recv.read(&mut [0]))
        .await
        .map_err(|_| BAD_BODY)?
        .map_err(|_| BAD_BODY)?
        != 0
    {
        return Err(BAD_BODY);
    }
    sender.send(Vec::new()).await.map_err(|_| UNANSWERED)?;
    Ok(())
}

async fn exchange_file<R, W, A>(
    file: &mut R,
    send: &mut W,
    recv: &mut A,
    size: u64,
    metadata: Option<&std::fs::File>,
    deadline: Instant,
    answer_wait: std::time::Duration,
) -> Result<(), &'static str>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
    A: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let upload = async {
        let mut remaining = size;
        let mut buffer = vec![0; 64 * 1024];
        while remaining > 0 {
            let take = remaining.min(buffer.len() as u64) as usize;
            let count = file
                .read(&mut buffer[..take])
                .await
                .map_err(|_| "the file changed while it was sent")?;
            if count == 0 {
                return Err("the file changed while it was sent");
            }
            send.write_all(&buffer[..count])
                .await
                .map_err(|_| UNANSWERED)?;
            remaining -= count as u64;
        }
        if file
            .read(&mut [0])
            .await
            .map_err(|_| "the file changed while it was sent")?
            != 0
            || metadata.is_some_and(|file| file.metadata().map_or(true, |meta| meta.len() != size))
        {
            return Err("the file changed while it was sent");
        }
        send.shutdown().await.map_err(|_| UNANSWERED)
    };
    let upload = timeout_at(deadline, upload);
    let upload = async { upload.await.map_err(|_| UNANSWERED)? };
    let answer = frame::read::<Answer, _>(recv, CONTROL_CAP);
    tokio::pin!(upload, answer);
    let received = tokio::select! {
        biased;
        received = &mut answer => {
            match received.map_err(|_| UNANSWERED)? {
                Answer::Refused { reason } => return Err(file_refusal(&reason)),
                Answer::Ok => { upload.await?; return Ok(()); }
                _ => return Err(UNANSWERED),
            }
        }
        sent = &mut upload => {
            if let Err(error) = sent && error != UNANSWERED { return Err(error); }
            // A receiver refusing before the body stops its receive stream too.
            // Read its answer before turning that stop into a generic error.
            let received = timeout(answer_wait, answer).await.map_err(|_| UNANSWERED)?.map_err(|_| UNANSWERED)?;
            if sent.is_err() && !matches!(received, Answer::Refused { .. }) { return Err(UNANSWERED); }
            received
        }
    };
    match received {
        Answer::Ok => Ok(()),
        Answer::Refused { reason } => Err(file_refusal(&reason)),
        _ => Err(UNANSWERED),
    }
}

fn file_refusal(reason: &str) -> &'static str {
    match reason {
        wire::NO_FOLDER => "that app takes no files on that computer",
        wire::EXISTS => "a file with that name is already there",
        wire::BUSY => "that computer is busy; try again",
        wire::FOLDER => "that app's folder on that computer is not usable",
        wire::WRITE => "that computer could not write the file",
        wire::FULL => "that app's folder on that computer is full",
        wire::SPACE => "that computer has no room for the file",
        _ => UNANSWERED,
    }
}

async fn refuse_file(send: &mut SendStream, reason: &str) -> Result<(), &'static str> {
    frame::write(
        send,
        &Answer::Refused {
            reason: reason.into(),
        },
        CONTROL_CAP,
    )
    .await?;
    send.finish().map_err(|_| UNANSWERED)
}

pub struct Peer {
    connection: Connection,
    gate: Arc<Gate>,
}

impl Peer {
    pub fn id(&self) -> EndpointId {
        self.connection.remote_id()
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub async fn hello(&self, hello: &Hello) -> Result<Hello, &'static str> {
        match self
            .request(&Request::Hello {
                hello: hello.clone(),
            })
            .await?
        {
            Answer::Hello { hello }
                if wire::valid_hello(&hello)
                    && self
                        .gate
                        .admission
                        .fleet()
                        .is_some_and(|fleet| fleet == hello.fleet) =>
            {
                Ok(hello)
            }
            _ => Err(UNANSWERED),
        }
    }

    pub async fn send_file(
        &self,
        app: &str,
        name: &str,
        file: std::fs::File,
        size: u64,
    ) -> Result<(), &'static str> {
        if !wire::file_name(name) {
            return Err("not a file name");
        }
        if size > wire::FILE_CAP {
            return Err("the file is larger than 64 MiB");
        }
        let deadline = Instant::now() + self.gate.limits.file;
        let (mut send, mut recv) = timeout_at(deadline, async {
            let (mut send, recv) = self.connection.open_bi().await.map_err(|_| UNANSWERED)?;
            frame::write(
                &mut send,
                &Request::File {
                    to: self.id().to_string(),
                    app: app.into(),
                    name: name.into(),
                    size,
                },
                CONTROL_CAP,
            )
            .await
            .map_err(|_| UNANSWERED)?;
            Ok::<_, &'static str>((send, recv))
        })
        .await
        .map_err(|_| UNANSWERED)??;
        let metadata = file
            .try_clone()
            .map_err(|_| "the file could not be opened")?;
        let mut file = tokio::fs::File::from_std(file);
        let result = exchange_file(
            &mut file,
            &mut send,
            &mut recv,
            size,
            Some(&metadata),
            deadline,
            self.gate.limits.frame,
        )
        .await;
        if result.is_err() {
            let _ = recv.stop(BAD_FRAME.into());
            let _ = send.reset(BAD_FRAME.into());
        }
        result
    }

    pub async fn send_app(&self, app: &str, body: &str) -> Result<(), &'static str> {
        let request = Request::App {
            to: self.id().to_string(),
            app: app.into(),
            body: body.into(),
        };
        match self.request(&request).await? {
            Answer::Ok => Ok(()),
            _ => Err(UNANSWERED),
        }
    }

    pub async fn send_record(&self, record: &SignedRecord) -> Result<(), &'static str> {
        match self
            .request(&Request::Record {
                record: record.clone(),
            })
            .await?
        {
            Answer::Ok => Ok(()),
            _ => Err(UNANSWERED),
        }
    }

    pub async fn send_computer(&self, record: &ComputerRecord) -> Result<(), &'static str> {
        match self
            .request(&Request::Computer {
                record: record.clone(),
            })
            .await?
        {
            Answer::Ok => Ok(()),
            _ => Err(UNANSWERED),
        }
    }

    async fn request(&self, request: &Request) -> Result<Answer, &'static str> {
        timeout(self.gate.limits.frame, async {
            let (mut send, mut recv) = self.connection.open_bi().await.map_err(|_| UNANSWERED)?;
            let result = async {
                frame::write(&mut send, request, CONTROL_CAP).await?;
                send.finish().map_err(|_| UNANSWERED)?;
                frame::read(&mut recv, CONTROL_CAP).await
            }
            .await;
            if result.is_err() {
                let _ = recv.stop(BAD_FRAME.into());
                let _ = send.reset(BAD_FRAME.into());
            }
            result
        })
        .await
        .map_err(|_| UNANSWERED)?
        .map_err(|_| UNANSWERED)
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;

    #[tokio::test]
    async fn only_an_exact_body_completes_the_writer() {
        for (bytes, size, expected) in [
            (b"abc".as_slice(), 3, Ok(())),
            (b"ab".as_slice(), 3, Err(BAD_BODY)),
            (b"abcd".as_slice(), 3, Err(BAD_BODY)),
            (b"".as_slice(), 0, Ok(())),
        ] {
            let (sender, mut receiver) = mpsc::channel(4);
            assert_eq!(
                feed_file(
                    &mut &bytes[..],
                    sender,
                    size,
                    std::time::Duration::from_secs(30)
                )
                .await,
                expected
            );
            let mut chunks = Vec::new();
            while let Some(chunk) = receiver.recv().await {
                chunks.push(chunk);
            }
            assert_eq!(chunks.last().is_some_and(Vec::is_empty), expected.is_ok());
        }
    }

    #[tokio::test]
    async fn a_large_upload_observes_refusal_without_body_consumption() {
        use tokio::io::{AsyncWriteExt, duplex};
        let (mut send, _unread_body) = duplex(1);
        let (mut answer, mut recv) = duplex(128);
        frame::write(
            &mut answer,
            &Answer::Refused {
                reason: wire::BUSY.into(),
            },
            CONTROL_CAP,
        )
        .await
        .unwrap();
        answer.shutdown().await.unwrap();
        let mut file = &vec![0u8; 1024 * 1024][..];
        assert_eq!(
            timeout(
                std::time::Duration::from_millis(100),
                exchange_file(
                    &mut file,
                    &mut send,
                    &mut recv,
                    1024 * 1024,
                    None,
                    Instant::now() + std::time::Duration::from_secs(5),
                    std::time::Duration::from_secs(5)
                )
            )
            .await,
            Ok(Err("that computer is busy; try again"))
        );
    }

    #[tokio::test]
    async fn an_invalid_file_answer_is_unanswered() {
        let mut answer = &b"\0\0\0\x01?"[..];
        assert_eq!(
            exchange_file(
                &mut &b"abc"[..],
                &mut tokio::io::sink(),
                &mut answer,
                3,
                None,
                Instant::now() + std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(1),
            )
            .await,
            Err("the computer did not answer")
        );
    }

    #[tokio::test]
    async fn an_answer_eof_during_upload_is_unanswered() {
        let (mut send, _unread_body) = tokio::io::duplex(1);
        assert_eq!(
            exchange_file(
                &mut &b"abc"[..],
                &mut send,
                &mut tokio::io::empty(),
                3,
                None,
                Instant::now() + std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(1),
            )
            .await,
            Err("the computer did not answer")
        );
    }

    #[tokio::test]
    async fn an_answer_eof_after_the_upload_is_unanswered() {
        let (answer, mut recv) = tokio::io::duplex(64);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            drop(answer);
        });
        assert_eq!(
            exchange_file(
                &mut &b"abc"[..],
                &mut tokio::io::sink(),
                &mut recv,
                3,
                None,
                Instant::now() + std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(1),
            )
            .await,
            Err("the computer did not answer")
        );
    }

    #[tokio::test]
    async fn an_invalid_answer_after_the_upload_is_unanswered() {
        use tokio::io::AsyncWriteExt;
        let (mut answer, mut recv) = tokio::io::duplex(64);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            answer.write_all(b"\0\0\0\x01?").await.unwrap();
            drop(answer);
        });
        assert_eq!(
            exchange_file(
                &mut &b"abc"[..],
                &mut tokio::io::sink(),
                &mut recv,
                3,
                None,
                Instant::now() + std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(1),
            )
            .await,
            Err("the computer did not answer")
        );
    }

    #[tokio::test]
    async fn a_short_source_reports_it_changed() {
        use tokio::io::duplex;
        let (mut send, _body) = duplex(128);
        let (_answer, mut recv) = duplex(128);
        assert_eq!(
            exchange_file(
                &mut &b"ab"[..],
                &mut send,
                &mut recv,
                3,
                None,
                Instant::now() + std::time::Duration::from_secs(5),
                std::time::Duration::from_secs(5)
            )
            .await,
            Err("the file changed while it was sent")
        );
    }

    #[tokio::test]
    async fn a_long_source_reports_it_changed() {
        use tokio::io::duplex;
        let (mut send, _body) = duplex(128);
        let (_answer, mut recv) = duplex(128);
        assert_eq!(
            timeout(
                std::time::Duration::from_millis(50),
                exchange_file(
                    &mut &b"abcd"[..],
                    &mut send,
                    &mut recv,
                    3,
                    None,
                    Instant::now() + std::time::Duration::from_secs(5),
                    std::time::Duration::from_secs(5)
                )
            )
            .await,
            Ok(Err("the file changed while it was sent"))
        );
    }

    #[tokio::test]
    async fn the_answer_has_its_own_budget_after_upload() {
        use tokio::io::{AsyncWriteExt, duplex};
        let (mut send, _body) = duplex(128);
        let (mut answer, mut recv) = duplex(128);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(70)).await;
            frame::write(&mut answer, &Answer::Ok, CONTROL_CAP)
                .await
                .unwrap();
            answer.shutdown().await.unwrap();
        });
        assert_eq!(
            exchange_file(
                &mut &b"abc"[..],
                &mut send,
                &mut recv,
                3,
                None,
                Instant::now() + std::time::Duration::from_millis(30),
                std::time::Duration::from_millis(150)
            )
            .await,
            Ok(())
        );
    }

    #[tokio::test]
    async fn a_complete_body_finishes_outside_the_file_deadline() {
        let worker = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            Ok(())
        });
        let feed = async {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            Ok(())
        };
        assert_eq!(
            finish_file(
                worker,
                feed,
                Instant::now() + std::time::Duration::from_millis(30)
            )
            .await,
            Ok(())
        );
    }

    #[tokio::test]
    async fn a_body_read_stall_is_bad_body() {
        let (_send, mut recv) = tokio::io::duplex(128);
        let (sender, _chunks) = mpsc::channel(2);
        assert_eq!(
            timeout(
                std::time::Duration::from_millis(200),
                feed_file(&mut recv, sender, 1, std::time::Duration::from_millis(20))
            )
            .await,
            Ok(Err(BAD_BODY))
        );
    }

    #[tokio::test]
    async fn a_timed_out_body_releases_the_blocking_writer() {
        let (sender, mut chunks) = mpsc::channel::<Vec<u8>>(2);
        let worker = tokio::task::spawn_blocking(move || {
            assert!(chunks.blocking_recv().is_none());
            Err(BAD_BODY)
        });
        let feed = async move {
            let _sender = sender;
            std::future::pending::<Result<(), &'static str>>().await
        };
        assert_eq!(
            timeout(
                std::time::Duration::from_millis(200),
                finish_file(
                    worker,
                    feed,
                    Instant::now() + std::time::Duration::from_millis(20)
                )
            )
            .await,
            Ok(Err(BAD_BODY))
        );
    }

    #[test]
    fn refusal_sentences_are_exact() {
        for (reason, expected) in [
            ("no_folder", "that app takes no files on that computer"),
            ("exists", "a file with that name is already there"),
            ("busy", "that computer is busy; try again"),
            ("folder", "that app's folder on that computer is not usable"),
            ("write", "that computer could not write the file"),
            ("full", "that app's folder on that computer is full"),
            ("space", "that computer has no room for the file"),
            ("other", "the computer did not answer"),
        ] {
            assert_eq!(file_refusal(reason), expected);
        }
    }
}
