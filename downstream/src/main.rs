use std::time::{SystemTime, UNIX_EPOCH};

use shared_lib::market::market_data_streamer_client::MarketDataStreamerClient;
use shared_lib::market::StreamRequest;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(version, about = "A downstream server that mimicks a gRPC client for high_speed_gateway", long_about = None)]
struct Args {
    #[arg(short, long, default_value = "http://127.0.0.1:50051")]
    gateway: String,

    #[arg(short, long, default_value = "")]
    symbol: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {

    let args = Args::parse();

    println!("[Client] Connecting to gateway at {}...", args.gateway);

    let mut client = MarketDataStreamerClient::connect(args.gateway).await?;
    
    println!("[Client] connected");
    println!("[Client] Requesting live stream ticks for \"{}\"", args.symbol);

    let request = StreamRequest {
        symbol: args.symbol,
    };

    let response = client.stream_ticks(request).await?;
    let mut inbound_stream = response.into_inner();

    println!("[Client] Stream established! Awaiting market updates...\n");
    println!("{:<10} | {:<12} | {:<8} | {:<12} | {:<8} | {:<12}", "Symbol", "Bid", "Volume", "Ask", "Volume", "Latency (µs)");
    println!("{:-<70}", "");

    while let Some(tick) = inbound_stream.message().await? {
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        
        let latency_us = (now_ns - tick.timestamp_ns) / 1000;

        println!(
            "{:<10} | {:<12.2} | {:<8} | {:<12.2} | {:<8} | {:<12}",
            tick.symbol,
            tick.bid_price,
            tick.bid_volume,
            tick.ask_price,
            tick.ask_volume,
            latency_us
        );
    }

    println!("[Client] Server closed the stream");

    Ok(())
}