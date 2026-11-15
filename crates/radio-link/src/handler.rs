use crate::EndpointId;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Instant,
};

/// Whether handler stdout is safe to return as a message reply.
pub fn valid_reply(reply: &str) -> bool {
    reply.len() <= 4096
        && reply.lines().count() <= 8
        && reply
            .chars()
            .all(|c| c == '\n' || c == '\t' || (!c.is_control() && !is_invisible(c)))
}

// Matches the invisible-character list in radio-rooms/src/text.rs.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}' | '\u{061C}' | '\u{115F}'..='\u{1160}' | '\u{180E}'
            | '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}' | '\u{FFA0}' | '\u{FFF9}'..='\u{FFFB}'
            | '\u{1D173}'..='\u{1D17A}' | '\u{E0000}'..='\u{E007F}'
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub reply: String,
    pub exit: i32,
}

pub struct Envelope<'a> {
    pub app: &'a str,
    pub from: EndpointId,
    pub from_name: Option<&'a str>,
    pub id: &'a str,
    pub body: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Ran {
    Reply { reply: String, exit: i32 },
    NoHandler,
    Timeout,
    BadReply,
}

pub trait Handlers: Send + Sync + 'static {
    fn has_handler(&self, app: &str) -> bool;
    fn run(&self, envelope: &Envelope<'_>) -> Ran;
    fn refused(&self, from: &EndpointId, from_name: Option<&str>, app: &str, reason: &str);
}

pub struct NoHandlers;

impl Handlers for NoHandlers {
    fn has_handler(&self, _app: &str) -> bool {
        false
    }
    fn run(&self, _envelope: &Envelope<'_>) -> Ran {
        Ran::NoHandler
    }
    fn refused(&self, _from: &EndpointId, _from_name: Option<&str>, _app: &str, _reason: &str) {}
}

pub(crate) struct Messages {
    pub(crate) handlers: Arc<dyn Handlers>,
    senders: Mutex<HashMap<EndpointId, Sender>>,
    slots: Arc<Semaphore>,
    not_members: Mutex<NotMembers>,
}

#[derive(Default)]
struct NotMembers {
    logged: VecDeque<(EndpointId, Instant)>,
}

impl NotMembers {
    fn allow(&mut self, key: EndpointId) -> bool {
        let now = Instant::now();
        while self
            .logged
            .front()
            .is_some_and(|(_, at)| now.duration_since(*at) >= Duration::from_secs(60))
        {
            self.logged.pop_front();
        }
        if self.logged.len() >= 60 || self.logged.iter().any(|(logged, _)| *logged == key) {
            return false;
        }
        self.logged.push_back((key, now));
        true
    }
}

struct Sender {
    seen: HashMap<String, Instant>,
    slot: Arc<Semaphore>,
}

impl Messages {
    pub(crate) fn new(handlers: Arc<dyn Handlers>) -> Self {
        Self {
            handlers,
            senders: Mutex::new(HashMap::new()),
            slots: Arc::new(Semaphore::new(4)),
            not_members: Mutex::new(NotMembers::default()),
        }
    }

    pub(crate) fn check(
        &self,
        envelope: &Envelope<'_>,
        sent: u64,
        now: u64,
        recorded: Instant,
    ) -> Result<(), &'static str> {
        use crate::wire;
        if envelope.body.len() > 8192 {
            return Err(wire::TOO_BIG);
        }
        if sent.abs_diff(now) > 120_000 {
            return Err(wire::CLOCK);
        }
        if !wire::message_id(envelope.id) {
            return Err("bad_frame");
        }
        let mut senders = self.senders.lock().map_err(|_| wire::BUSY)?;
        senders.retain(|_, sender| {
            sender.seen.retain(|_, expires| *expires > recorded);
            !sender.seen.is_empty() || sender.slot.available_permits() == 0
        });
        let sender = senders.entry(envelope.from).or_insert_with(|| Sender {
            seen: HashMap::new(),
            slot: Arc::new(Semaphore::new(1)),
        });
        if sender.seen.contains_key(envelope.id) {
            return Err(wire::REPLAY);
        }
        if sender.seen.len() >= 1024 {
            return Err(wire::BUSY);
        }
        sender
            .seen
            .insert(envelope.id.into(), recorded + Duration::from_secs(300));
        Ok(())
    }

    pub(crate) fn reserve(
        &self,
        from: EndpointId,
    ) -> Result<(OwnedSemaphorePermit, OwnedSemaphorePermit), &'static str> {
        let mut senders = self.senders.lock().map_err(|_| crate::wire::BUSY)?;
        let sender = senders.entry(from).or_insert_with(|| Sender {
            seen: HashMap::new(),
            slot: Arc::new(Semaphore::new(1)),
        });
        let sender_slot = sender
            .slot
            .clone()
            .try_acquire_owned()
            .map_err(|_| crate::wire::BUSY)?;
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| crate::wire::BUSY)?;
        Ok((sender_slot, slot))
    }

    pub(crate) fn audit(&self, from: &EndpointId, name: Option<&str>, app: &str, reason: &str) {
        if reason == "not_member" && !self.not_members.lock().unwrap().allow(*from) {
            return;
        }
        let app = if crate::wire::app_name(app) { app } else { "" };
        self.handlers.refused(from, name, app, reason);
    }

    pub(crate) fn refused(&self, envelope: &Envelope<'_>, reason: &str) -> crate::wire::Answer {
        self.audit(&envelope.from, envelope.from_name, envelope.app, reason);
        crate::wire::Answer::Refused {
            reason: reason.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn envelope<'a>(key: EndpointId, id: &'a str, body: &'a str) -> Envelope<'a> {
        Envelope {
            app: "notes",
            from: key,
            from_name: Some("garden"),
            id,
            body,
        }
    }
    #[test]
    fn replies_reject_oversized_and_unsafe_text() {
        assert!(valid_reply(&"x".repeat(4096)));
        assert!(valid_reply("1\n2\n3\n4\n5\n6\n7\n8\n"));
        assert!(valid_reply("hello\tworld\n"));
        assert!(!valid_reply("\u{1b}[2J"));
        assert!(!valid_reply(&"x".repeat(4097)));
        assert!(!valid_reply("1\n2\n3\n4\n5\n6\n7\n8\n9"));
    }

    #[test]
    fn replies_reject_invisible_and_format_characters() {
        for c in ['\u{202e}', '\u{2066}', '\u{200b}', '\u{feff}'] {
            assert!(!valid_reply(&format!("hello{c}world")), "{c:?}");
        }
    }

    #[test]
    fn refusal_audit_drops_invalid_app_names() {
        struct Audit(Mutex<Vec<String>>);
        impl Handlers for Audit {
            fn has_handler(&self, _app: &str) -> bool {
                false
            }
            fn run(&self, _envelope: &Envelope<'_>) -> Ran {
                Ran::NoHandler
            }
            fn refused(&self, _key: &EndpointId, _name: Option<&str>, app: &str, _reason: &str) {
                self.0.lock().unwrap().push(app.into());
            }
        }
        let audit = Arc::new(Audit(Mutex::new(Vec::new())));
        let state = Messages::new(audit.clone());
        let key = crate::SecretKey::generate().public();
        let app = "x".repeat(60_000);
        let mut request = envelope(key, "", "");
        request.app = &app;
        state.refused(&request, "not_for_us");
        assert!(audit.0.lock().unwrap()[0].is_empty());
        request.app = "notes";
        state.refused(&request, "clock");
        assert_eq!(audit.0.lock().unwrap()[1], "notes");
    }

    #[test]
    fn envelope_checks_run_in_order_and_keep_exact_boundaries() {
        let state = Messages::new(Arc::new(NoHandlers));
        let key = crate::SecretKey::generate().public();
        let instant = Instant::now();
        assert_eq!(
            state.check(
                &envelope(key, "bad", &"x".repeat(8193)),
                0,
                500_000,
                instant
            ),
            Err("too_big")
        );
        assert_eq!(
            state.check(&envelope(key, "bad", "x"), 0, 500_000, instant),
            Err("clock")
        );
        assert_eq!(
            state.check(&envelope(key, "bad", "x"), 500_000, 500_000, instant),
            Err("bad_frame")
        );
        for (index, sent) in [380_000, 620_000].into_iter().enumerate() {
            assert_eq!(
                state.check(
                    &envelope(key, &format!("{index:032x}"), &"x".repeat(8192)),
                    sent,
                    500_000,
                    instant
                ),
                Ok(())
            );
        }
        for sent in [379_999, 620_001] {
            assert_eq!(
                state.check(&envelope(key, &"a".repeat(32), "x"), sent, 500_000, instant),
                Err("clock")
            );
        }
    }
    #[test]
    fn replay_is_per_sender_capped_and_expires_without_eviction() {
        let state = Messages::new(Arc::new(NoHandlers));
        let first = crate::SecretKey::generate().public();
        let second = crate::SecretKey::generate().public();
        let instant = Instant::now();
        for id in 0..1024 {
            assert_eq!(
                state.check(&envelope(first, &format!("{id:032x}"), "x"), 0, 0, instant),
                Ok(())
            );
        }
        assert_eq!(
            state.check(
                &envelope(first, &format!("{:032x}", 1024), "x"),
                0,
                0,
                instant
            ),
            Err("busy")
        );
        assert_eq!(
            state.check(&envelope(first, &format!("{:032x}", 0), "x"), 0, 0, instant),
            Err("replay")
        );
        assert_eq!(
            state.check(
                &envelope(second, &format!("{:032x}", 0), "x"),
                0,
                0,
                instant
            ),
            Ok(())
        );
        assert_eq!(
            state.check(
                &envelope(first, &format!("{:032x}", 1024), "x"),
                0,
                0,
                instant + Duration::from_secs(299)
            ),
            Err("busy")
        );
        assert_eq!(
            state.check(
                &envelope(first, &format!("{:032x}", 1024), "x"),
                0,
                0,
                instant + Duration::from_secs(300)
            ),
            Ok(())
        );
    }
    #[test]
    fn handler_slots_allow_four_senders_and_one_run_per_sender() {
        let state = Messages::new(Arc::new(NoHandlers));
        let keys: Vec<_> = (0..5)
            .map(|_| crate::SecretKey::generate().public())
            .collect();
        let mut held = Vec::new();
        for key in &keys[..4] {
            held.push(state.reserve(*key).unwrap());
        }
        assert!(matches!(state.reserve(keys[4]), Err("busy")));
        assert!(matches!(state.reserve(keys[0]), Err("busy")));
        held.pop();
        let replacement = state.reserve(keys[4]).unwrap();
        drop(held);
        drop(replacement);
        assert!(state.reserve(keys[0]).is_ok());
    }
}
