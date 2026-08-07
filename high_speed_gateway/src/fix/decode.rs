use super::data_types::DataType;


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