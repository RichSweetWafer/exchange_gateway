mod fix;

use tracing::trace;
use fix::{StreamingReceiver, FixVersion};

fn main()  {

    #[cfg(debug_assertions)]
    {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE) // hardcoded fallback level
            .compact()
            .init();
    }

    let mut rx: StreamingReceiver = StreamingReceiver::new(FixVersion::Fix44);

    let message = b"8=FIX.4.4\x019=148\x0135=D\x0134=1080\x0149=TESTBUY1\x0152=20180920-18:14:19.508\x0156=TESTSELL1\x0111=636730640278898634\x0115=USD\x0121=2\x0138=7000\x0140=1\x0154=1\x0155=MSFT\x0160=20180920-18:14:19.492\x0110=092\x01\x01\x018=FIX.4.4\x019=75\x0135=A\x0134=1092\x0149=TESTBUY1\x0152=20180920-18:24:59.643\x0156=TESTSELL1\x0198=0\x01108=60\x0110=178\x01";
    // let message = b"8=FIX.4.4\x019=148\x0135=D\x0134=1080\x0149=TESTBUY1\x0152=20180920-18:14:19.508\x0156=TESTSELL1\x0111=636730640278898634\x0115=USD\x0121=2\x0138=7000\x0140=1\x0154=1\x0155=MSFT\x0160=20180920-18:14:19.492\x0110=092\x01";
    // let message = b"8=FIX.4.4\x019=75\x0135=A\x0134=1092\x0149=TESTBUY1\x0152=20180920-18:24:59.643\x0156=TESTSELL1\x0198=0\x01108=60\x0110=178\x01";
    // let message = b"8=FIX.4.4\x019=148\x0135=D\x0134=1080\x0149=TESTBUY1\x0152=20180920-18:14:19.508\x0156=TESTSELL1\x0111=636730640278898634\x0115=USD\x0121=2\x0138=7000\x0140=1\x0154=1\x0155=MSFT\x0160=20180920-18:14:19.492\x0110=092\x01\x01\x018=FIX.4.4\x019=75\x0135=A\x0134=1293\x0149=TESTBUY1\x0152=20180920-18:24:59.643\x0156=TESTSELL1\x0198=0\x01108=60\x0110=178\x01";
    // println!("Original message: {:?}", String::from_utf8_lossy(message))
    println!("Start test");

    for &b in message {
        // println!("Current byte: {}", b as char);
        match rx.feed_byte(b) {
            Ok(Some(parsed_message)) => {
                trace!("got message");
                trace!(tag = 35, got_tag = parsed_message.get_str(35));
                trace!(tag = 34, got_tag = parsed_message.get_str(34));
                trace!(tag = 49, got_tag = parsed_message.get_str(49));
                trace!(tag = 52, got_tag = parsed_message.get_str(52));
                trace!(tag = 56, got_tag = parsed_message.get_str(56));
                trace!(tag = 55, got_tag = parsed_message.get_str(55));
                trace!(tag = 38, got_tag = match parsed_message.get_int(38) {
                    Some(Ok(val)) => format!("{}", val),
                    Some(Err(_)) => "ParsingError".to_string(),
                    None => "Missing".to_string(),
                });
                rx.clear();
                // trace!(got_message = %String::from_utf8_lossy(parsed_message).replace('\x01', "|"));
            },
            Err(e) => {
                println!("Got an error: {}", e);
                return;
            },
            Ok(None) => {
                // Message is incomplete yet
            }
        }
    }

    println!("End.")

}