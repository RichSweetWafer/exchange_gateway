//! EXPERIMENTAL: the benchmarks behind the numbers in the other headers, as
//! ignored tests so they need no Cargo.toml changes. Run in release, pinned:
//!   taskset -c 2 cargo test --release <name> -- --ignored --nocapture --test-threads=1
//! Numbers come from a VirtualBox VM without PMU access; compare variants, not absolutes.

/// Byte loop cost with and without per-field exits (scalar baseline).
mod byte_loops {
    #![allow(dead_code)]
    use std::hint::black_box; use std::time::Instant;
    const ORDER: &[u8] = b"8=FIX.4.4\x019=148\x0135=D\x0134=1080\x0149=TESTBUY1\x0152=20180920-18:14:19.508\x0156=TESTSELL1\x0111=636730640278898634\x0115=USD\x0121=2\x0138=7000\x0140=1\x0154=1\x0155=MSFT\x0160=20180920-18:14:19.492\x0110=092\x01";
    fn time(name: &str, mut f: impl FnMut()) { let n = 5_000_000; for _ in 0..n/10 { f() } let t = Instant::now(); for _ in 0..n { f() } println!("{name:<52} {:>6.1} ns/msg", t.elapsed().as_nanos() as f64 / n as f64); }

    #[test]
    #[ignore = "benchmark: cargo test --release byte_loops -- --ignored --nocapture"]
    fn run() {
            // 1. straight byte sum, no field boundaries
            time("byte loop, checksum only (no field exits)", || { let f = black_box(ORDER); let mut s = 0u8; for &b in f { s = s.wrapping_add(b) } black_box(s); });
            // 2. byte loop that exits at every SOH / '=' (same shape as parse_fields, no stores)
            time("byte loop, exit at every '=' and SOH (no stores)", || {
                let f = black_box(ORDER); let mut s = 0u8; let mut i = 0; let mut n = 0u32;
                while i < f.len() { loop { let b = f[i]; s = s.wrapping_add(b); i += 1; if b == b'=' { break } }
                                    loop { let b = f[i]; s = s.wrapping_add(b); i += 1; if b == 1 { break } } n += 1; }
                black_box((s, n)); });
            // 3. same, but every frame has different field lengths so the predictor can't memorize
            let frames: Vec<Vec<u8>> = (0..64u32).map(|k| { let mut v = Vec::new(); for t in 0..16u32 { v.extend(format!("{}={}\x01", 10 + t * 7, "x".repeat(1 + ((k * 31 + t * 17) % 20) as usize)).bytes()) } v }).collect();
            let mut j = 0;
            time("  same, 64 frames with varying field lengths", || {
                let f = black_box(&frames[j & 63]); j += 1; let mut s = 0u8; let mut i = 0;
                while i < f.len() { loop { let b = f[i]; s = s.wrapping_add(b); i += 1; if b == b'=' { break } }
                                    loop { let b = f[i]; s = s.wrapping_add(b); i += 1; if b == 1 { break } } }
                black_box(s); });
    }
}

/// AVX2 parse_frame cost broken into stages.
mod scan_stages {
    #![allow(dead_code)]
    use crate::fix::experimental::parser_fast::*;
    use crate::fix::flat_index_map::FlatIndexMap;
    use std::hint::black_box; use std::time::Instant;
    const ORDER: &[u8] = b"8=FIX.4.4\x019=148\x0135=D\x0134=1080\x0149=TESTBUY1\x0152=20180920-18:14:19.508\x0156=TESTSELL1\x0111=636730640278898634\x0115=USD\x0121=2\x0138=7000\x0140=1\x0154=1\x0155=MSFT\x0160=20180920-18:14:19.492\x0110=092\x01";
    fn time(name: &str, mut f: impl FnMut()) { let n = 5_000_000; for _ in 0..n/10 { f() } let mut best = f64::MAX;
      for _ in 0..5 { let t = Instant::now(); for _ in 0..n { f() } best = best.min(t.elapsed().as_nanos() as f64 / n as f64) } println!("{name:<48} {best:>6.1} ns/msg"); }

    #[test]
    #[ignore = "benchmark: cargo test --release scan_stages -- --ignored --nocapture"]
    fn run() {
            let body = &ORDER[..ORDER.len() - 7];
            time("checksum fold only", || { black_box(black_box(body).iter().fold(0u8, |s, &b| s.wrapping_add(b))); });
            time("avx2 masks only", || unsafe { black_box(experiment::scan(black_box(ORDER), 0)); });
            time("avx2 masks + walk + decode_tag", || unsafe { black_box(experiment::scan(black_box(ORDER), 1)); });
            time("avx2 ... + field checks (no insert)", || unsafe { black_box(experiment::scan(black_box(ORDER), 2)); });
            let tags = [8u16, 9, 35, 34, 49, 52, 56, 11, 15, 21, 38, 40, 54, 55, 60, 10];
            let mut m = Box::new(FlatIndexMap::new());
            time("FlatIndexMap clear + 16 inserts", || { m.clear(); for (i, &t) in black_box(&tags).iter().enumerate() { let _ = m.insert(t, i as u32 * 8, 4); } black_box(&m); });
            let mut p = Box::new(Parser::with_mode(Mode::Avx2));
            time("full parse_frame (avx2)", || { black_box(matches!(p.parse_frame(black_box(ORDER)), ParseOutcome::Message(_))); });
    }
}

/// FlatIndexMap vs FlatIndex vs push_all with a local length.
mod flat_index_store {
    #![allow(dead_code)]
    use crate::fix::flat_index_map::FlatIndexMap;
    use crate::fix::experimental::flat_index::{FlatIndex, TAPE_LEN};
    use std::hint::black_box; use std::time::Instant;
    fn time(name: &str, mut f: impl FnMut()) { let n = 5_000_000; for _ in 0..n/10 { f() } let mut best = f64::MAX;
      for _ in 0..5 { let t = Instant::now(); for _ in 0..n { f() } best = best.min(t.elapsed().as_nanos() as f64 / n as f64) } println!("{name:<48} {best:>6.1} ns/msg"); }

    #[test]
    #[ignore = "benchmark: cargo test --release flat_index_store -- --ignored --nocapture"]
    fn run() {
            let tags = [8u16, 9, 35, 34, 49, 52, 56, 11, 15, 21, 38, 40, 54, 55, 60, 10];
            let mut m = Box::new(FlatIndexMap::new());
            time("FlatIndexMap: clear only", || { m.clear(); black_box(&m); });
            time("FlatIndexMap: clear + 16 inserts", || { m.clear(); for (i, &t) in black_box(&tags).iter().enumerate() { let _ = m.insert(t, i as u32 * 8, 4); } black_box(&m); });
            let mut ix = Box::new(FlatIndex::<TAPE_LEN>::new());
            time("FlatIndex (tape): clear + 16 pushes", || { ix.clear(); for (i, &t) in black_box(&tags).iter().enumerate() { let _ = ix.push(t, i as u32 * 8, 4); } black_box(&ix); });
            let mut ix = Box::new(FlatIndex::<TAPE_LEN>::new());
            let batch: Vec<(u16, u32, u16)> = tags.iter().enumerate().map(|(i, &t)| (t, i as u32 * 8, 4)).collect();
            let mut ix2 = Box::new(FlatIndex::<TAPE_LEN>::new());
            time("FlatIndex: clear + push_all(16), local len", || { ix2.clear(); let _ = ix2.push_all(black_box(&batch)); black_box(&ix2); });
            time("FlatIndex (tape): clear + 16 pushes + 16 gets", || { ix.clear(); for (i, &t) in black_box(&tags).iter().enumerate() { let _ = ix.push(t, i as u32 * 8, 4); } for &t in black_box(&tags) { black_box(ix.get(t)); } });
    }
}

/// parse_frame: scalar vs memchr vs SSE2 vs AVX2 on three workloads.
mod scanners {
    #![allow(dead_code)]
    use crate::fix::experimental::parser_fast::*;
    use std::hint::black_box;
    use std::time::Instant;

    const ORDER: &[u8] = b"8=FIX.4.4\x019=148\x0135=D\x0134=1080\x0149=TESTBUY1\x0152=20180920-18:14:19.508\x0156=TESTSELL1\x0111=636730640278898634\x0115=USD\x0121=2\x0138=7000\x0140=1\x0154=1\x0155=MSFT\x0160=20180920-18:14:19.492\x0110=092\x01";

    fn frame(fields: &[(u32, String)]) -> Vec<u8> {
        let body: String = fields.iter().map(|(t, v)| format!("{t}={v}\x01")).collect();
        let mut f = format!("8=FIX.4.4\x019={}\x01{}", body.len(), body).into_bytes();
        let sum = f.iter().fold(0u32, |s, &b| s + b as u32) % 256;
        f.extend(format!("10={:03}\x01", sum).bytes());
        f
    }

    #[test]
    #[ignore = "benchmark: cargo test --release scanners -- --ignored --nocapture"]
    fn run() {
            let varying: Vec<Vec<u8>> = (0..64u32).map(|k| {
                let mut fields = vec![(35, "D".to_string())];
                for t in 0..16u32 { fields.push((11 + t * 7, "7".repeat(1 + ((k * 31 + t * 17) % 20) as usize))) }
                frame(&fields)
            }).collect();
            let large = {
                let mut fields = vec![(35, "W".to_string()), (34, "12".into()), (49, "EXCH".into()), (56, "GW".into())];
                for t in 0..120u32 { fields.push((100 + t * 3, format!("{}.{}", 400 + t, t * 7 % 100))) }
                frame(&fields)
            };
            let modes = [Mode::Scalar, Mode::Memchr, Mode::Sse2, Mode::Avx2];
            let mut parsers: Vec<Box<Parser>> = modes.iter().map(|&m| Box::new(Parser::with_mode(m))).collect();
            for p in parsers.iter_mut() {
                for f in varying.iter().chain([&large, &ORDER.to_vec()]) { assert!(matches!(p.parse_frame(f), ParseOutcome::Message(_)), "{}", p.isa()) }
            }
            let avg_varying = varying.iter().map(|f| f.len()).sum::<usize>() as f64 / 64.0;
            println!("ORDER {} B, varying avg {:.0} B (17 fields), large {} B (124 fields)", ORDER.len(), avg_varying, large.len());

            let workloads: [(&str, Vec<&[u8]>); 3] = [
                ("ORDER (fixed)", vec![ORDER]),
                ("64 varying-length frames", varying.iter().map(|f| f.as_slice()).collect()),
                ("large 124-field frame", vec![large.as_slice()]),
            ];
            let rounds = 7;
            let n = 2_000_000usize;
            for (name, frames) in &workloads {
                let mut results = vec![Vec::new(); modes.len()];
                for _ in 0..rounds {
                    for (mi, p) in parsers.iter_mut().enumerate() {
                        let iters = if frames[0].len() > 1000 { n / 8 } else { n };
                        let t = Instant::now();
                        for i in 0..iters {
                            let f = black_box(frames[i % frames.len()]);
                            black_box(matches!(p.parse_frame(f), ParseOutcome::Message(_)));
                        }
                        results[mi].push(t.elapsed().as_nanos() as f64 / iters as f64);
                    }
                }
                println!("\n{name}");
                let base = { let mut r = results[0].clone(); r.sort_by(f64::total_cmp); r[0] };
                for (mi, p) in parsers.iter().enumerate() {
                    let mut r = results[mi].clone(); r.sort_by(f64::total_cmp);
                    let bytes = frames.iter().map(|f| f.len()).sum::<usize>() as f64 / frames.len() as f64;
                    println!("  {:<7} best {:>7.1} ns  median {:>7.1} ns  {:>5.2} ns/B  {:>4.1}x", p.isa(), r[0], r[rounds / 2], r[0] / bytes, base / r[0]);
                }
            }
    }
}

/// Current parser vs map+AVX2 vs tape+writer (scalar / AVX2).
mod end_to_end {
    #![allow(dead_code)]
    use crate::fix::parser_v2 as cur;
        use crate::fix::experimental::{parser_tape as tape, parser_fast as fast};
    use std::hint::black_box;
    use std::time::Instant;

    const ORDER: &[u8] = b"8=FIX.4.4\x019=148\x0135=D\x0134=1080\x0149=TESTBUY1\x0152=20180920-18:14:19.508\x0156=TESTSELL1\x0111=636730640278898634\x0115=USD\x0121=2\x0138=7000\x0140=1\x0154=1\x0155=MSFT\x0160=20180920-18:14:19.492\x0110=092\x01";

    fn frame(fields: &[(u32, String)]) -> Vec<u8> {
        let body: String = fields.iter().map(|(t, v)| format!("{t}={v}\x01")).collect();
        let mut f = format!("8=FIX.4.4\x019={}\x01{}", body.len(), body).into_bytes();
        let sum = f.iter().fold(0u32, |s, &b| s + b as u32) % 256;
        f.extend(format!("10={:03}\x01", sum).bytes());
        f
    }

    #[test]
    #[ignore = "benchmark: cargo test --release end_to_end -- --ignored --nocapture"]
    fn run() {
            let varying: Vec<Vec<u8>> = (0..64u32).map(|k| {
                let mut fields = vec![(35, "D".to_string())];
                for t in 0..16u32 { fields.push((11 + t * 7, "7".repeat(1 + ((k * 31 + t * 17) % 20) as usize))) }
                frame(&fields)
            }).collect();
            let large = {
                let mut fields = vec![(35, "W".to_string()), (34, "12".into()), (49, "EXCH".into()), (56, "GW".into())];
                for t in 0..120u32 { fields.push((100 + t * 3, format!("{}.{}", 400 + t, t * 7 % 100))) }
                frame(&fields)
            };

            let mut c = Box::new(cur::Parser::new());
            let mut fa = Box::new(fast::Parser::with_mode(fast::Mode::Avx2));
            let mut ts = Box::new(tape::Parser::new());
            let mut ta = Box::new(tape::Parser::with_avx2());
            let names = ["current (map, scalar)", "map + avx2", "tape + writer, scalar", "tape + writer, avx2"];
            let mut run = |which: usize, f: &[u8]| -> bool {
                match which {
                    0 => matches!(c.parse_frame(f), cur::ParseOutcome::Message(_)),
                    1 => matches!(fa.parse_frame(f), fast::ParseOutcome::Message(_)),
                    2 => matches!(ts.parse_frame(f), tape::ParseOutcome::Message(_)),
                    _ => matches!(ta.parse_frame(f), tape::ParseOutcome::Message(_)),
                }
            };
            for w in 0..4 { for f in varying.iter().chain([&large, &ORDER.to_vec()]) { assert!(run(w, f), "{}", names[w]) } }

            let workloads: [(&str, Vec<&[u8]>); 3] = [
                ("ORDER (171 B, 16 fields)", vec![ORDER]),
                ("64 varying-length frames (avg 263 B, 18 fields)", varying.iter().map(|f| f.as_slice()).collect()),
                ("large frame (1356 B, 124 fields)", vec![large.as_slice()]),
            ];
            let rounds = 7;
            for (name, frames) in &workloads {
                let iters = if frames[0].len() > 1000 { 250_000 } else { 2_000_000 };
                let mut results = vec![Vec::new(); 4];
                for _ in 0..rounds {
                    for w in 0..4 {
                        let t = Instant::now();
                        for i in 0..iters { black_box(run(w, black_box(frames[i % frames.len()]))); }
                        results[w].push(t.elapsed().as_nanos() as f64 / iters as f64);
                    }
                }
                println!("\n{name}");
                for r in results.iter_mut() { r.sort_by(f64::total_cmp) }
                for w in 0..4 {
                    println!("  {:<24} best {:>7.1} ns  median {:>7.1} ns   {:>4.2}x", names[w], results[w][0], results[w][rounds / 2], results[0][0] / results[w][0]);
                }
            }
    }
}

/// parse_frame vs a strict schema pass vs optional get() lookups.
mod schema_check {
    #![allow(dead_code)]
    use crate::fix::experimental::flat_index::*;
    use crate::fix::parser_v2::*;
    use std::hint::black_box;
    use std::time::Instant;

    const ORDER: &[u8] = b"8=FIX.4.4\x019=148\x0135=D\x0134=1080\x0149=TESTBUY1\x0152=20180920-18:14:19.508\x0156=TESTSELL1\x0111=636730640278898634\x0115=USD\x0121=2\x0138=7000\x0140=1\x0154=1\x0155=MSFT\x0160=20180920-18:14:19.492\x0110=092\x01";

    // Allowed-tag bitset for one message type: 16384 bits = 2 KB.
    struct Schema { allowed: [u64; 256], required: &'static [u16] }
    impl Schema {
        const fn new(tags: &[u16], required: &'static [u16]) -> Self {
            let mut allowed = [0u64; 256];
            let mut i = 0;
            while i < tags.len() { allowed[tags[i] as usize / 64] |= 1 << (tags[i] % 64); i += 1; }
            Self { allowed, required }
        }
        #[inline(always)]
        fn allows(&self, tag: u16) -> bool { self.allowed[tag as usize / 64] >> (tag % 64) & 1 != 0 }
    }
    // header + trailer + NewOrderSingle body (subset)
    static NOS: Schema = Schema::new(&[8,9,35,34,49,52,56,10, 11,15,21,38,40,44,54,55,59,60,1,18,99,126,453,448,447,452], &[11,21,38,40,54,55,60]);

    fn validate<const N: usize>(ix: &FlatIndex<N>, s: &Schema) -> Result<(), u16> {
        for f in ix.tape() { if !s.allows(f.tag) { return Err(f.tag) } }
        for &t in s.required { if ix.position(t).is_none() { return Err(t) } }
        Ok(())
    }

    fn index(frame: &[u8], ix: &mut FlatIndex<TAPE_LEN>) {
        ix.clear();
        let end = frame.len();
        let mut i = 0;
        while i < end {
            let eq = i + frame[i..].iter().position(|&b| b == b'=').unwrap();
            let soh = eq + frame[eq..].iter().position(|&b| b == 1).unwrap();
            let mut tag = 0u16; for &b in &frame[i..eq] { tag = tag * 10 + (b - b'0') as u16 }
            ix.push(tag, (eq + 1) as u32, (soh - eq - 1) as u16).unwrap();
            i = soh + 1;
        }
    }

    fn time(name: &str, iters: u32, mut f: impl FnMut()) {
        for _ in 0..iters / 10 { f() }
        let t = Instant::now();
        for _ in 0..iters { f() }
        println!("{name:<40} {:>7.1} ns/msg", t.elapsed().as_nanos() as f64 / iters as f64);
    }

    #[test]
    #[ignore = "benchmark: cargo test --release schema_check -- --ignored --nocapture"]
    fn run() {
            let n = 5_000_000;
            let mut p = Box::new(Parser::new());
            time("parser_v2::parse_frame (148B body, 16 fields)", n, || {
                black_box(matches!(p.parse_frame(black_box(ORDER)), ParseOutcome::Message(_)));
            });
            let mut ix = Box::new(FlatIndex::<TAPE_LEN>::new());
            index(ORDER, &mut ix);
            assert!(validate(&ix, &NOS).is_ok());
            time("schema check: tags allowed + 7 required", n, || { black_box(validate(black_box(&*ix), &NOS).is_ok()); });
            time("7 optional get() lookups", n, || {
                let ix = black_box(&*ix);
                for t in [44u16, 59, 99, 18, 126, 1, 58] { black_box(ix.get(t)); }
            });
    }
}
