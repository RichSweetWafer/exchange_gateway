use tokio::net::TcpListener;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;

use clap::Parser;

use tracing;

use rand::seq::IndexedRandom;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;


#[derive(Parser, Debug)]
#[command(version, about = "A mock exchange server for testing purposes", long_about = None)]
struct Args {
    #[arg(short, long, default_value = "127.0.0.1:9001")]
    listen: SocketAddr,

    #[arg(short, long, default_value = "100")]
    max_listeners: usize,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    
    // Option parsing
    let args = Args::parse();

    // Logging
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .init();

    // Server init
    let listener = TcpListener::bind(args.listen).await?;
    tracing::info!("Server online. Listening a maximum of {} listeners on {}", args.max_listeners, args.listen);

    let tickers = Arc::new(
        vec![
            "BTCUSD",
            "AMZN",
            "NVDA",
            "PLTR",
            "XOM",
        ]
    );


    // Backpressure control
    let semaphore = Arc::new(Semaphore::new(args.max_listeners));

    loop {

        let permit = Arc::clone(&semaphore).acquire_owned().await?;

        let (mut socket, peer) = listener.accept().await?;
        tracing::info!("Connection accepted from: {}", peer);

        let task_tickers = Arc::clone(&tickers);

        tokio::spawn(async move {
            let mut base_price = 46560.0;
            loop {
                // Choose a ticker
                let  ticker = {
                    let mut rng = rand::rng();
                    match task_tickers.choose(& mut rng).cloned() {
                        Some(ticker) => ticker,
                        None => "ticker_error"
                    }
                };
                // Random price walk
                base_price += (rand::random::<f64>() - 0.5) * 10.0;
                let bid = base_price;
                let bid_volume = (rand::random::<f64>() * 10.0 + 1.0) as usize;
                let ask = base_price + 2.5;
                let ask_volume = (rand::random::<f64>() * 10.0 + 1.0) as usize;


                // Format FIX message
                let fix_msg = format!(
                    "8=FIX.4.4\u{1}9=122\u{1}35=W\u{1}55={}\u{1}269=0\u{1}270={:.2}\u{1}271={}\u{1}269=1\u{1}270={:.2}\u{1}271={}\u{1}10=142\u{1}\n",
                    ticker, bid, bid_volume, ask, ask_volume
                );

                // Write message
                if socket.write_all(fix_msg.as_bytes()).await.is_err() {
                    tracing::info!("Client disconnected: {}", peer);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }

            drop(permit);
        });
    }
}