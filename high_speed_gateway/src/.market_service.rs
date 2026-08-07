use std::pin::Pin;
use async_stream;
use tonic::{Request, Response, Status};
use tokio::sync::broadcast;
use tokio_stream::Stream;

pub use tonic::transport::Server;

use shared_lib::market::market_data_streamer_server::MarketDataStreamer;
use shared_lib::market::StreamRequest;

pub use shared_lib::market::PriceTick;
pub use shared_lib::market::market_data_streamer_server::MarketDataStreamerServer;

// --- gRPC streaming server
pub struct MarketService {
    pub tx: broadcast::Sender<PriceTick>,
}

#[tonic::async_trait]
impl MarketDataStreamer for MarketService {
    type StreamTicksStream = Pin<Box<dyn Stream<Item = Result<PriceTick, Status>> + Send>>;

    async fn stream_ticks(
        &self,
        request: Request<StreamRequest>,
    ) -> Result<Response<Self::StreamTicksStream>, Status> {
        let req = request.into_inner();
        let target_symbol = req.symbol;
        
        // Create a PriceTick receiver
        let mut rx = self.tx.subscribe();
        tracing::info!("Client subscribed to symbol: {}", target_symbol);

        // Create a response stream
        let output_stream = async_stream::try_stream! {
            loop {
                match rx.recv().await {
                    Ok(tick) => {
                        // Empty symbol means "I want it all"
                        if target_symbol.is_empty() || tick.symbol == target_symbol {
                            yield tick;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::info!("Client lagged behind by {} items", skipped);
                    }
                    // Close recv stream if the channel is done (sender closed/channel broken)
                    Err(_) => break,
                }
            }
        };

        // Answer with a response stream
        Ok(Response::new(Box::pin(output_stream) as Self::StreamTicksStream))
    }
}
