//! EXPERIMENTAL: differential tests. Random and mutated frames (empty values,
//! bad tags, '=' in values, flipped/inserted/removed bytes, wrong checksums) go
//! through the current scalar parser and each prototype; outcomes, Reject tags
//! and reasons, and field maps must match exactly.
//! Results: parser_fast 200k frames (~30k valid / 24k garbled / 147k rejects), all
//! modes identical; parser_tape 181k compared (TagRepeated cases skipped), identical.

pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 { self.0 ^= self.0 << 13; self.0 ^= self.0 >> 7; self.0 ^= self.0 << 17; self.0 }
    pub fn below(&mut self, n: u64) -> u64 { self.next() % n }
    pub fn chance(&mut self, pct: u64) -> bool { self.below(100) < pct }
}

pub fn with_trailer(mut f: Vec<u8>, rng: &mut Rng) -> Vec<u8> {
    let sum = f.iter().fold(0u32, |s, &b| s + b as u32) % 256;
    let cs = if rng.chance(10) { (sum + 1 + rng.below(255) as u32) % 256 } else { sum };
    f.extend(format!("10={:03}\x01", cs).bytes());
    f
}

pub fn gen_frame(rng: &mut Rng) -> Vec<u8> {
    let mut body = String::new();
    let types = ["0", "A", "D", "8", "W"];
    if !rng.chance(3) { body += &format!("35={}\x01", types[rng.below(5) as usize]); }
    for _ in 0..rng.below(40) {
        let tag = match rng.below(100) {
            0 => "035".to_string(), 1 => "123456".into(), 2 => "70000".into(), 3 => "20000".into(),
            4 => "10".into(), 5 => "".into(), 6 => "5x".into(), 7 => "0".into(),
            _ => (1 + rng.below(999)).to_string(),
        };
        let len = if rng.chance(3) { 0 } else { 1 + rng.below(40) as usize };
        let mut value = String::new();
        for _ in 0..len {
            value.push(match rng.below(40) { 0 => '=', 1 => '|', _ => (0x20 + rng.below(0x5F) as u8) as char });
        }
        body += &format!("{tag}={value}\x01");
    }
    let body = body.replace('|', "~");
    let mut f = format!("8=FIX.4.4\x019={}\x01{}", body.len(), body).into_bytes();
    // Mutations: random byte, inserted SOH, removed byte.
    for _ in 0..rng.below(3) {
        if f.len() < 2 || !rng.chance(30) { continue }
        let i = rng.below(f.len() as u64) as usize;
        match rng.below(3) {
            0 => f[i] = rng.below(256) as u8,
            1 => f.insert(i, 0x01),
            _ => { f.remove(i); }
        }
    }
    if rng.chance(2) { f.pop(); } // body not ending in SOH
    with_trailer(f, rng)
}


mod fast {
    use super::*;
    use crate::fix::experimental::parser_fast::*;

    #[derive(Debug, PartialEq)]
    enum Summary { Msg(Vec<(u16, Vec<u8>)>), Garbled(GarbledKind), Reject(u16, RejectReason, Vec<(u16, Vec<u8>)>) }

    fn fields(msg: &crate::fix::Message, probe: &[u16]) -> Vec<(u16, Vec<u8>)> {
        probe.iter().filter_map(|&t| msg.fields.get_slice(t, msg.raw).map(|v| (t, v.to_vec()))).collect()
    }

    fn summarize(p: &mut Parser, f: &[u8], probe: &[u16]) -> Summary {
        match p.parse_frame(f) {
            ParseOutcome::Message(m) => Summary::Msg(fields(&m, probe)),
            ParseOutcome::Garbled(k) => Summary::Garbled(k),
            ParseOutcome::Reject { partial, tag, reason } => Summary::Reject(tag, reason, fields(&partial, probe)),
        }
    }

    #[test]
    fn decode_tag_matches_scalar_for_every_tag() {
        for n in 0..=99_999u32 {
            for s in [n.to_string(), format!("{n:05}")] {
                let mut buf = s.clone().into_bytes();
                buf.extend(b"=x\x01junk");
                assert_eq!(decode_tag(&buf, 0, s.len()), Some(n), "{s}");
            }
        }
        for bad in ["1a", "a1", "1/", "1:", " 1", "12 34"] {
            let mut buf = bad.as_bytes().to_vec();
            buf.extend(b"=x\x01junkjunk");
            assert_eq!(decode_tag(&buf, 0, bad.len()), None, "{bad}");
        }
    }

    #[test]
    fn fast_modes_match_scalar() {
        let probe: Vec<u16> = (0..=1000).chain([16383, 20000, 65535]).collect();
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut scalar = Box::new(Parser::with_mode(Mode::Scalar));
        let mut fast: Vec<Box<Parser>> = [Mode::Memchr, Mode::Sse2, Mode::Avx2].map(|m| Box::new(Parser::with_mode(m))).into();
        let (mut msgs, mut garbled, mut rejects) = (0, 0, 0);
        for case in 0..200_000 {
            let f = gen_frame(&mut rng);
            let want = summarize(&mut scalar, &f, &probe);
            match want { Summary::Msg(_) => msgs += 1, Summary::Garbled(_) => garbled += 1, Summary::Reject(..) => rejects += 1 }
            for p in fast.iter_mut() {
                let got = summarize(p, &f, &probe);
                assert_eq!(got, want, "case {case} mode {} frame {:?}", p.isa(), String::from_utf8_lossy(&f));
            }
        }
        eprintln!("cases: {msgs} messages, {garbled} garbled, {rejects} rejects; modes {:?}", fast.iter().map(|p| p.isa()).collect::<Vec<_>>());
        assert!(msgs > 10_000 && garbled > 10_000 && rejects > 10_000);
    }
}

mod tape {
    use super::*;
    use crate::fix::parser_v2 as cur;
    use crate::fix::experimental::parser_tape as tape;

    #[derive(Debug, PartialEq)]
    enum Summary { Msg(Vec<(u16, Vec<u8>)>), Garbled(String), Reject(u16, String, Vec<(u16, Vec<u8>)>) }

    fn cur_summary(p: &mut cur::Parser, f: &[u8], probe: &[u16]) -> Summary {
        let fields = |m: &crate::fix::Message| probe.iter().filter_map(|&t| m.fields.get_slice(t, m.raw).map(|v| (t, v.to_vec()))).collect();
        match p.parse_frame(f) {
            cur::ParseOutcome::Message(m) => Summary::Msg(fields(&m)),
            cur::ParseOutcome::Garbled(k) => Summary::Garbled(format!("{k:?}")),
            cur::ParseOutcome::Reject { partial, tag, reason } => Summary::Reject(tag, format!("{reason:?}"), fields(&partial)),
        }
    }

    fn tape_summary(p: &mut tape::Parser, f: &[u8], probe: &[u16]) -> Summary {
        let fields = |m: &tape::Message| probe.iter().filter_map(|&t| m.fields.get_slice(t, m.raw).map(|v| (t, v.to_vec()))).collect();
        match p.parse_frame(f) {
            tape::ParseOutcome::Message(m) => Summary::Msg(fields(&m)),
            tape::ParseOutcome::Garbled(k) => Summary::Garbled(format!("{k:?}")),
            tape::ParseOutcome::Reject { partial, tag, reason } => Summary::Reject(tag, format!("{reason:?}"), fields(&partial)),
        }
    }

    #[test]
    fn tape_parsers_match_current() {
        let probe: Vec<u16> = (0..=1000).chain([16383, 20000, 65535]).collect();
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut current = Box::new(cur::Parser::new());
        let mut scalar = Box::new(tape::Parser::new());
        let mut avx2 = Box::new(tape::Parser::with_avx2());
        let (mut compared, mut skipped) = (0, 0);
        for case in 0..200_000 {
            let f = gen_frame(&mut rng);
            let want = cur_summary(&mut current, &f, &probe);
            // The tape keeps repeats instead of rejecting them, so TagRepeated is the one intended difference.
            if matches!(&want, Summary::Reject(_, r, _) if r == "TagRepeated") { skipped += 1; continue }
            compared += 1;
            for (name, got) in [("scalar", tape_summary(&mut scalar, &f, &probe)), ("avx2", tape_summary(&mut avx2, &f, &probe))] {
                assert_eq!(got, want, "case {case} {name} frame {:?}", String::from_utf8_lossy(&f));
            }
        }
        eprintln!("compared {compared}, skipped {skipped} TagRepeated cases");
    }
}
