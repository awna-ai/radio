use crate::{
    EndpointId, SecretKey,
    admit::{Admission, Gate, Limits, Standing},
    endpoint::{self, ALPN, JOIN_ALPN},
    frame::{self, CONTROL_CAP},
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
const UNANSWERED: &str = "the computer did not answer";

#[derive(Clone)]
pub struct Local {
    pub name: String,
    pub sha: String,
}

#[derive(Debug)]
pub enum Event {
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
                                tokio::select! {
                                    _ = connection.closed() => {},
                                    _ = events.send(Event::Join {
                                        from: connection.remote_id(), connection: connection.clone(),
                                    }) => {},
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
    let mut streams = JoinSet::new();
    let mut idle_at = Instant::now() + gate.limits.idle;
    loop {
        tokio::select! {
            stream = connection.accept_bi() => {
                let Ok((send, recv)) = stream else { break };
                let started = Instant::now();
                if gate.admission.standing(&connection.remote_id()) != Standing::Accepted {
                    connection.close(REFUSED.into(), &[]);
                    break;
                }
                let connection = connection.clone();
                let gate = gate.clone();
                let local = local.clone();
                let events = events.clone();
                streams.spawn(async move {
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
    let deadline = started + gate.limits.frame;
    let result = timeout_at(deadline, async {
        let request = frame::read::<Request, _>(&mut recv, CONTROL_CAP).await?;
        let from = connection.remote_id();
        match request {
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
                frame::write(&mut send, &answer, CONTROL_CAP).await?;
                send.finish().map_err(|_| UNANSWERED)?;
                events
                    .send(Event::Hello { from, hello })
                    .await
                    .map_err(|_| UNANSWERED)?;
            }
            Request::App { to, app, body } => {
                if !wire::valid_app(&to, &app, &id) {
                    return Err(UNANSWERED);
                }
                events
                    .send(Event::App { from, app, body })
                    .await
                    .map_err(|_| UNANSWERED)?;
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
