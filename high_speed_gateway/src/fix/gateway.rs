

pub struct Gateway {
    listener: TcpListener,


    // market_data: ...,
    // orders: ...,
}


impl Gateway {
    
    async fn accept(&mut self) -> Result<()> {

        let (stream, error) = self.listener.accept().await?;

        let connection = Connection::new(
            stream,
            // self.session_configs,
            // referenced to whatever shared state
        );


    }
}