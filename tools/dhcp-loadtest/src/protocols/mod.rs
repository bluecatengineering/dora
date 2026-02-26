pub mod v4;
pub mod v6;

pub(crate) fn xid_for(client_index: usize, stage: u8, attempt: usize) -> u32 {
    let mut xid = (client_index as u32).wrapping_mul(0x9e37_79b9);
    xid ^= (stage as u32) << 20;
    xid ^= attempt as u32;
    if xid == 0 { 1 } else { xid }
}

pub(crate) fn xid_for_v6(client_index: usize, stage: u8, attempt: usize) -> [u8; 3] {
    let mut xid = xid_for(client_index, stage, attempt) & 0x00ff_ffff;
    if xid == 0 {
        xid = 1;
    }
    [
        ((xid >> 16) & 0xff) as u8,
        ((xid >> 8) & 0xff) as u8,
        (xid & 0xff) as u8,
    ]
}
