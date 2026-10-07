/// Internet checksum (RFC 1071): sum 16-bit words, fold carries, ones-complement.
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        let word = (data[i] as u32) << 8 | data[i + 1] as u32;
        sum += word;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

pub fn tcp_pseudo_checksum(
    src: &[u8; 4],
    dst: &[u8; 4],
    proto: u8,
    tcp_len: u16,
    tcp: &[u8],
) -> u16 {
    let mut buf = [0u8; 12 + 1500];
    buf[0..4].copy_from_slice(src);
    buf[4..8].copy_from_slice(dst);
    buf[9] = proto;
    buf[10] = (tcp_len >> 8) as u8;
    buf[11] = (tcp_len & 0xff) as u8;
    let total = 12 + tcp.len();
    if total > buf.len() {
        return 0;
    }
    buf[12..12 + tcp.len()].copy_from_slice(tcp);
    checksum(&buf[..total])
}

pub fn verify_ipv4_checksum(hdr: &[u8]) -> bool {
    if hdr.len() < 20 {
        return false;
    }
    let stored = ((hdr[10] as u16) << 8) | hdr[11] as u16;
    let mut copy = [0u8; 20];
    copy.copy_from_slice(&hdr[..20]);
    copy[10] = 0;
    copy[11] = 0;
    checksum(&copy) == stored
}
