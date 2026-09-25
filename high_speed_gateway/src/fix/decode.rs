
#[inline(always)]
pub fn to_u16(bytes: &[u8]) -> Option<u16> {
    let mut res = 0u32;

    if bytes.is_empty() || bytes.len() > 5 { return None; }
    for &b in bytes {
        if b < b'0' || b > b'9' { return None; }
        res = res * 10 + (b - b'0') as u32;
    }

    if res <= u16::MAX as u32 {
        Some(res as u16)
    } else {
        None // Value overflows u16
    }
}

#[inline(always)]
pub fn to_u32(bytes: &[u8]) -> Option<u32> {
    let mut res = 0u64;

    if bytes.is_empty() || bytes.len() > 10 { return None; }
    for &b in bytes {
        if b < b'0' || b > b'9' { return None; }
        res = res * 10 + (b - b'0') as u64;
    }

    if res <= u32::MAX as u64 {
        Some(res as u32)
    } else {
        None // Value overflows u32
    }
}

#[inline(always)]
pub fn to_u64(bytes: &[u8]) -> Option<u64> {
    let mut res = 0u64;

    if bytes.is_empty() || bytes.len() > 20 { return None; }
    for &b in bytes {
        if b < b'0' || b > b'9' { return None; }

        res = res.checked_mul(10)?.checked_add((b - b'0') as u64)?;
    }

    Some(res)
}

#[inline(always)]
pub fn to_usize(bytes: &[u8]) -> Option<usize> {
    
    let res = to_u64(bytes)?;

    if res <= usize::MAX as u64 {
        Some(res as usize)
    } else {
        None
    }
}

/// Fixed-point decimal as written on the wire: `mantissa / 10^scale`, so
/// "1.50" is (150, 2). Equality is by representation: 1.5 != 1.50.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decimal {
    pub mantissa: i64,
    pub scale: u8,
}

impl Decimal {
    pub const fn new(mantissa: i64, scale: u8) -> Self {
        Self { mantissa, scale }
    }

    pub fn is_positive(&self) -> bool {
        self.mantissa > 0
    }
}

/// "-12.345", "7", ".5". None when malformed or out of the i64 range.
#[inline(always)]
pub fn to_decimal(bytes: &[u8]) -> Option<Decimal> {
    let (negative, digits) = match bytes.strip_prefix(b"-") {
        Some(rest) => (true, rest),
        None => (false, bytes),
    };
    let mut mantissa = 0i64;
    let mut scale = 0u8;
    let mut seen_dot = false;
    let mut seen_digit = false;
    for &b in digits {
        match b {
            b'0'..=b'9' => {
                let d = (b - b'0') as i64;
                mantissa = mantissa.checked_mul(10)?;
                mantissa = if negative { mantissa.checked_sub(d)? } else { mantissa.checked_add(d)? };
                scale += seen_dot as u8;
                seen_digit = true;
            }
            b'.' if !seen_dot => seen_dot = true,
            _ => return None,
        }
    }
    seen_digit.then_some(Decimal { mantissa, scale })
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals() {
        assert_eq!(to_decimal(b"1.50"), Some(Decimal::new(150, 2)));
        assert_eq!(to_decimal(b"-12.345"), Some(Decimal::new(-12345, 3)));
        assert_eq!(to_decimal(b"7"), Some(Decimal::new(7, 0)));
        assert_eq!(to_decimal(b".5"), Some(Decimal::new(5, 1)));
        assert_eq!(to_decimal(b"5."), Some(Decimal::new(5, 0)));
        assert_eq!(to_decimal(b"-9223372036854775808"), Some(Decimal::new(i64::MIN, 0)));
        for bad in [&b""[..], b"-", b".", b"1.2.3", b"1e5", b"9223372036854775808", b"+1"] {
            assert_eq!(to_decimal(bad), None, "{:?}", String::from_utf8_lossy(bad));
        }
    }
}