mod fix;

use std::net::{TcpListener, TcpStream};
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
use fix::transport::{StdTcp, SystemClock};

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
        /// Send ResetSeqNumFlag=Y.
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

    let result = match args.mode {
        Mode::Accept { bind } => accept(&bind, &args.our, &args.their, poll),
        Mode::Initiate { connect, heartbeat, reset, order } => {
            let mut config = SessionConfig::new(Role::Initiator, &args.our, &args.their);
            config.heart_bt_int_secs = heartbeat;
            config.reset_on_logon = reset;
            TcpStream::connect(&connect).and_then(|stream| {
                info!(%connect, "connected");
                match order {
                    Some(order) => run(stream, config, poll, OneOrder::new(order)),
                    None => run(stream, config, poll, NoApplication),
                }
            })
        }
    };
    if let Err(e) = result {
        error!(%e, "stopped");
        std::process::exit(1);
    }
}

fn accept(bind: &str, our: &str, their: &str, poll: Duration) -> std::io::Result<()> {
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
        if let Err(e) = run(stream, SessionConfig::new(Role::Acceptor, our, their), poll, OrderAcker::default()) {
            error!(%e, "connection failed");
        }
    }
    info!("stopped");
    Ok(())
}

fn run(stream: TcpStream, config: SessionConfig, poll: Duration, app: impl Application) -> std::io::Result<()> {
    let mut conn = Connection::new(StdTcp::new(stream, poll)?, SystemClock, config, app)?;
    let reason = conn.run(&STOP)?;
    info!(?reason, in_seq = conn.session().next_in_seq(), out_seq = conn.session().next_out_seq(), "session ended");
    Ok(())
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
