use std::sync::Mutex;

use iroh::endpoint::{Connection, ConnectionError};
use tokio::time::{Instant, timeout, timeout_at};

use crate::{
    SecretKey,
    code::JoinCode,
    computer_record::ComputerRecord,
    fleet_db::{Applied, Computer, FleetDb, Recipient},
    frame::{self, CONTROL_CAP, RECORD_CAP},
    link::{Link, REFUSED},
    membership::{JoinAnswer, RecordBody, SignedRecord},
    wire::JoinRequest,
};

const READ: &str = "fleet.db could not be read";
const UNANSWERED: &str = "the computer did not answer";

pub async fn join(
    link: &Link,
    key: &SecretKey,
    db: &Mutex<FleetDb>,
    code: &JoinCode,
    now: f64,
) -> Result<(), &'static str> {
    if key.public() != link.id() {
        return Err("the key is not this link's");
    }
    if db.lock().map_err(|_| READ)?.fleet()?.is_some() {
        return Err("already in a fleet");
    }
    if code.hints.direct.is_empty() {
        return Err("the join code has no address to dial");
    }
    let connection = link.dial_join(code.inviter, &code.hints.direct).await?;
    let result = timeout(link.limits().frame, async {
        let (mut send, mut recv) = connection.open_bi().await.map_err(|_| UNANSWERED)?;
        frame::write(
            &mut send,
            &JoinRequest {
                code: code.encode(),
                consent: crate::membership::consent(
                    key,
                    &code.fleet,
                    &code.inviter.to_string(),
                    link.name(),
                    &link.addrs(),
                ),
                name: link.name().into(),
                addrs: link.addrs(),
            },
            CONTROL_CAP,
        )
        .await?;
        send.finish().map_err(|_| UNANSWERED)?;
        frame::read::<JoinAnswer, _>(&mut recv, RECORD_CAP).await
    })
    .await;
    let answer = match result {
        Ok(Ok(answer)) => Ok(answer),
        _ => match connection.close_reason() {
            Some(ConnectionError::ApplicationClosed(close))
                if close.error_code == REFUSED.into() =>
            {
                Err("the join was refused")
            }
            _ => Err(UNANSWERED),
        },
    };
    connection.close(0u32.into(), &[]);
    let answer = answer?;
    db.lock()
        .map_err(|_| READ)?
        .joined(&link.id(), code, &answer, now)
}

pub async fn answer(
    link: &Link,
    key: &SecretKey,
    db: &Mutex<FleetDb>,
    connection: Connection,
    now: f64,
) -> Result<Option<SignedRecord>, &'static str> {
    if key.public() != link.id() {
        return Err("the key is not this link's");
    }
    let deadline = Instant::now() + link.limits().frame;
    let result = timeout_at(deadline, async {
        let (send, mut recv) = connection.accept_bi().await.map_err(|_| UNANSWERED)?;
        let request: JoinRequest = frame::read(&mut recv, CONTROL_CAP).await?;
        let code = JoinCode::parse(&request.code)?;
        let (answer, introduction) = db.lock().map_err(|_| READ)?.admit(
            key,
            link.name(),
            &link.addrs(),
            &code,
            &connection.remote_id(),
            &request.name,
            &request.addrs,
            &request.consent,
            now,
        )?;
        Ok((send, answer, introduction))
    })
    .await
    .unwrap_or(Err(UNANSWERED));
    let (mut send, answer, introduction) = match result {
        Ok(admitted) => admitted,
        Err(error) => {
            connection.close(REFUSED.into(), &[]);
            return Err(error);
        }
    };
    let _ = timeout_at(deadline, async {
        frame::write(&mut send, &answer, RECORD_CAP).await?;
        send.finish().map_err(|_| UNANSWERED)?;
        let _ = send.stopped().await;
        Ok::<(), &'static str>(())
    })
    .await;
    Ok(introduction)
}

pub async fn deliver(link: &Link, record: &SignedRecord, to: &[Recipient]) -> usize {
    let mut delivered = 0;
    for (key, addrs) in to {
        if let Ok(peer) = link.dial(*key, addrs).await
            && peer.send_record(record).await.is_ok()
        {
            delivered += 1;
        }
    }
    delivered
}

pub async fn introduce(link: &Link, db: &Mutex<FleetDb>, record: &SignedRecord) -> usize {
    let RecordBody::Introduction { newcomer, .. } = &record.body else {
        return 0;
    };
    let Some(newcomer) = crate::fleet::public_key(newcomer) else {
        return 0;
    };
    let recipients = match db.lock() {
        Ok(db) => db.recipients(&newcomer),
        Err(_) => Err(READ),
    };
    match recipients {
        Ok(to) => deliver(link, record, &to).await,
        Err(_) => 0,
    }
}

pub fn apply(
    link: &Link,
    db: &Mutex<FleetDb>,
    record: &SignedRecord,
    now: f64,
) -> Result<Applied, &'static str> {
    let applied = db
        .lock()
        .map_err(|_| READ)?
        .apply(&link.id(), link.name(), record, now)?;
    match &applied {
        Applied::Removed(key) => link.close_key(key),
        Applied::LeftFleet => link.close_all(),
        _ => {}
    }
    Ok(applied)
}

pub fn block(link: &Link, db: &Mutex<FleetDb>, target: &str) -> Result<Computer, &'static str> {
    let computer = db.lock().map_err(|_| READ)?.block(target)?;
    let key = crate::fleet::public_key(&computer.key).ok_or(READ)?;
    link.close_key(&key);
    Ok(computer)
}

pub async fn remove(
    link: &Link,
    key: &SecretKey,
    db: &Mutex<FleetDb>,
    target: &str,
    now: f64,
) -> Result<usize, &'static str> {
    let computer = db.lock().map_err(|_| READ)?.removable(&link.id(), target)?;
    let subject = crate::fleet::public_key(&computer.key).ok_or(READ)?;
    let peer = if computer.addrs.is_empty() {
        None
    } else {
        link.dial(subject, &computer.addrs).await.ok()
    };
    let (record, to) = {
        let mut db = db.lock().map_err(|_| READ)?;
        let (record, _) = db.remove(key, &computer.key, now)?;
        let to = db.recipients(&subject)?;
        (record, to)
    };
    let delivered = match peer {
        Some(peer) => usize::from(peer.send_record(&record).await.is_ok()),
        None => 0,
    };
    link.close_key(&subject);
    Ok(delivered + deliver(link, &record, &to).await)
}

pub async fn leave(
    link: &Link,
    key: &SecretKey,
    db: &Mutex<FleetDb>,
    now: f64,
) -> Result<usize, &'static str> {
    let (record, to) = db.lock().map_err(|_| READ)?.leave(key, now)?;
    let delivered = deliver(link, &record, &to).await;
    db.lock().map_err(|_| READ)?.forget()?;
    link.close_all();
    Ok(delivered)
}

pub async fn publish(link: &Link, db: &Mutex<FleetDb>, record: &ComputerRecord) -> usize {
    let recipients = match db.lock() {
        Ok(db) => db.recipients(&link.id()),
        Err(_) => Err(READ),
    };
    let Ok(recipients) = recipients else { return 0 };
    let mut delivered = 0;
    for (key, addrs) in recipients {
        if let Ok(peer) = link.dial(key, &addrs).await
            && peer.send_computer(record).await.is_ok()
        {
            delivered += 1;
        }
    }
    delivered
}

pub fn apply_computer(
    link: &Link,
    db: &Mutex<FleetDb>,
    record: &ComputerRecord,
) -> Result<(), &'static str> {
    db.lock()
        .map_err(|_| READ)?
        .apply_computer(&link.id(), record)
}
