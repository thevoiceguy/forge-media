//! REMB against transport-cc, through a simulated bottleneck (FCP's
//! `docs/TRANSPORT_CC.md` §5, R5a).
//!
//! One viewer of a forwarding room receives three people, each at one of
//! three layers (180p 150 kb/s, 360p 500 kb/s, 720p 1.5 Mb/s), so what the
//! room can send is a ladder of seven steps from 450 kb/s to 4.5 Mb/s. The
//! room moves down at once and up only once the budget has covered the
//! next step for 3 s, as forge-conference's allocator does. Each person's
//! frames leave as one burst, 30 a second, staggered.
//!
//! - **REMB**: the viewer runs [`RembEstimator`] at Anvil's cadence — an
//!   estimate every 200 ms, sent each second or at once when it falls
//!   under 97% of the last one sent.
//! - **Transport-cc**: the viewer runs [`FeedbackWriter`]; the room runs
//!   [`SendSideEstimator`] and probes at 1.25× the next step when the
//!   budget covers at least half of it, a proven probe halving the 3 s
//!   wait and one that falls short backing off.
//!
//! `cargo test -p forge-bwe --test simulation -- --nocapture` prints the
//! table `docs/TRANSPORT_CC.md` quotes.

use forge_bwe::testing::Bottleneck;
use forge_bwe::{
    BandwidthEstimator, FeedbackWriter, Prober, RembEstimator, SendSideEstimator, PROBE_PADDING,
};
use forge_rtp::transport_cc::TransportCcFeedback;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};
use std::time::{Duration, Instant};

const LAYERS_KBPS: [u64; 3] = [150, 500, 1500];
/// Each step: the layer of each of the three people.
const STEPS: [[usize; 3]; 7] = [
    [0, 0, 0],
    [1, 0, 0],
    [1, 1, 0],
    [1, 1, 1],
    [2, 1, 1],
    [2, 2, 1],
    [2, 2, 2],
];
const MTU_PAYLOAD: usize = 1200;
const HEADER: usize = 12 + 8 + 28; // RTP, extensions, UDP/IP
const QUEUE_BYTES: usize = 64 * 1024;
const ONE_WAY: Duration = Duration::from_millis(25);
const UP_AFTER: Duration = Duration::from_secs(3);
const UP_AFTER_PROBED: Duration = Duration::from_millis(1500);
const TICK: Duration = Duration::from_millis(1);
const ALLOCATE_EVERY: Duration = Duration::from_millis(100);

/// One person's frame at `kbps`: its packets and each one's size.
fn frame(kbps: u64) -> (usize, usize) {
    let bytes = (kbps * 1000 / 8 / 30) as usize;
    let packets = bytes.div_ceil(MTU_PAYLOAD);
    (packets, bytes / packets + HEADER)
}

/// What a step costs on the wire, headers and all — what an estimator
/// measures, and what the room's measured layer rates are.
fn step_bps(step: usize) -> u64 {
    STEPS[step]
        .iter()
        .map(|l| {
            let (packets, size) = frame(LAYERS_KBPS[*l]);
            (packets * size * 8 * 30) as u64
        })
        .sum()
}

/// The largest step `budget` covers (none: no limit).
fn step_for(budget: Option<u64>) -> usize {
    match budget {
        None => STEPS.len() - 1,
        Some(b) => (0..STEPS.len())
            .rev()
            .find(|s| step_bps(*s) <= b)
            .unwrap_or(0),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Remb,
    TransportCc,
}

enum Back {
    Remb(u64),
    Feedback(TransportCcFeedback),
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Arrival {
    at_us: u64,
    seq_no: u64,
    ssrc: u32,
    seq: u16,
    ts: u32,
    size: usize,
    twcc: u16,
    cross: bool,
}

/// One ms of the run.
#[derive(Clone, Copy, Debug)]
struct Sample {
    budget: Option<u64>,
    sending: u64,
    queue: Duration,
}

/// Changes to the path at given times.
struct Script {
    capacity: Vec<(Duration, u64)>,
    loss: f64,
    cross: Option<(Duration, u64)>,
    length: Duration,
}

fn run(mode: Mode, script: &Script) -> Vec<Sample> {
    let t0 = Instant::now();
    let us = |t: Instant| t.duration_since(t0).as_micros() as u64;
    let mut link =
        Bottleneck::new(script.capacity[0].1, QUEUE_BYTES, ONE_WAY).with_loss(script.loss);
    let mut heap: BinaryHeap<Reverse<Arrival>> = BinaryHeap::new();
    let mut back: VecDeque<(Instant, Back)> = VecDeque::new();
    let mut remb = RembEstimator::default();
    let mut writer = FeedbackWriter::new();
    let mut sse = SendSideEstimator::new();
    let mut prober = Prober::new();
    let mut last_remb: Option<u64> = None;
    let mut budget: Option<u64> = None;
    let mut step: Option<usize> = None;
    let mut pending: Option<(usize, Instant)> = None;
    let mut seqs = [0u16; 3];
    let mut twcc: u16 = 0;
    let mut n: u64 = 0;
    let mut next_frame: [Duration; 3] = [
        Duration::ZERO,
        Duration::from_millis(11),
        Duration::from_millis(22),
    ];
    let mut frames = [0u32; 3];
    let mut next_cross = Duration::ZERO;
    let mut samples = Vec::new();
    let mut probed_at: Option<(Instant, u64)> = None;

    let mut elapsed = Duration::ZERO;
    while elapsed < script.length {
        let now = t0 + elapsed;
        for (t, c) in &script.capacity {
            if *t == elapsed {
                link.set_rate(*c);
            }
        }

        // What the viewer said, reaching the room.
        while back.front().is_some_and(|(t, _)| *t <= now) {
            match back.pop_front().unwrap().1 {
                Back::Remb(bps) => budget = Some(bps),
                Back::Feedback(fb) => {
                    sse.on_feedback(&fb, now);
                    budget = sse.estimate();
                    if let Some(p) = sse.last_probe() {
                        if probed_at.map_or(true, |(_, v)| v != p) {
                            probed_at = Some((now, p));
                            let next = step_bps((step.unwrap_or(0) + 1).min(STEPS.len() - 1));
                            prober.result(p, next, now);
                        }
                    }
                }
            }
        }

        // The allocator.
        if elapsed.as_millis() % ALLOCATE_EVERY.as_millis() == 0 {
            let want = step_for(budget);
            match step {
                None => step = Some(want),
                Some(cur) if want < cur => {
                    step = Some(want);
                    pending = None;
                }
                Some(cur) if want > cur => {
                    let wait = match probed_at {
                        Some((t, v))
                            if now.duration_since(t) < Duration::from_secs(2)
                                && v >= step_bps(want) =>
                        {
                            UP_AFTER_PROBED
                        }
                        _ => UP_AFTER,
                    };
                    match pending {
                        Some((w, since)) if w == want => {
                            if now.duration_since(since) >= wait {
                                step = Some(want);
                                pending = None;
                            }
                        }
                        _ => pending = Some((want, now)),
                    }
                }
                Some(_) => pending = None,
            }
            // Probe the next step when the budget nearly covers it.
            if mode == Mode::TransportCc {
                let cur = step.unwrap();
                if cur + 1 < STEPS.len() {
                    let next = step_bps(cur + 1);
                    if let Some(b) = budget {
                        if b < next && b * 2 >= next {
                            prober.start(next * 5 / 4, PROBE_PADDING + HEADER, now);
                        }
                    }
                }
            }
        }

        // The room's packets.
        let cur = step.unwrap_or(0);
        for k in 0..3 {
            if elapsed >= next_frame[k] {
                let (packets, size) = frame(LAYERS_KBPS[STEPS[cur][k]]);
                for _ in 0..packets {
                    if mode == Mode::TransportCc {
                        sse.on_sent(twcc, size, now, None);
                    }
                    if let Some(at) = link.send(size, now) {
                        n += 1;
                        heap.push(Reverse(Arrival {
                            at_us: us(at),
                            seq_no: n,
                            ssrc: k as u32 + 1,
                            seq: seqs[k],
                            ts: frames[k] * 3000,
                            size,
                            twcc,
                            cross: false,
                        }));
                    }
                    seqs[k] = seqs[k].wrapping_add(1);
                    twcc = twcc.wrapping_add(1);
                }
                frames[k] += 1;
                next_frame[k] = Duration::from_micros(
                    (k as u64 * 11_000) + u64::from(frames[k]) * 1_000_000 / 30,
                );
            }
        }
        while let Some(cluster) = prober.poll(now) {
            let size = PROBE_PADDING + HEADER;
            sse.on_sent(twcc, size, now, Some(cluster));
            if let Some(at) = link.send(size, now) {
                n += 1;
                heap.push(Reverse(Arrival {
                    at_us: us(at),
                    seq_no: n,
                    ssrc: 99,
                    seq: 0,
                    ts: 0,
                    size,
                    twcc,
                    cross: false,
                }));
            }
            twcc = twcc.wrapping_add(1);
        }
        // Someone else's traffic through the same queue.
        if let Some((from, bps)) = script.cross {
            while elapsed >= from && elapsed >= next_cross {
                link.send(1200, now);
                next_cross += Duration::from_secs_f64(1200.0 * 8.0 / bps as f64);
            }
            if elapsed < from {
                next_cross = from;
            }
        }

        // The viewer.
        while heap.peek().is_some_and(|Reverse(a)| a.at_us <= us(now)) {
            let Reverse(a) = heap.pop().unwrap();
            if a.cross {
                continue;
            }
            let at = t0 + Duration::from_micros(a.at_us);
            match mode {
                Mode::Remb => {
                    if a.ssrc != 99 {
                        remb.on_packet(a.ssrc, a.seq, a.ts, a.size, at)
                    }
                }
                Mode::TransportCc => writer.on_packet(a.twcc, at),
            }
        }
        match mode {
            Mode::Remb => {
                let ms = elapsed.as_millis();
                if ms % 200 == 0 {
                    if let Some(e) = remb.estimate(now) {
                        let fell = last_remb.is_some_and(|l| (e as f64) < l as f64 * 0.97);
                        if ms % 1000 == 0 || fell {
                            last_remb = Some(e);
                            back.push_back((now + ONE_WAY, Back::Remb(e)));
                        }
                    }
                }
            }
            Mode::TransportCc => {
                if let Some(fb) = writer.poll(now) {
                    back.push_back((now + ONE_WAY, Back::Feedback(fb)));
                }
            }
        }

        samples.push(Sample {
            budget,
            sending: step_bps(cur),
            queue: link.queue_delay(now),
        });
        elapsed += TICK;
    }
    samples
}

fn ms(d: usize) -> String {
    format!("{:.1} s", d as f64 / 1000.0)
}

/// The first ms at or after `from` at which `f` holds.
fn first(samples: &[Sample], from: usize, f: impl Fn(&Sample) -> bool) -> Option<usize> {
    samples[from..].iter().position(f)
}

fn peak_queue(samples: &[Sample], from: usize) -> Duration {
    samples[from..]
        .iter()
        .map(|s| s.queue)
        .max()
        .unwrap_or_default()
}

fn mean(samples: &[Sample], from: usize, f: impl Fn(&Sample) -> f64) -> f64 {
    let s = &samples[from..];
    s.iter().map(f).sum::<f64>() / s.len() as f64
}

struct Down {
    estimate_under: Option<usize>,
    sending_under: Option<usize>,
    peak: Duration,
}

fn step_down(mode: Mode) -> Down {
    let s = run(
        mode,
        &Script {
            capacity: vec![
                (Duration::ZERO, 2_500_000),
                (Duration::from_secs(30), 600_000),
            ],
            loss: 0.0,
            cross: None,
            length: Duration::from_secs(50),
        },
    );
    let t = 30_000;
    Down {
        estimate_under: first(&s, t, |x| x.budget.is_some_and(|b| b < 600_000)),
        sending_under: first(&s, t, |x| x.sending <= 600_000),
        peak: peak_queue(&s, t),
    }
}

struct Up {
    estimate_over: Option<usize>,
    sending_at: Option<usize>,
}

fn step_up(mode: Mode) -> Up {
    let s = run(
        mode,
        &Script {
            capacity: vec![
                (Duration::ZERO, 600_000),
                (Duration::from_secs(30), 2_500_000),
            ],
            loss: 0.0,
            cross: None,
            length: Duration::from_secs(70),
        },
    );
    let t = 30_000;
    // The tallest step under 90% of the new capacity: 1.5 Mb/s.
    let target = step_bps(step_for(Some(2_250_000)));
    Up {
        estimate_over: first(&s, t, |x| x.budget.is_some_and(|b| b >= 2_250_000)),
        sending_at: first(&s, t, |x| x.sending >= target),
    }
}

struct Lossy {
    budget: f64,
    sending: f64,
}

fn lossy(mode: Mode) -> Lossy {
    let s = run(
        mode,
        &Script {
            capacity: vec![(Duration::ZERO, 2_500_000)],
            loss: 0.02,
            cross: None,
            length: Duration::from_secs(40),
        },
    );
    Lossy {
        budget: mean(&s, 10_000, |x| x.budget.unwrap_or(0) as f64),
        sending: mean(&s, 10_000, |x| x.sending as f64),
    }
}

struct Competing {
    /// Until the room sends what fits beside the other flow.
    sending_under: Option<usize>,
    peak: Duration,
    sending: f64,
    /// Before the other flow: how often the room changed step, and the
    /// worst queue, from 10 s to 30 s.
    steady_changes: usize,
    steady_peak: Duration,
}

fn competing(mode: Mode) -> Competing {
    let s = run(
        mode,
        &Script {
            capacity: vec![(Duration::ZERO, 2_500_000)],
            loss: 0.0,
            cross: Some((Duration::from_secs(30), 1_000_000)),
            length: Duration::from_secs(60),
        },
    );
    let t = 30_000;
    let fits = step_bps(step_for(Some(1_500_000 - 1)));
    Competing {
        sending_under: first(&s, t, |x| x.sending <= fits),
        peak: peak_queue(&s, t),
        sending: mean(&s, 40_000, |x| x.sending as f64),
        steady_changes: s[10_000..t]
            .windows(2)
            .filter(|w| w[0].sending != w[1].sending)
            .count(),
        steady_peak: s[10_000..t]
            .iter()
            .map(|x| x.queue)
            .max()
            .unwrap_or_default(),
    }
}

fn show(t: Option<usize>) -> String {
    t.map_or("never".into(), ms)
}

#[test]
fn remb_against_transport_cc() {
    let (rd, td) = (step_down(Mode::Remb), step_down(Mode::TransportCc));
    let (ru, tu) = (step_up(Mode::Remb), step_up(Mode::TransportCc));
    let (rl, tl) = (lossy(Mode::Remb), lossy(Mode::TransportCc));
    let (rc, tc) = (competing(Mode::Remb), competing(Mode::TransportCc));
    println!("| Scenario | Measure | REMB | Transport-cc |");
    println!("|---|---|---|---|");
    println!(
        "| 2.5 Mb/s → 600 kb/s | estimate under capacity | {} | {} |",
        show(rd.estimate_under),
        show(td.estimate_under)
    );
    println!(
        "| | sending under capacity | {} | {} |",
        show(rd.sending_under),
        show(td.sending_under)
    );
    println!(
        "| | peak queue delay | {} ms | {} ms |",
        rd.peak.as_millis(),
        td.peak.as_millis()
    );
    println!(
        "| 600 kb/s → 2.5 Mb/s | estimate at 90% | {} | {} |",
        show(ru.estimate_over),
        show(tu.estimate_over)
    );
    println!(
        "| | sending 1.5 Mb/s | {} | {} |",
        show(ru.sending_at),
        show(tu.sending_at)
    );
    println!(
        "| 2.5 Mb/s, 2% loss | mean estimate | {:.0} kb/s | {:.0} kb/s |",
        rl.budget / 1000.0,
        tl.budget / 1000.0
    );
    println!(
        "| | mean sending | {:.0} kb/s | {:.0} kb/s |",
        rl.sending / 1000.0,
        tl.sending / 1000.0
    );
    println!(
        "| 2.5 Mb/s, steady (10–30 s) | step changes | {} | {} |",
        rc.steady_changes, tc.steady_changes
    );
    println!(
        "| | peak queue delay | {} ms | {} ms |",
        rc.steady_peak.as_millis(),
        tc.steady_peak.as_millis()
    );
    println!(
        "| 2.5 Mb/s, 1 Mb/s joins at 30 s | sending under 1.5 Mb/s | {} | {} |",
        show(rc.sending_under),
        show(tc.sending_under)
    );
    println!(
        "| | peak queue delay | {} ms | {} ms |",
        rc.peak.as_millis(),
        tc.peak.as_millis()
    );
    println!(
        "| | mean sending after | {:.0} kb/s | {:.0} kb/s |",
        rc.sending / 1000.0,
        tc.sending / 1000.0
    );

    // What R5a found (docs/TRANSPORT_CC.md §5.1), held so a change to
    // either estimator that undoes it is seen:
    // - both get under a falling capacity, transport-cc sooner and with
    //   the queue no deeper;
    let (r, t) = (rd.sending_under.unwrap(), td.sending_under.unwrap());
    assert!(t < r, "down: {t} ms against {r} ms");
    assert!(td.peak <= rd.peak);
    // - after a dip, REMB never sees past the 180p it receives, and the
    //   probes find the raised capacity;
    assert!(
        ru.sending_at.is_none(),
        "REMB recovered: the finding is gone"
    );
    assert!(tu.sending_at.is_some_and(|t| t < 15_000));
    // - both hold a steady path without moving.
    assert!(tc.steady_changes <= 2, "{} step changes", tc.steady_changes);
}
