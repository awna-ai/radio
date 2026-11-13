use radio_link::{
    code::{CODE_MAX_CHARS, Hints, JoinCode},
    computer_record::{
        self, APPS_MAX, COMPUTER_RECORD_MAX, ComputerBody, ComputerRecord, ROLES_MAX, SECTION_MAX,
    },
    fleet::{self, FleetBody, FleetRecord, Signer},
    frame::{self, CONTROL_CAP, RECORD_CAP},
    membership::{self, JoinAnswer, RecordBody, SignedRecord},
    wire::{Answer, Hello, JoinRequest, Protocol, Request},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, ReadBuf};

struct Gen(u64);
impl Gen {
    fn u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0);
        self.u64() % n
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.u64() as u8).collect()
    }
    fn array32(&mut self) -> [u8; 32] {
        std::array::from_fn(|_| self.u64() as u8)
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

fn settings(default_seed: u64) -> (u64, usize) {
    fn decimal(variable: &str, default: u64, message: &str) -> u64 {
        match std::env::var(variable) {
            Err(std::env::VarError::NotPresent) => default,
            Ok(text) if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) => {
                text.parse().unwrap_or_else(|_| panic!("{message}"))
            }
            _ => panic!("{message}"),
        }
    }
    let seed = decimal(
        "RADIO_PROPERTY_SEED",
        default_seed,
        "RADIO_PROPERTY_SEED must be a decimal u64",
    );
    let cases = decimal(
        "RADIO_PROPERTY_CASES",
        256,
        "RADIO_PROPERTY_CASES must be 1 to 1000000",
    );
    assert!(
        (1..=1_000_000).contains(&cases),
        "RADIO_PROPERTY_CASES must be 1 to 1000000"
    );
    (seed, cases as usize)
}

fn run(name: &str, default_seed: u64, mut property: impl FnMut(&mut Gen, usize)) {
    let (seed, cases) = settings(default_seed);
    for i in 0..cases {
        let mut g = Gen(seed ^ (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        if let Err(error) = catch_unwind(AssertUnwindSafe(|| property(&mut g, i))) {
            let message = error
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| error.downcast_ref::<&str>().copied())
                .unwrap_or("panic");
            panic!("{name} seed {seed} case {i}: {message}");
        }
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}
fn key(g: &mut Gen) -> iroh::SecretKey {
    iroh::SecretKey::from_bytes(&g.array32())
}
fn hex64(g: &mut Gen) -> String {
    g.array32().iter().map(|b| format!("{b:02x}")).collect()
}
fn name(g: &mut Gen) -> String {
    let len = 1 + g.below(32) as usize;
    let mut value = String::from((b'a' + g.below(26) as u8) as char);
    for _ in 1..len {
        value.push(*g.pick(b"abcdefghijklmnopqrstuvwxyz0123456789-") as char);
    }
    value
}
fn app(g: &mut Gen) -> String {
    let len = 1 + g.below(64) as usize;
    let mut value = String::from(*g.pick(b"abcdefghijklmnopqrstuvwxyz0123456789") as char);
    for _ in 1..len {
        value.push(*g.pick(b"abcdefghijklmnopqrstuvwxyz0123456789._-") as char);
    }
    value
}
fn text(g: &mut Gen, max: usize) -> String {
    let len = g.below(max as u64 + 1) as usize;
    (0..len)
        .map(|_| char::from_u32(g.below(0x110000) as u32).unwrap_or('\0'))
        .collect()
}
fn section(g: &mut Gen) -> String {
    let max = if g.chance(90) { 64 } else { SECTION_MAX };
    let limit = 1 + g.below(max as u64) as usize;
    let mut value = String::new();
    while value.len() < limit {
        let c = match g.below(7) {
            0 => (b'a' + g.below(26) as u8) as char,
            1 => '"',
            2 => '\\',
            3 => char::from(g.below(32) as u8),
            4 => '\u{7f}',
            5 => 'é',
            _ => *g.pick(&['你', '🙂']),
        };
        if value.len() + c.len_utf8() <= limit {
            value.push(c);
        } else {
            value.push('a');
        }
    }
    value
}
fn addrs(g: &mut Gen) -> Vec<SocketAddr> {
    let port = (1 + g.below(65535)) as u16;
    let v4 = SocketAddr::from((Ipv4Addr::new(192, 0, 2, 1 + g.below(254) as u8), port));
    let v6 = SocketAddr::from((
        Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1 + g.below(65535) as u16),
        port,
    ));
    match g.below(4) {
        0 => vec![],
        1 => vec![v4],
        2 => vec![v6],
        _ => vec![v4, v6],
    }
}
fn computer_body(g: &mut Gen, node: &iroh::SecretKey) -> ComputerBody {
    let mut roles = BTreeSet::new();
    let count = g.below(ROLES_MAX as u64 + 1) as usize;
    while roles.len() < count {
        roles.insert(name(g));
    }
    let mut apps = BTreeMap::new();
    let count = g.below(APPS_MAX as u64 + 1) as usize;
    while apps.len() < count {
        apps.insert(app(g), section(g));
    }
    ComputerBody {
        fleet: hex64(g),
        key: node.public().to_string(),
        version: 1 + g.below(i64::MAX as u64),
        name: name(g),
        roles: roles.into_iter().collect(),
        addrs: addrs(g),
        apps,
    }
}
fn small_computer(g: &mut Gen, node: &iroh::SecretKey) -> ComputerRecord {
    let mut body = computer_body(g, node);
    for value in body.apps.values_mut() {
        value.truncate(value.char_indices().nth(8).map_or(value.len(), |(i, _)| i));
    }
    computer_record::sign(body, node)
}
fn addrs_for_introduction(g: &mut Gen) -> Vec<SocketAddr> {
    let mut addresses = addrs(g);
    if addresses.is_empty() {
        addresses.push("192.0.2.1:1".parse().unwrap());
    }
    addresses
}
fn introduction(g: &mut Gen, inviter: &iroh::SecretKey) -> SignedRecord {
    let newcomer = key(g);
    introduction_with(g, inviter, &newcomer)
}
fn introduction_with(
    g: &mut Gen,
    inviter: &iroh::SecretKey,
    newcomer: &iroh::SecretKey,
) -> SignedRecord {
    let fleet = hex64(g);
    let name = name(g);
    let addresses = addrs_for_introduction(g);
    let inviter_id = inviter.public().to_string();
    let consent = membership::consent(newcomer, &fleet, &inviter_id, &name, &addresses);
    membership::sign(
        RecordBody::Introduction {
            fleet,
            inviter: inviter_id,
            counter: 1 + g.below(u64::MAX),
            newcomer: newcomer.public().to_string(),
            name,
            addrs: addresses,
            consent,
        },
        inviter,
    )
}
fn removal(g: &mut Gen, node: &iroh::SecretKey) -> SignedRecord {
    membership::sign(
        RecordBody::Removal {
            fleet: hex64(g),
            signer: node.public().to_string(),
            counter: 1 + g.below(u64::MAX),
            subject: key(g).public().to_string(),
        },
        node,
    )
}
fn fleet_record(
    g: &mut Gen,
    founder: &iroh::SecretKey,
    recovery: &iroh::SecretKey,
    signer: Signer,
) -> FleetRecord {
    fleet::sign(
        FleetBody {
            id: hex64(g),
            name: name(g),
            version: 1 + g.below(u64::MAX),
            epoch: g.u64(),
            founder: founder.public().to_string(),
            recovery: recovery.public().to_string(),
            release_key: if g.chance(50) {
                Some(key(g).public().to_string())
            } else {
                None
            },
            relays: vec![],
        },
        signer,
        match signer {
            Signer::Founder => founder,
            Signer::Recovery => recovery,
        },
    )
}
fn hello(g: &mut Gen) -> Hello {
    Hello {
        protocol: Protocol {
            major: g.u64() as u32,
            minor: g.u64() as u32,
        },
        fleet: text(g, 64),
        sha: text(g, 64),
        arch: text(g, 32),
        name: text(g, 32),
    }
}
fn request(g: &mut Gen) -> Request {
    match g.below(4) {
        0 => {
            let node = key(g);
            Request::Computer {
                record: small_computer(g, &node),
            }
        }
        1 => {
            let node = key(g);
            Request::Record {
                record: if g.chance(50) {
                    introduction(g, &node)
                } else {
                    removal(g, &node)
                },
            }
        }
        2 => Request::Hello { hello: hello(g) },
        _ => Request::App {
            to: text(g, 64),
            app: text(g, 64),
            body: text(g, 256),
        },
    }
}

fn frame_body(g: &mut Gen) -> Vec<u8> {
    match g.below(4) {
        0 => serde_json::to_vec(&request(g)).unwrap(),
        1 => serde_json::to_vec(&if g.chance(50) {
            Answer::Hello { hello: hello(g) }
        } else {
            Answer::Ok
        })
        .unwrap(),
        2 => serde_json::to_vec(&JoinRequest {
            code: text(g, 128),
            consent: text(g, 128),
            name: text(g, 64),
            addrs: addrs(g),
        })
        .unwrap(),
        _ => {
            let node = key(g);
            let answer = JoinAnswer {
                fleet: fleet::found(&node, &name(g)).unwrap().0,
                computers: vec![],
                removed: vec![],
                records: (0..g.below(3)).map(|_| small_computer(g, &node)).collect(),
            };
            let bytes = serde_json::to_vec(&answer).unwrap();
            assert!(bytes.len() <= RECORD_CAP);
            bytes
        }
    }
}

struct Chunks {
    bytes: Vec<u8>,
    at: usize,
    sizes: Vec<usize>,
    served: usize,
    pending: Vec<bool>,
    yielded: bool,
    pendings: usize,
}
impl Chunks {
    fn new(bytes: Vec<u8>, g: &mut Gen) -> Self {
        let count = 1 + g.below(8) as usize;
        Self {
            bytes,
            at: 0,
            sizes: (0..count).map(|_| 1 + g.below(64) as usize).collect(),
            served: 0,
            pending: (0..1 + g.below(8)).map(|_| g.chance(30)).collect(),
            yielded: false,
            pendings: 0,
        }
    }
}
impl AsyncRead for Chunks {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.pending[self.at % self.pending.len()] && !self.yielded {
            self.yielded = true;
            self.pendings += 1;
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        self.yielded = false;
        let n = self.sizes[self.at % self.sizes.len()]
            .min(buf.remaining())
            .min(self.bytes.len() - self.served);
        buf.put_slice(&self.bytes[self.served..self.served + n]);
        self.at += 1;
        self.served += n;
        Poll::Ready(Ok(()))
    }
}
fn mutate(g: &mut Gen, bytes: &mut Vec<u8>, count: usize) {
    for _ in 0..count {
        let at = g.below(bytes.len() as u64 + 1) as usize;
        match g.below(5) {
            0 if at < bytes.len() => bytes[at] ^= 1 << g.below(8),
            1 => bytes.insert(at, g.u64() as u8),
            2 if at < bytes.len() => {
                bytes.remove(at);
            }
            3 => bytes.truncate(at),
            _ => bytes.push(g.u64() as u8),
        }
    }
}
macro_rules! round_trip {
    ($value:expr, $ty:ty, $cap:expr, $g:expr) => {{
        let value = $value;
        let size = serde_json::to_vec(&value).unwrap().len();
        let mut bytes = vec![];
        let result = frame::write(&mut bytes, &value, $cap).await;
        if size > $cap {
            assert_eq!(result, Err("the frame is too large"));
            assert!(bytes.is_empty());
        } else {
            assert_eq!(result, Ok(()));
            assert_eq!(bytes.len(), 4 + size);
            let decoded = frame::read::<$ty, _>(&mut Chunks::new(bytes, $g), $cap)
                .await
                .unwrap();
            assert!(decoded == value);
        }
    }};
}

#[test]
fn frame_decoder_is_total_on_any_bytes() {
    let rt = runtime();
    let mut pendings = 0;
    let mut decoded = [0; 4];
    run("frame_decoder_is_total_on_any_bytes", 0x301, |g, _| {
        rt.block_on(async {
            let bytes = match g.below(3) {
                0 => {
                    let len = g.below(65) as usize;
                    g.bytes(len)
                }
                1 => {
                    let cap = *g.pick(&[CONTROL_CAP, RECORD_CAP]);
                    let body = if g.chance(50) {
                        frame_body(g)
                    } else {
                        let len = g.below(513) as usize;
                        g.bytes(len)
                    };
                    let len = body.len() as u32;
                    let random = g.u64() as u32;
                    let prefix = *g.pick(&[
                        0,
                        1,
                        cap as u32,
                        cap as u32 + 1,
                        u32::MAX,
                        random,
                        len,
                        len.saturating_sub(1),
                        len + 1,
                    ]);
                    let mut bytes = prefix.to_be_bytes().to_vec();
                    bytes.extend(body);
                    bytes
                }
                _ => {
                    let body = frame_body(g);
                    let mut bytes = (body.len() as u32).to_be_bytes().to_vec();
                    bytes.extend(body);
                    if g.chance(50) {
                        let count = 1 + g.below(4) as usize;
                        mutate(g, &mut bytes, count);
                    }
                    bytes
                }
            };
            macro_rules! decode {
                ($ty:ty, $cap:expr, $index:expr) => {{
                    let mut reader = Chunks::new(bytes.clone(), g);
                    let result = tokio::time::timeout(
                        std::time::Duration::from_secs(1),
                        frame::read::<$ty, _>(&mut reader, $cap),
                    )
                    .await;
                    pendings += reader.pendings;
                    assert!(result.is_ok(), "decoder timed out");
                    match result.unwrap() {
                        Ok(value) => {
                            decoded[$index] += 1;
                            let length = u32::from_be_bytes(bytes[..4].try_into().unwrap());
                            assert_eq!(bytes.len(), 4 + length as usize, "decoded past its frame");
                            round_trip!(value, $ty, $cap, g);
                        }
                        Err(error) => assert_eq!(error, "the frame is not valid"),
                    }
                }};
            }
            decode!(Request, CONTROL_CAP, 0);
            decode!(Answer, CONTROL_CAP, 1);
            decode!(JoinRequest, CONTROL_CAP, 2);
            decode!(JoinAnswer, RECORD_CAP, 3);
        });
    });
    let (seed, cases) = settings(0x301);
    assert!(
        pendings > 0,
        "frame_decoder_is_total_on_any_bytes seed {seed} case {}: pending polls {pendings}",
        cases - 1
    );
    assert!(
        decoded.into_iter().all(|count| count > 0),
        "frame_decoder_is_total_on_any_bytes seed {seed} case {}: decoded types {decoded:?}",
        cases - 1
    );
}

#[test]
fn over_cap_prefixes_read_only_the_prefix() {
    let rt = runtime();
    run("over_cap_prefixes_read_only_the_prefix", 0x302, |g, _| {
        rt.block_on(async {
            for cap in [CONTROL_CAP, RECORD_CAP] {
                let length = if g.chance(50) {
                    0
                } else {
                    cap as u32 + 1 + g.below(u32::MAX as u64 - cap as u64) as u32
                };
                let mut bytes = length.to_be_bytes().to_vec();
                bytes.extend(g.bytes(64));
                let mut reader = Chunks::new(bytes, g);
                assert_eq!(
                    frame::read::<Request, _>(&mut reader, cap).await,
                    Err("the frame is not valid")
                );
                assert_eq!(reader.served, 4);
            }
        });
    });
}

#[test]
fn frames_round_trip_in_any_chunking() {
    let rt = runtime();
    let mut sides = [false; 2];
    let mut exact = [false; 2];
    run("frames_round_trip_in_any_chunking", 0x303, |g, _| {
        rt.block_on(async {
            let value = if g.below(8) == 0 {
                let mut value = Request::App {
                    to: text(g, 64),
                    app: text(g, 64),
                    body: String::new(),
                };
                let overhead = serde_json::to_vec(&value).unwrap().len();
                let size = match g.below(4) {
                    0 => CONTROL_CAP,
                    1 => CONTROL_CAP + 1,
                    _ => (CONTROL_CAP as i64 + g.below(33) as i64 - 16) as usize,
                };
                if size == CONTROL_CAP || size == CONTROL_CAP + 1 {
                    exact[size - CONTROL_CAP] = true;
                }
                if let Request::App { body, .. } = &mut value {
                    *body = "a".repeat(size - overhead);
                }
                sides[usize::from(size > CONTROL_CAP)] = true;
                value
            } else {
                request(g)
            };
            round_trip!(value, Request, CONTROL_CAP, g);
            let answer = if g.chance(50) {
                Answer::Hello { hello: hello(g) }
            } else {
                Answer::Ok
            };
            round_trip!(answer, Answer, CONTROL_CAP, g);
            let join = JoinRequest {
                code: text(g, 128),
                consent: text(g, 128),
                name: text(g, 64),
                addrs: addrs(g),
            };
            round_trip!(join, JoinRequest, CONTROL_CAP, g);
        });
    });
    let (seed, cases) = settings(0x303);
    assert!(
        sides.into_iter().all(|hit| hit),
        "frames_round_trip_in_any_chunking seed {seed} case {}: both cap sides {sides:?}",
        cases - 1
    );
    assert!(
        exact.into_iter().all(|hit| hit),
        "frames_round_trip_in_any_chunking seed {seed} case {}: exact cap edges {exact:?}",
        cases - 1
    );
}

#[test]
fn computer_records_verify_exactly_when_they_fit() {
    let mut sides = [false; 2];
    let mut near = [false; 2];
    let mut exact = [false; 2];
    run(
        "computer_records_verify_exactly_when_they_fit",
        0x304,
        |g, _| {
            let node = key(g);
            let mut body = computer_body(g, &node);
            if g.chance(25) {
                body.apps = BTreeMap::from([(app(g), "\u{1}".repeat(SECTION_MAX))]);
                let target = match g.below(4) {
                    0 => COMPUTER_RECORD_MAX,
                    1 => COMPUTER_RECORD_MAX + 1,
                    _ => COMPUTER_RECORD_MAX - 64 + g.below(129) as usize,
                };
                let initial = computer_record::sign(body.clone(), &node);
                let remaining = target - serde_json::to_vec(&initial).unwrap().len();
                body.apps.insert(
                    "lantern-padding".into(),
                    "\u{1}".repeat((remaining - SECTION_MAX / 2) / 6),
                );
                body.apps.insert("lantern-size".into(), "a".into());
                for _ in 0..4 {
                    let record = computer_record::sign(body.clone(), &node);
                    let length = serde_json::to_vec(&record).unwrap().len();
                    if length == target {
                        break;
                    }
                    let section = body.apps.get_mut("lantern-size").unwrap();
                    let adjusted = section.len() as isize + target as isize - length as isize;
                    assert!((1..=SECTION_MAX as isize).contains(&adjusted));
                    *section = "a".repeat(adjusted as usize);
                }
                assert_eq!(
                    serde_json::to_vec(&computer_record::sign(body.clone(), &node))
                        .unwrap()
                        .len(),
                    target
                );
            } else if g.chance(50) {
                body.apps = (0..3)
                    .map(|i| (format!("lantern{i}"), "\u{1}".repeat(SECTION_MAX)))
                    .collect();
            }
            let record = computer_record::sign(body, &node);
            let bytes = serde_json::to_vec(&record).unwrap();
            let length = bytes.len();
            let large = length > COMPUTER_RECORD_MAX;
            if (COMPUTER_RECORD_MAX - 63..=COMPUTER_RECORD_MAX + 64).contains(&length) {
                near[usize::from(large)] = true;
            }
            if length == COMPUTER_RECORD_MAX || length == COMPUTER_RECORD_MAX + 1 {
                exact[length - COMPUTER_RECORD_MAX] = true;
            }
            sides[usize::from(large)] = true;
            assert_eq!(
                record.verify(),
                if large {
                    Err("the computer record is too large")
                } else {
                    Ok(())
                },
                "serialized bytes {}",
                bytes.len()
            );
            if !large {
                let decoded: ComputerRecord = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(decoded, record);
                assert_eq!(decoded.verify(), Ok(()));
            }
        },
    );
    let (seed, cases) = settings(0x304);
    assert!(
        sides.into_iter().all(|hit| hit),
        "computer_records_verify_exactly_when_they_fit seed {seed} case {}: both size outcomes {sides:?}",
        cases - 1
    );
    for (label, hits) in [("near cap sides", near), ("exact cap edges", exact)] {
        assert!(
            hits.into_iter().all(|hit| hit),
            "computer_records_verify_exactly_when_they_fit seed {seed} case {}: {label} {hits:?}",
            cases - 1
        );
    }
}

fn changed_signature(signature: &mut String, g: &mut Gen) {
    let at = g.below(signature.len() as u64) as usize;
    let old = signature.as_bytes()[at];
    let new = loop {
        let new = *g.pick(b"0123456789abcdef");
        if new != old {
            break new;
        }
    };
    signature.replace_range(at..at + 1, &(new as char).to_string());
}
fn different<T: PartialEq>(original: &T, mut make: impl FnMut() -> T) -> T {
    loop {
        let value = make();
        if value != *original {
            return value;
        }
    }
}

#[test]
fn any_change_to_a_signed_record_breaks_it() {
    run("any_change_to_a_signed_record_breaks_it", 0x305, |g, i| {
        let node = key(g);
        let other = key(g);
        let recovery = key(g);
        if i % 5 == 0 {
            let record = small_computer(g, &node);
            assert_eq!(record.verify(), Ok(()));
            let mut changed = record.clone();
            let body = &mut changed.body;
            match g.below(10) {
                0 => body.version = different(&body.version, || 1 + g.below(i64::MAX as u64)),
                1 => body.name = different(&body.name, || name(g)),
                2 => body.roles = different(&body.roles, || vec![name(g)]),
                3 => body.addrs = different(&body.addrs, || addrs(g)),
                4 => body.fleet = different(&body.fleet, || hex64(g)),
                5 => body.key = other.public().to_string(),
                6 => {
                    if body.apps.len() == APPS_MAX {
                        body.apps.pop_first();
                    } else {
                        let added = loop {
                            let added = app(g);
                            if !body.apps.contains_key(&added) {
                                break added;
                            }
                        };
                        body.apps.insert(added, "added".into());
                    }
                }
                7 if !body.apps.is_empty() => {
                    body.apps.pop_first();
                }
                _ => {
                    if let Some((_, value)) = body.apps.iter_mut().next() {
                        *value = different(value, || section(g));
                    } else {
                        body.apps.insert(app(g), section(g));
                    }
                }
            }
            assert_ne!(changed.body, record.body);
            let signing_key = if changed.body.key == node.public().to_string() {
                &node
            } else {
                &other
            };
            assert_eq!(
                computer_record::sign(changed.body.clone(), signing_key).verify(),
                Ok(()),
                "changed computer still has valid shape"
            );
            assert!(changed.verify().is_err(), "computer body change");
            let mut bad = record.clone();
            changed_signature(&mut bad.signature, g);
            assert!(bad.verify().is_err(), "computer signature change");
            assert!(
                computer_record::sign(record.body, &other).verify().is_err(),
                "computer wrong key"
            );
        } else if matches!(i % 5, 1 | 2) {
            let newcomer_key = key(g);
            let record = if i % 5 == 1 {
                introduction_with(g, &node, &newcomer_key)
            } else {
                removal(g, &node)
            };
            assert_eq!(record.verify(), Ok(()));
            let mut changed = record.clone();
            let signing_key = match &mut changed.body {
                RecordBody::Introduction {
                    fleet,
                    inviter,
                    counter,
                    newcomer,
                    name: member_name,
                    addrs,
                    consent,
                } => {
                    let newcomer_key = match g.below(6) {
                        0 => {
                            *counter = different(counter, || 1 + g.below(u64::MAX));
                            &newcomer_key
                        }
                        1 => {
                            *fleet = different(fleet, || hex64(g));
                            &newcomer_key
                        }
                        2 => {
                            *member_name = different(member_name, || name(g));
                            &newcomer_key
                        }
                        3 => {
                            *addrs = different(addrs, || addrs_for_introduction(g));
                            &newcomer_key
                        }
                        4 => {
                            *newcomer = other.public().to_string();
                            &other
                        }
                        _ => {
                            *inviter = other.public().to_string();
                            &newcomer_key
                        }
                    };
                    *consent =
                        membership::consent(newcomer_key, fleet, inviter, member_name, addrs);
                    if *inviter == node.public().to_string() {
                        &node
                    } else {
                        &other
                    }
                }
                RecordBody::Removal {
                    fleet,
                    signer,
                    counter,
                    subject,
                } => {
                    match g.below(4) {
                        0 => *counter = different(counter, || 1 + g.below(u64::MAX)),
                        1 => *fleet = different(fleet, || hex64(g)),
                        2 => *subject = different(subject, || key(g).public().to_string()),
                        _ => *signer = other.public().to_string(),
                    }
                    if *signer == node.public().to_string() {
                        &node
                    } else {
                        &other
                    }
                }
            };
            assert_ne!(changed.body, record.body);
            assert_eq!(
                membership::sign(changed.body.clone(), signing_key).verify(),
                Ok(()),
                "changed membership still has valid shape"
            );
            assert!(changed.verify().is_err(), "membership body change");
            let mut bad = record.clone();
            changed_signature(&mut bad.signature, g);
            assert!(bad.verify().is_err(), "membership signature change");
            assert!(
                membership::sign(record.body.clone(), &other)
                    .verify()
                    .is_err(),
                "membership wrong key"
            );
            let mut body = record.body.clone();
            if let RecordBody::Introduction {
                fleet,
                inviter,
                name: member_name,
                addrs,
                consent,
                ..
            } = &mut body
            {
                let (agreed_name, agreed_addrs) = if g.chance(50) {
                    (different(member_name, || name(g)), addrs.clone())
                } else {
                    (
                        member_name.clone(),
                        different(addrs, || addrs_for_introduction(g)),
                    )
                };
                *consent =
                    membership::consent(&newcomer_key, fleet, inviter, &agreed_name, &agreed_addrs);
                assert!(
                    membership::sign(body, &node).verify().is_err(),
                    "introduction with another consent"
                );
            }
        } else {
            let signer = if i % 5 == 3 {
                Signer::Founder
            } else {
                Signer::Recovery
            };
            let record = fleet_record(g, &node, &recovery, signer);
            assert_eq!(record.verify(), Ok(()));
            let mut changed = record.clone();
            match g.below(9) {
                0 => changed.body.id = different(&changed.body.id, || hex64(g)),
                1 => changed.body.name = different(&changed.body.name, || name(g)),
                2 => {
                    changed.body.version =
                        different(&changed.body.version, || 1 + g.below(u64::MAX))
                }
                3 => changed.body.epoch = different(&changed.body.epoch, || g.u64()),
                4 => changed.body.founder = other.public().to_string(),
                5 => changed.body.recovery = other.public().to_string(),
                6 => {
                    changed.body.release_key = if changed.body.release_key.is_some() {
                        None
                    } else {
                        Some(other.public().to_string())
                    }
                }
                7 => {
                    changed.body.release_key = Some(
                        different(&changed.body.release_key, || {
                            Some(key(g).public().to_string())
                        })
                        .unwrap(),
                    )
                }
                _ => {
                    changed.signer = match signer {
                        Signer::Founder => Signer::Recovery,
                        Signer::Recovery => Signer::Founder,
                    }
                }
            }
            assert_ne!(changed, record);
            let signer_id = match changed.signer {
                Signer::Founder => &changed.body.founder,
                Signer::Recovery => &changed.body.recovery,
            };
            let signing_key = [&node, &recovery, &other]
                .into_iter()
                .find(|key| key.public().to_string() == *signer_id)
                .unwrap();
            assert_eq!(
                fleet::sign(changed.body.clone(), changed.signer, signing_key).verify(),
                Ok(()),
                "changed fleet still has valid shape"
            );
            assert!(changed.verify().is_err(), "fleet body or signer change");
            let mut bad = record.clone();
            changed_signature(&mut bad.signature, g);
            assert!(bad.verify().is_err(), "fleet signature change");
            assert!(
                fleet::sign(record.body, signer, &other).verify().is_err(),
                "fleet wrong key"
            );
        }
    });
}

fn json_tokens(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut tokens = vec![];
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        match bytes[at] {
            b'"' => {
                at += 1;
                while bytes[at] != b'"' {
                    at += if bytes[at] == b'\\' { 2 } else { 1 };
                }
                at += 1;
                if bytes.get(at) != Some(&b':') {
                    tokens.push((start, at));
                }
            }
            b'0'..=b'9' => {
                while bytes.get(at).is_some_and(u8::is_ascii_digit) {
                    at += 1;
                }
                tokens.push((start, at));
            }
            _ => at += 1,
        }
    }
    tokens
}
fn edit_record_json(g: &mut Gen, bytes: &mut Vec<u8>, preserving: bool) {
    if preserving {
        match g.below(3) {
            0 => {
                let mut edited = vec![];
                let mut string = false;
                let mut escaped = false;
                for &byte in bytes.iter() {
                    if !string && b"{}[],:".contains(&byte) {
                        edited.push(*g.pick(b" \n\t"));
                    }
                    edited.push(byte);
                    if byte == b'"' && !escaped {
                        string = !string;
                    }
                    escaped = string && byte == b'\\' && !escaped;
                }
                *bytes = edited;
                return;
            }
            1 => {
                let text = std::str::from_utf8(bytes).unwrap();
                let split = text.rfind(",\"signature\":").unwrap();
                let value: serde_json::Value = serde_json::from_slice(bytes).unwrap();
                let middle = if value.get("signer").is_some() {
                    text[..split].rfind(",\"signer\":").unwrap()
                } else {
                    split
                };
                *bytes = format!(
                    "{{{}{},{}}}",
                    &text[split + 1..text.len() - 1],
                    &text[middle..split],
                    &text[1..middle]
                )
                .into_bytes();
                return;
            }
            _ => {}
        }
    }
    let tokens = json_tokens(bytes);
    let numbers: Vec<_> = tokens
        .iter()
        .copied()
        .filter(|(start, _)| bytes[*start] != b'"')
        .collect();
    if !preserving && g.chance(50) && !numbers.is_empty() {
        let &(start, end) = g.pick(&numbers);
        let at = start + g.below((end - start) as u64) as usize;
        bytes[at] = if bytes[at] == b'9' {
            b'8'
        } else {
            bytes[at] + 1
        };
        return;
    }
    let mut positions = vec![];
    for (start, end) in tokens {
        if bytes[start] != b'"' {
            continue;
        }
        let mut at = start + 1;
        while at < end - 1 {
            if bytes[at] == b'\\' {
                at += if bytes[at + 1] == b'u' { 6 } else { 2 };
            } else {
                if bytes[at].is_ascii_alphanumeric() {
                    positions.push(at);
                }
                at += 1;
            }
        }
    }
    let at = *g.pick(&positions);
    if preserving {
        let escaped = if g.chance(50) {
            format!("\\u00{:02x}", bytes[at])
        } else {
            format!("\\u00{:02X}", bytes[at])
        };
        bytes.splice(at..at + 1, escaped.bytes());
    } else {
        bytes[at] = if bytes[at] == b'a' { b'b' } else { b'a' };
    }
}

#[test]
fn mutated_record_json_never_verifies_as_another_record() {
    let mut verified_equal = 0;
    let mut refused = 0;
    run(
        "mutated_record_json_never_verifies_as_another_record",
        0x306,
        |g, i| {
            let node = key(g);
            let recovery = key(g);
            macro_rules! check {
                ($record:expr, $ty:ty) => {{
                    let record = $record;
                    assert_eq!(record.verify(), Ok(()));
                    let mut bytes = serde_json::to_vec(&record).unwrap();
                    let edit = g.below(3);
                    if edit == 0 {
                        let count = 1 + g.below(8) as usize;
                        mutate(g, &mut bytes, count);
                    } else {
                        edit_record_json(g, &mut bytes, edit == 1);
                    }
                    let decoded = serde_json::from_slice::<$ty>(&bytes);
                    if edit == 1 {
                        assert!(
                            decoded.is_ok(),
                            "meaning-preserving JSON must parse: {bytes:?}"
                        );
                        assert_eq!(
                            decoded.as_ref().unwrap().verify(),
                            Ok(()),
                            "meaning-preserving JSON must verify: {bytes:?}"
                        );
                    }
                    match decoded {
                        Ok(decoded) if decoded.verify().is_ok() => {
                            assert_eq!(decoded, record, "mutated bytes {bytes:?}");
                            verified_equal += 1;
                        }
                        _ => refused += 1,
                    }
                }};
            }
            match i % 5 {
                0 => check!(small_computer(g, &node), ComputerRecord),
                1 => check!(introduction(g, &node), SignedRecord),
                2 => check!(removal(g, &node), SignedRecord),
                3 => check!(
                    fleet_record(g, &node, &recovery, Signer::Founder),
                    FleetRecord
                ),
                _ => check!(
                    fleet_record(g, &node, &recovery, Signer::Recovery),
                    FleetRecord
                ),
            }
        },
    );
    let (seed, cases) = settings(0x306);
    assert!(
        verified_equal > 0 && refused > 0,
        "mutated_record_json_never_verifies_as_another_record seed {seed} case {}: both verification outcomes [{verified_equal}, {refused}]",
        cases - 1
    );
}

#[test]
fn shape_rules_hold_whatever_the_rest_says() {
    run("shape_rules_hold_whatever_the_rest_says", 0x307, |g, _| {
        let node = key(g);
        let record = small_computer(g, &node);
        assert_eq!(record.verify(), Ok(()));
        let mut body = record.body.clone();
        let rule = g.below(20);
        match rule {
            0 => body.version = 0,
            1 => body.version = i64::MAX as u64 + 1,
            2 => body.name.clear(),
            3 => body.name = "a".repeat(33),
            4 => body.name = "9garden".into(),
            5 => body.name = "Garden".into(),
            6 => body.roles = (0..9).map(|i| format!("role{i}")).collect(),
            7 => body.roles = vec!["cedar".into(), "birch".into()],
            8 => body.roles = vec!["birch".into(); 2],
            9 => body.roles = vec!["Birch".into()],
            10 => body.apps = BTreeMap::from([("Lantern".into(), "x".into())]),
            11 => body.apps = BTreeMap::from([("lantern".into(), String::new())]),
            12 => body.apps = BTreeMap::from([("lantern".into(), "x".repeat(SECTION_MAX + 1))]),
            13 => {
                body.apps = (0..9)
                    .map(|i| (format!("lantern{i}"), "x".into()))
                    .collect()
            }
            14 => {
                body.addrs = vec![
                    g.pick(&[
                        "0.0.0.0:1",
                        "[::]:1",
                        "224.0.0.1:1",
                        "255.255.255.255:1",
                        "[ff02::1]:1",
                        "[::ffff:192.0.2.1]:1",
                    ])
                    .parse()
                    .unwrap(),
                ]
            }
            15 => {
                body.addrs = if g.chance(50) {
                    vec![
                        "192.0.2.1:1".parse().unwrap(),
                        "192.0.2.2:1".parse().unwrap(),
                    ]
                } else {
                    vec![
                        "192.0.2.1:1".parse().unwrap(),
                        "[2001:db8::1]:2".parse().unwrap(),
                    ]
                }
            }
            16 => body.key = "G".repeat(64),
            17 => body.fleet = "a".repeat(63),
            18 => body.fleet = "AB".repeat(32),
            _ => body.key = node.public().to_string().to_uppercase(),
        }
        assert_eq!(
            computer_record::sign(body, &node).verify(),
            Err("not a computer record"),
            "shape rule {rule}"
        );
        for signature in [
            record.signature[..127].to_string(),
            format!("{}0", record.signature),
            record.signature.to_uppercase(),
        ] {
            let mut bad = record.clone();
            bad.signature = signature;
            assert_eq!(bad.verify(), Err("not a computer record"));
        }
    });
}

#[test]
fn fleet_accept_keeps_its_promises() {
    let mut accepted = [false; 2];
    run("fleet_accept_keeps_its_promises", 0x308, |g, _| {
        let founder = key(g);
        let recovery = key(g);
        let other = key(g);
        let ids = [hex64(g), hex64(g)];
        let keys = [&founder, &recovery, &other];
        let make = |g: &mut Gen| {
            let signer = *g.pick(&[Signer::Founder, Signer::Recovery]);
            let founder = *g.pick(&[&founder, &other]);
            let recovery = *g.pick(&[&recovery, &other]);
            let body = FleetBody {
                id: g.pick(&ids).clone(),
                name: g.pick(&["garden", "birch"]).to_string(),
                version: 1 + g.below(6),
                epoch: g.below(5),
                founder: founder.public().to_string(),
                recovery: recovery.public().to_string(),
                release_key: if g.chance(50) {
                    Some(other.public().to_string())
                } else {
                    None
                },
                relays: vec![],
            };
            let right = match signer {
                Signer::Founder => founder,
                Signer::Recovery => recovery,
            };
            let signing = if g.chance(75) { right } else { *g.pick(&keys) };
            fleet::sign(body, signer, signing)
        };
        let current = make(g);
        let new = make(g);
        assert_eq!(fleet::accept(None, &new), new.verify());
        for r in [&current, &new] {
            if r.verify().is_ok() {
                assert!(fleet::accept(Some(r), r).is_err());
            }
        }
        if fleet::accept(Some(&current), &new).is_ok() {
            assert_eq!(new.verify(), Ok(()));
            assert_eq!(new.body.id, current.body.id);
            assert_eq!(new.body.recovery, current.body.recovery);
            match new.signer {
                Signer::Founder => {
                    accepted[0] = true;
                    assert_eq!(new.body.epoch, current.body.epoch);
                    assert_eq!(new.body.founder, current.body.founder);
                    assert!(new.body.version > current.body.version);
                }
                Signer::Recovery => {
                    accepted[1] = true;
                    assert!(new.body.epoch > current.body.epoch);
                    assert_eq!(new.body.name, current.body.name);
                    assert_eq!(new.body.relays, current.body.relays);
                }
            }
            assert!(fleet::accept(Some(&new), &current).is_err());
        }
    });
    let (seed, cases) = settings(0x308);
    assert!(
        accepted.into_iter().all(|hit| hit),
        "fleet_accept_keeps_its_promises seed {seed} case {}: both signers {accepted:?}",
        cases - 1
    );
}

#[test]
fn join_codes_parse_only_what_they_encode() {
    run("join_codes_parse_only_what_they_encode", 0x309, |g, _| {
        let count = g.below(9) as usize;
        let mut direct = vec![];
        while direct.len() < count {
            let addresses = addrs(g);
            for address in addresses {
                if direct.len() < count && !direct.contains(&address) {
                    direct.push(address);
                }
            }
        }
        let mut code = JoinCode::new(hex64(g), key(g).public(), Hints { direct }).unwrap();
        code.secret = g.array32();
        let encoded = code.encode();
        assert!(JoinCode::parse(&encoded).unwrap() == code);
        let mut chars: Vec<char> = encoded.chars().collect();
        for _ in 0..1 + g.below(4) {
            let at = g.below(chars.len() as u64 + 1) as usize;
            let c = *g.pick(b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-_=")
                as char;
            match g.below(3) {
                0 if at < chars.len() => chars[at] = c,
                1 if at < chars.len() => {
                    chars.remove(at);
                }
                _ => chars.insert(at, c),
            }
        }
        let mutated: String = chars.into_iter().collect();
        let mut non_ascii = encoded.clone();
        let at = g.below(non_ascii.len() as u64 + 1) as usize;
        let c = char::from_u32(128 + g.below(0x110000 - 128) as u32).unwrap_or('🙂');
        non_ascii.insert(at, c);
        let padded = format!(
            "{encoded}{}",
            "a".repeat(CODE_MAX_CHARS + 1 + g.below(64) as usize - encoded.len())
        );
        for input in [mutated, non_ascii, padded] {
            let parsed = JoinCode::parse(&input);
            if input.len() > CODE_MAX_CHARS {
                assert!(parsed.is_err());
            }
            if let Ok(parsed) = parsed {
                let encoded = parsed.encode();
                assert!(encoded.len() <= CODE_MAX_CHARS);
                assert!(JoinCode::parse(&encoded).unwrap() == parsed);
            }
        }
    });
}
