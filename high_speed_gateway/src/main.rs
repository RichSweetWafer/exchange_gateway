use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::sync::broadcast;

use tracing;

use clap::Parser;

pub mod fix_parser;
use fix_parser::FixParser;

pub mod market_service;
use market_service::{MarketService, MarketDataStreamerServer, PriceTick, Server};


#[derive(Parser, Debug)]
#[command(version, about = "A high speed exchange gateway for parsing FIX data into gRPC streamed messages", long_about = None)]
struct Args {
    #[arg(short, long, default_value = "127.0.0.1:9001")]
    exchange: SocketAddr,

    #[arg(short, long, default_value = "127.0.0.1:50051")]
    grpc_server: SocketAddr,
}



async fn ingestion_pipeline(exchange_addr: &SocketAddr, tx: broadcast::Sender<PriceTick>) {

    const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
    const MAX_RETRIES: i32 = 5;
    
    let mut retry_delay = Duration::from_secs(1);
    let mut retry_count = 0;

    while retry_count < MAX_RETRIES {
        tracing::info!("Attempting connection to the Exchange at {} ({} retries)...", exchange_addr, retry_count);
        // Try to open the connection
        let mut stream =  match TcpStream::connect(exchange_addr).await {
            Ok(s) => {
                tracing::info!("Connected to the exchange");
                // Reset delay and retry count
                retry_delay = Duration::from_secs(1);
                retry_count = 0;
                s
            },
            Err(e) => {
                tracing::info!("Connection failed: {:?}. Retrying in {}s...",
                        e.kind(),
                        retry_delay.as_secs()
                );
                tokio::time::sleep(retry_delay).await;
                // Probably the worst choice for the exchange gateway, but I want to double the timeout for now
                retry_delay = std::cmp::min(retry_delay * 2, MAX_RETRY_DELAY);
                retry_count += 1;
                continue;
            }
        };

        let mut buffer = vec![0u8; 1024];

        loop {
            match stream.read(&mut buffer).await {
                Ok(0) => {
                    tracing::info!("Exchange disconnected cleanly (EOF)");
                    // No reconnect neeeded
                    return;
                },
                Ok(n) => {
                    let chunk = &buffer[..n];
                    for line in chunk.split(|&b| b == b'\n') {
                        if line.is_empty() { continue; }
                        
                        if let Some(parsed) = FixParser::parse(line) {
                            // Extract epoch
                            let now = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .map(|d| d.as_nanos() as i64)
                                .unwrap_or(0);

                            let tick = PriceTick {
                                symbol: parsed.symbol.to_string(),
                                bid_price: parsed.bid_price,
                                bid_volume: parsed.bid_volume,
                                ask_price: parsed.ask_price,
                                ask_volume: parsed.ask_volume,
                                timestamp_ns: now
                            };

                            // Broadcast gRPC
                            // Error means there are no listeners
                            let _ = tx.send(tick);
                        }
                    }
                },
                Err(e) => {
                    tracing::info!("Socket read error: {:?}", e.kind());
                    // Try to reconnect
                    break;
                }

            }
        }
        
    }
    tracing::info!("Reached the maximum number of retries");

}


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {

    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .init();

    let (tx, _) = broadcast::channel::<PriceTick>(4096);

    // Run ingestion
    let tx_clone = tx.clone();
    tokio::spawn(async move {
        ingestion_pipeline(&args.exchange, tx_clone).await;
    });

    // Run fronting gRPC interface
    let service = MarketService{ tx };
    tracing::info!("gRPC service live at {}", args.grpc_server);

    Server::builder()
        .add_service(MarketDataStreamerServer::new(service))
        .serve(args.grpc_server)
        .await?;

    Ok(())
}