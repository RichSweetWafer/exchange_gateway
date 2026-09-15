

pub struct Connection {
    stream: TcpStream,
    parser: Parser,
    session: Session,
    market_data: MarketDataController,
    orders: OrderController,

}