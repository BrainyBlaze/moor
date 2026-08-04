use moor::windows::{InstrumentAck, LaunchHost, LaunchRequest, Marker, admit, launch};
use std::path::Path;

const MARKER: [u8; 84] = [
    0x4d,0x4f,0x4f,0x52,0x4d,0x52,0x4b,0x33,1,0,0,0,7,0,0,0,
    0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,46,0,92,92,46,92,112,105,112,101,92,109,111,111,114,45,
    48,48,48,49,48,50,48,51,48,52,48,53,48,54,48,55,48,56,48,57,48,97,48,98,48,99,48,100,48,101,48,102,
    0xb1,0x25,0xd5,0x68,
];

#[test]
fn marker_matches_v12_and_rejects_every_frozen_field() {
    let marker = Marker::new(7, core::array::from_fn(|n| n as u8), core::array::from_fn(|n| n as u8)).unwrap();
    assert_eq!(marker.encode(), MARKER);
    assert_eq!(Marker::decode(&MARKER).unwrap(), marker);
    for at in [0, 8, 9, 10, 12, 32, 34, 48, 80] {
        let mut bad = MARKER; bad[at] ^= 1; assert!(Marker::decode(&bad).is_err(), "accepted byte {at}");
    }
    assert!(Marker::new(0, [0; 16], [0; 16]).is_err());
    assert!(Marker::decode(&MARKER[..83]).is_err());
}

#[test]
fn instrument_ack_matches_v22_and_requires_eof_and_identity() {
    let nonce = core::array::from_fn(|n| n as u8 + 0x10);
    let expected = [
        0x4d,0x4f,0x4f,0x52,0x49,0x4e,0x53,0x33,1,0,0,0,7,0,0,0,
        0x34,0x12,0,0,0x10,0x11,0x12,0x13,0x14,0x15,0x16,0x17,0x18,0x19,0x1a,0x1b,0x1c,0x1d,0x1e,0x1f,
    ];
    assert_eq!(InstrumentAck::new(7, 0x1234, nonce).unwrap().encode(), expected);
    assert!(InstrumentAck::validate(&expected, true, 7, 0x1234, nonce).is_ok());
    for (bytes, eof, generation, pid) in [(&expected[..35], true, 7, 0x1234), (&expected[..], false, 7, 0x1234), (&expected[..], true, 8, 0x1234), (&expected[..], true, 7, 0)] {
        assert!(InstrumentAck::validate(bytes, eof, generation, pid, nonce).is_err());
    }
}

#[derive(Default)]
struct Host { calls: Vec<&'static str>, ack: Option<Vec<u8>>, fail: Option<&'static str> }
impl Host { fn step(&mut self, name: &'static str) -> Result<(), String> { self.calls.push(name); if self.fail == Some(name) { Err(name.into()) } else { Ok(()) } } }
impl LaunchHost for Host {
    fn protected_root_marker(&mut self) -> Result<(), String> { self.step("protect") }
    fn first_protected_pipe(&mut self, _: &[u8; 46]) -> Result<(), String> { self.step("pipe") }
    fn conpty_job_bootstrap(&mut self, _: &[std::ffi::OsString]) -> Result<u32, String> { self.step("bootstrap")?; Ok(0x1234) }
    fn stage_instrument(&mut self, _: &Path) -> Result<(), String> { self.step("stage") }
    fn inject_and_ack(&mut self, _: u32, _: [u8; 16]) -> Result<(u32, Vec<u8>, bool), String> { self.step("inject")?; Ok((0, self.ack.take().unwrap(), true)) }
    fn resume_child(&mut self, _: u32) -> Result<(), String> { self.step("resume") }
    fn publish_marker(&mut self, _: &[u8; 84]) -> Result<(), String> { self.step("publish") }
    fn authenticate_same_user(&mut self, _: [u8; 4]) -> Result<(), String> { self.step("user") }
}

#[test]
fn security_precedes_marker_parse_and_protocol_parse() {
    let mut host = Host::default(); let mut bad = MARKER; bad[0] = 0;
    assert!(admit(&mut host, &bad, *b"MOOR").is_err()); assert_eq!(host.calls, ["protect"]);
    host.calls.clear(); assert!(admit(&mut host, &MARKER, *b"NOPE").is_err()); assert_eq!(host.calls, ["protect", "user"]);
    host.calls.clear(); assert!(admit(&mut host, &MARKER, *b"MOOS").is_ok()); assert_eq!(host.calls, ["protect", "user"]);
}

#[test]
fn launch_is_contained_fail_closed_and_publishes_last() {
    let nonce = core::array::from_fn(|n| n as u8 + 0x10); let ack = InstrumentAck::new(7, 0x1234, nonce).unwrap().encode().to_vec();
    let marker = Marker::decode(&MARKER).unwrap(); let command = vec!["cmd.exe".into()]; let instrument = Path::new(r"C:\safe.dll");
    let request = LaunchRequest { marker: &marker, command: &command, instrument: Some(instrument), nonce };
    let mut host = Host { ack: Some(ack.clone()), ..Host::default() }; assert_eq!(launch(&mut host, request).unwrap(), 0x1234);
    assert_eq!(host.calls, ["protect", "pipe", "stage", "bootstrap", "inject", "resume", "publish"]);
    let request = LaunchRequest { marker: &marker, command: &command, instrument: Some(instrument), nonce };
    let mut host = Host { ack: Some(ack), fail: Some("inject"), ..Host::default() }; assert!(launch(&mut host, request).is_err());
    assert_eq!(host.calls, ["protect", "pipe", "stage", "bootstrap", "inject"]);
}
