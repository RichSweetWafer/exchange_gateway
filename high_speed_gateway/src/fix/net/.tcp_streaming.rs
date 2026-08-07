



pub fn process_socket_stream(mut stream: TcpStream, rx: &mut StreamingReceiver) {

    let mut chunk_buffer = [0; 1024]; // TODO: change


    loop {
        match stream.read(&mut chunk_buffer) {
            Ok(0) => break, // Remote disconnected
            Ok(bytes_read) => {
                
                for i in 0..bytes_read {
                    match rx.feed_byte(chunk_buffer[i]) {
                        Ok(Some(message)) => {
                            // parse
                        },
                        Err(e) => {
                            // log error
                            return;
                        },
                        Ok(None) => {
                            // Message is incomplete yet
                        }
                    }
                }
            },
            Err(_) => break,
        }
    }
}