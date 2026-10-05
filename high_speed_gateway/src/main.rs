mod fix;

use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::{Parser as Cli, Subcommand};
use tracing::{error, info};

use fix::app::{Application, NoApplication};
use fix::connection::Connection;
use fix::decode::to_decimal;
use fix::demo::{OneOrder, OrderAcker, OrderSpec};
use fix::session::{Role, SessionConfig};
use fix::spec::Side;
use fix::store::{FlushPolicy, Journal, JournalConfig};
use fix::transport::{Clock, StdTcp, SystemClock};

/// Set by Ctrl-C; the running session logs out and the process exits.
static STOP: AtomicBool = AtomicBool::new(false);

#[derive(Cli)]
#[command(about = "FIX 4.4 session endpoint")]
struct Args {
    #[command(subcommand)]
    mode: Mode,

    /// Our CompID (SenderCompID on what we send).
    #[arg(long, global = true, default_value = "GATEWAY")]
    our: String,

    /// The counterparty's CompID.
    #[arg(long, global = true, default_value = "CLIENT")]
    their: String,

    /// How often the loop wakes for timers when the line is quiet, in ms.
    #[arg(long, global = true, default_value_t = 100)]
    poll_ms: u64,

    /// Keep sequence numbers and sent messages in a journal file in this
    /// directory (one per CompID pair), so a restart carries on and resends
    /// are real. Without it, sequence numbers start at 1 and resends are gap fills.
    #[arg(long, global = true)]
    store_dir: Option<PathBuf>,

    /// Journal index entries: how many of our latest messages can be resent.
    #[arg(long, global = true, default_value_t = 65_536)]
    store_entries: u32,

    /// Journal data ring size in MiB (a power of two).
    #[arg(long, global = true, default_value_t = 32)]
    store_mib: usize,

    /// When the journal is made durable: never (kernel writeback), every
    /// (msync per message), or an interval in ms.
    #[arg(long, global = true, default_value = "1000", value_parser = parse_flush)]
    flush: FlushPolicy,

    /// mlock the journal mapping.
    #[arg(long, global = true)]
    store_mlock: bool,
}

#[derive(Subcommand)]
enum Mode {
    /// Wait for counterparties to log on, one connection at a time, and
    /// acknowledge their orders.
    Accept {
        #[arg(long, default_value = "127.0.0.1:9878")]
        bind: String,
    },
    /// Connect and send the Logon.
    Initiate {
        #[arg(long, default_value = "127.0.0.1:9878")]
        connect: String,
        /// HeartBtInt to ask for, in seconds.
        #[arg(long, default_value_t = 30)]
        heartbeat: u32,
        /// Send ResetSeqNumFlag=Y and start the sequence numbers (and the
        /// journal) over.
        #[arg(long)]
        reset: bool,
        /// Send one NewOrderSingle after logon: SYMBOL,buy|sell,QTY[,PRICE]
        /// (no price: market order), e.g. MSFT,buy,100,412.50.
        #[arg(long, value_parser = parse_order)]
        order: Option<OrderSpec>,
    },
}

extern "C" fn on_sigint(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .compact()
        .init();
    // SAFETY: the handler only stores to an atomic.
    unsafe { libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t) };

    let args = Args::parse();
    let poll = Duration::from_millis(args.poll_ms);
    let store = args.store_dir.map(|dir| JournalConfig {
        dir,
        entries: args.store_entries,
        capacity: args.store_mib << 20,
        flush: args.flush,
        lock: args.store_mlock,
    });
    let store = store.as_ref();

    let result = match args.mode {
        Mode::Accept { bind } => accept(&bind, &args.our, &args.their, poll, store),
        Mode::Initiate { connect, heartbeat, reset, order } => {
            let mut config = SessionConfig::new(Role::Initiator, &args.our, &args.their);
            config.heart_bt_int_secs = heartbeat;
            config.reset_on_logon = reset;
            TcpStream::connect(&connect).and_then(|stream| {
                info!(%connect, "connected");
                match order {
                    Some(order) => run(stream, config, poll, store, OneOrder::new(order)),
                    None => run(stream, config, poll, store, NoApplication),
                }
            })
        }
    };
    if let Err(e) = result {
        error!(%e, "stopped");
        std::process::exit(1);
    }
}

fn accept(bind: &str, our: &str, their: &str, poll: Duration, store: Option<&JournalConfig>) -> std::io::Result<()> {
    let listener = TcpListener::bind(bind)?;
    // Non-blocking so Ctrl-C is noticed while nobody is connected.
    listener.set_nonblocking(true)?;
    info!(%bind, our, their, "listening");
    while !STOP.load(Ordering::Relaxed) {
        let stream = match listener.accept() {
            Ok((stream, peer)) => {
                info!(%peer, "accepted");
                stream
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::Interrupted => {
                std::thread::sleep(poll);
                continue
            }
            Err(e) => return Err(e),
        };
        stream.set_nonblocking(false)?;
        if let Err(e) = run(stream, SessionConfig::new(Role::Acceptor, our, their), poll, store, OrderAcker::default()) {
            error!(%e, "connection failed");
        }
    }
    info!("stopped");
    Ok(())
}

fn run(stream: TcpStream, config: SessionConfig, poll: Duration, store: Option<&JournalConfig>, app: impl Application) -> std::io::Result<()> {
    let transport = StdTcp::new(stream, poll)?;
    let mut conn = match store {
        Some(store) => {
            let journal = Journal::open(store, &config.our_comp_id, &config.their_comp_id, SystemClock.now_ns())?;
            info!(path = %journal.path().display(), in_seq = journal.next_in_seq(), out_seq = journal.next_out_seq(), "journal opened");
            Connection::with_journal(transport, SystemClock, config, journal, app)?
        }
        None => Connection::new(transport, SystemClock, config, app)?,
    };
    let reason = conn.run(&STOP)?;
    info!(?reason, in_seq = conn.session().next_in_seq(), out_seq = conn.session().next_out_seq(), "session ended");
    Ok(())
}

/// never | every | <interval in ms>
fn parse_flush(text: &str) -> Result<FlushPolicy, String> {
    match text {
        "never" => Ok(FlushPolicy::Never),
        "every" => Ok(FlushPolicy::EveryMessage),
        ms => match ms.parse::<u64>() {
            Ok(ms) if ms > 0 => Ok(FlushPolicy::Timer(Duration::from_millis(ms))),
            _ => Err(format!("expected never, every or a positive interval in ms, got {ms}")),
        },
    }
}

/// SYMBOL,buy|sell,QTY[,PRICE]
fn parse_order(text: &str) -> Result<OrderSpec, String> {
    let parts: Vec<&str> = text.split(',').map(str::trim).collect();
    let [symbol, side, qty, rest @ ..] = parts.as_slice() else {
        return Err("expected SYMBOL,buy|sell,QTY[,PRICE]".into())
    };
    let side = match side.to_ascii_lowercase().as_str() {
        "buy" | "1" => Side::Buy,
        "sell" | "2" => Side::Sell,
        other => return Err(format!("side must be buy or sell, got {other}")),
    };
    let qty = to_decimal(qty.as_bytes()).ok_or(format!("bad quantity {qty}"))?;
    let price = match rest {
        [] => None,
        [price] => Some(to_decimal(price.as_bytes()).ok_or(format!("bad price {price}"))?),
        _ => return Err("too many fields".into()),
    };
    Ok(OrderSpec { cl_ord_id: format!("cli-{}", std::process::id()), symbol: symbol.to_string(), side, qty, price })
}
