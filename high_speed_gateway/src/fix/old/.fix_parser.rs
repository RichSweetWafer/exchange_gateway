use std::char::MAX;


type Price = i64;

#[derive(Debug, Clone)]
pub struct FixParser<'symbol> {
    pub symbol: &'symbol str,
    pub bid_price: Price,
    pub bid_volume: i64,
    pub ask_price: Price,
    pub ask_volume: i64,
}

impl<'symbol> FixParser<'symbol> {
    pub fn parse(raw_bytes: &'symbol [u8]) -> Option<Self> {
        
        const MAX_NUMBER_OF_ENTRIES: i64 = 2;
        let mut number_of_entries = 0i64;

        let mut symbol = "";
        let mut bid_price: Price = 0;
        let mut bid_volume: i64 = 0;
        let mut ask_price: Price = 0;
        let mut ask_volume: i64 = 0;
        
        // Ask: 1, Bid: 0, Unknown: -1
        let mut current_entry_type: i32 = -1;

        // Parse each FIX field using split (zero-copy, lazy eval)
        for field in raw_bytes.split(|&byte| byte == b'\x01') {
            if field.is_empty() { continue }

            // splitn does the same, just with a counter
            let mut parts = field.splitn(2, |&byte| byte == b'=');
            let tag = parts.next()?;
            let val = parts.next()?;

            // Example string:
            // 8=FIX.4.4|9=122|35=W|55=BTCUSD|269=0|270=61250.50|271=2|269=1|270=61253.00|271=1|10=142|
            // Field types:
            // 9 - Message length in bytes (ignored)
            // 35 - Message Type (Unsolicited Market data snapshot) (required)
            // 55 - Ticker symbol (required)
            // 268 - Number of entries (required)
            // 269 - Market data entry type (ask, bid, etc.)
            // 270 - Price of the entry
            // 271 - Amount of the entry 
            // 10 - Checksum

            match tag {
                // Symbol tag
                b"55" => symbol = std::str::from_utf8(val).ok()?,
                // Number of entries
                b"268" => {
                    let val_str = std::str::from_utf8(val).ok()?;
                    number_of_entries = val_str.parse().ok()?;
                    if number_of_entries > MAX_NUMBER_OF_ENTRIES {
                        {}
                    }
                }
                // Ask/Bid tag
                b"269" => {
                    if number_of_entries <= 0 {
                        {}
                    }
                    number_of_entries -= 1;
                    if let Ok(val_str) = std::str::from_utf8(val) {
                        current_entry_type = val_str.parse().unwrap_or(-1);
                    }
                }
                // Price tag
                b"270" => {
                    match FixParser::parse_price(val) {
                        Some(price) => {
                            match current_entry_type {
                                0 => bid_price = price,
                                1 => ask_price = price,
                                _ => {}
                            }
                        },
                        None => {}
                    }
                }
                // Volume tag
                b"271" => {
                    let val_str = std::str::from_utf8(val).ok()?;
                    let volume = val_str.parse().ok()?;
                    
                    match current_entry_type {
                        0 => bid_volume = volume,
                        1 => ask_volume = volume,
                        _ => {}
                    }
                }
                _ => {} // Ignore other tags
            }
        }

            
        if symbol.is_empty() {
            None
        } else {
            Some(Self {symbol, bid_price, bid_volume, ask_price, ask_volume})
        }
    }


    #[inline(always)]
    fn parse_price(bytes: &[u8]) -> Option<Price> {
        if bytes.is_empty() {
            return None;
        }

        const PRICE_PRECISION: i64 = 4;

        let mut value: Price = 0;
        let mut is_neg = false;
        let mut iter = bytes.iter().peekable();

        if let Some(&&b'-') = iter.peek() {
            is_neg = true;
            let _ = iter.next();
        } else if let Some(&&b'+') = iter.peek() {
            let _ = iter.next();
        }

        let mut dot_seen = false;
        let mut decimals = 0;


        while let Some(&&b) = iter.peek() {
            match b {
                b'.' => {
                    if dot_seen { return None; }
                    dot_seen = true;
                    let _ = iter.next();
                }
                b'0'..=b'9' => {
                    let digit = (b - b'0') as i64;
                    
                    if dot_seen {
                        if decimals < PRICE_PRECISION {
                            value = value * 10 + digit;
                            decimals +=1;
                        }
                    } else {
                        value = value * 10 + digit;
                    }
                    let _ = iter.next();

                },
                b'\x01' | b' ' | b'\n' | b'\r' => { 
                    // slice got too many bytes
                    break;
                }
                _ => return None, // malformed
            }
        }
        
        let decimals_left = PRICE_PRECISION - decimals;
        if decimals_left > 0 {
            value *= 10 * decimals_left;
        }
        
        if is_neg {
            Some(-value)
        } else {
            Some(value)
        }
        
    }
}



#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parser() {
        
        struct TestCase {
            name: &'static str,
            input: &'static [u8],
            //                  symbol       bid  vol  ask  vol
            expected: Option<( &'static str, i64, i64, i64, i64)>
        }

        let cases = vec![
            TestCase {
                name: "Valid bid and ask message",
                input: b"8=FIX.4.4\x019=122\x0135=W\x0155=BTCUSD\x01269=0\x01270=61250.50\x01271=2\x01269=1\x01270=61253.00\x01271=1\x0110=142\x01",
                expected: Some(("BTCUSD", 612505000, 2, 612530000, 1)),
            },
            TestCase {
                name: "Valid bid and ask message #2",
                input: b"8=FIX.4.4\x019=122\x0135=W\x0155=ETHUSD\x01269=0\x01270=32174.99\x01271=5\x01269=1\x01270=61253.00\x01271=3\x0110=142\x01",
                expected: Some(("ETHUSD", 321749900, 5, 612530000, 3)), 
            },

        ];


        for case in cases {
            let result = FixParser::parse(case.input);

            match (result, case.expected) {
                (Some(actual), Some(expected)) => {
                    assert_eq!(actual.symbol, expected.0, "[FixParser-test] failed on {}: symbol mismatch", case.name);
                    assert_eq!(actual.bid_price, expected.1, "[FixParser-test] failed on {}: bid mismatch", case.name);
                    assert_eq!(actual.ask_price, expected.2, "[FixParser-test] failed on {}: ask mismatch", case.name);
                    // assert_eq!(actual.volume, expected.3, "[FixParser-test] failed on {}: volume mismatch", case.name);
                },
                (None, None) => {},
                (Some(actual), None) => {
                    panic!(
                        "[FixParser-test] failed on {}. Expected to reject, but it returned: {:?}",
                        case.name, actual
                    );
                },
                (None, Some(_)) => {
                    panic!(
                        "[FixParser-test] failed on {}. Expected to return, but it rejected",
                        case.name
                    );
                }
            }
        }
    }
}