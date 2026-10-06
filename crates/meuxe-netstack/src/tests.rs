extern crate std;

use super::*;
use std::vec::Vec;

fn test_config() -> Config {
    Config {
        mac: Mac([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]),
        ip: Ipv4([10, 0, 2, 15]),
        prefix: 24,
        gateway: Ipv4([10, 0, 2, 2]),
    }
}

fn peer_mac() -> Mac {
    Mac([0x52, 0x55, 0x0a, 0x00, 0x02, 0x02])
}

fn build_arp_reply(sender_ip: [u8; 4], sender_mac: [u8; 6], target_ip: [u8; 4], target_mac: [u8; 6]) -> Vec<u8> {
    let mut f = std::vec![0u8; HDR + 28];
    f[0..6].copy_from_slice(&target_mac);
    f[6..12].copy_from_slice(&sender_mac);
    f[12] = 0x08;
    f[13] = 0x06;
    let arp = &mut f[HDR..];
    arp[0] = 0;
    arp[1] = 1;
    arp[2] = 0x08;
    arp[3] = 0x00;
    arp[4] = 6;
    arp[5] = 4;
    arp[6] = 0;
    arp[7] = 2;
    arp[8..14].copy_from_slice(&sender_mac);
    arp[14..18].copy_from_slice(&sender_ip);
    arp[18..24].copy_from_slice(&target_mac);
    arp[24..28].copy_from_slice(&target_ip);
    f
}

fn build_ipv4_icmp_echo_reply(
    dst_mac: [u8; 6],
    src_mac: [u8; 6],
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    id: u16,
    seq: u8,
) -> Vec<u8> {
    let icmp_len = 8;
    let ip_len = 20 + icmp_len;
    let mut f = std::vec![0u8; HDR + ip_len];
    f[0..6].copy_from_slice(&dst_mac);
    f[6..12].copy_from_slice(&src_mac);
    f[12] = 0x08;
    f[13] = 0x00;
    let ip = &mut f[HDR..HDR + 20];
    ip[0] = 0x45;
    ip[2] = (ip_len >> 8) as u8;
    ip[3] = (ip_len & 0xff) as u8;
    ip[8] = 64;
    ip[9] = 1;
    ip[12..16].copy_from_slice(&src_ip);
    ip[16..20].copy_from_slice(&dst_ip);
    let cs = checksum::checksum(ip);
    ip[10] = (cs >> 8) as u8;
    ip[11] = (cs & 0xff) as u8;
    let icmp = &mut f[HDR + 20..];
    icmp[0] = 0;
    icmp[1] = 0;
    icmp[4] = (id >> 8) as u8;
    icmp[5] = (id & 0xff) as u8;
    icmp[6] = 0;
    icmp[7] = seq;
    let ics = checksum::checksum(icmp);
    icmp[2] = (ics >> 8) as u8;
    icmp[3] = (ics & 0xff) as u8;
    f
}

fn build_tcp_segment(
    dst_mac: [u8; 6],
    src_mac: [u8; 6],
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    sport: u16,
    dport: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    data: &[u8],
) -> Vec<u8> {
    let tcp_len = 20 + data.len();
    let ip_len = 20 + tcp_len;
    let mut f = std::vec![0u8; HDR + ip_len];
    f[0..6].copy_from_slice(&dst_mac);
    f[6..12].copy_from_slice(&src_mac);
    f[12] = 0x08;
    f[13] = 0x00;
    let ip = &mut f[HDR..HDR + 20];
    ip[0] = 0x45;
    ip[2] = (ip_len >> 8) as u8;
    ip[3] = (ip_len & 0xff) as u8;
    ip[8] = 64;
    ip[9] = 6;
    ip[12..16].copy_from_slice(&src_ip);
    ip[16..20].copy_from_slice(&dst_ip);
    let ics = checksum::checksum(ip);
    ip[10] = (ics >> 8) as u8;
    ip[11] = (ics & 0xff) as u8;
    let tcp = &mut f[HDR + 20..HDR + 20 + tcp_len];
    tcp[0] = (sport >> 8) as u8;
    tcp[1] = (sport & 0xff) as u8;
    tcp[2] = (dport >> 8) as u8;
    tcp[3] = (dport & 0xff) as u8;
    tcp[4] = (seq >> 24) as u8;
    tcp[5] = (seq >> 16) as u8;
    tcp[6] = (seq >> 8) as u8;
    tcp[7] = (seq & 0xff) as u8;
    tcp[8] = (ack >> 24) as u8;
    tcp[9] = (ack >> 16) as u8;
    tcp[10] = (ack >> 8) as u8;
    tcp[11] = (ack & 0xff) as u8;
    tcp[12] = 0x50;
    tcp[13] = flags;
    tcp[14] = 0x10;
    tcp[15] = 0x00;
    tcp[20..20 + data.len()].copy_from_slice(data);
    let tcs = checksum::tcp_pseudo_checksum(&src_ip, &dst_ip, 6, tcp_len as u16, tcp);
    tcp[16] = (tcs >> 8) as u8;
    tcp[17] = (tcs & 0xff) as u8;
    f
}

#[test]
fn arp_request_for_peer() {
    let mut stack = Stack::new(test_config());
    let mut out = [0u8; 1514];
    let n = stack.ping(Ipv4([10, 0, 2, 2]), &mut out).expect("ping");
    assert!(n >= HDR + 28);
    assert_eq!(&out[0..6], &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    assert_eq!(&out[6..12], &test_config().mac.0);
    assert_eq!(out[12], 0x08);
    assert_eq!(out[13], 0x06);
    let arp = &out[HDR..HDR + 28];
    assert_eq!(arp[6], 0);
    assert_eq!(arp[7], 1);
    assert_eq!(&arp[8..14], &test_config().mac.0);
    assert_eq!(&arp[14..18], &[10, 0, 2, 15]);
    assert_eq!(&arp[24..28], &[10, 0, 2, 2]);

    let reply = build_arp_reply(
        [10, 0, 2, 2],
        peer_mac().0,
        [10, 0, 2, 15],
        test_config().mac.0,
    );
    stack.recv(&reply, &mut out);
    assert_eq!(stack.arp(Ipv4([10, 0, 2, 2])), Some(peer_mac()));
}

#[test]
fn ping_after_arp() {
    let mut stack = Stack::new(test_config());
    let mut out = [0u8; 1514];
    let reply = build_arp_reply(
        [10, 0, 2, 2],
        peer_mac().0,
        [10, 0, 2, 15],
        test_config().mac.0,
    );
    stack.recv(&reply, &mut out);
    stack.ping(Ipv4([10, 0, 2, 2]), &mut out).unwrap();
    assert_eq!(&out[0..6], &peer_mac().0);
    assert_eq!(out[12], 0x08);
    assert_eq!(out[13], 0x00);

    let echo_reply = build_ipv4_icmp_echo_reply(
        test_config().mac.0,
        peer_mac().0,
        [10, 0, 2, 2],
        [10, 0, 2, 15],
        0x4d58,
        0,
    );
    stack.recv(&echo_reply, &mut out);
    match stack.ping_status() {
        PingStatus::Active { received, .. } => assert_eq!(received, 1),
        _ => panic!("expected active ping"),
    }

    for seq in 1..4 {
        let er = build_ipv4_icmp_echo_reply(
            test_config().mac.0,
            peer_mac().0,
            [10, 0, 2, 2],
            [10, 0, 2, 15],
            0x4d58,
            seq,
        );
        stack.recv(&er, &mut out);
    }
    assert_eq!(
        stack.ping_status(),
        PingStatus::Active {
            sent: 4,
            received: 4,
            rtt_ticks: [0, 0, 0, 0],
        }
    );
}

#[test]
fn inbound_arp_request_gets_reply() {
    let mut stack = Stack::new(test_config());
    let mut out = [0u8; 1514];
    let mut req = std::vec![0u8; HDR + 28];
    req[0..6].copy_from_slice(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    req[6..12].copy_from_slice(&peer_mac().0);
    req[12] = 0x08;
    req[13] = 0x06;
    let arp = &mut req[HDR..];
    arp[0] = 0;
    arp[1] = 1;
    arp[2] = 0x08;
    arp[3] = 0x00;
    arp[4] = 6;
    arp[5] = 4;
    arp[6] = 0;
    arp[7] = 1;
    arp[8..14].copy_from_slice(&peer_mac().0);
    arp[14..18].copy_from_slice(&[10, 0, 2, 2]);
    arp[24..28].copy_from_slice(&[10, 0, 2, 15]);
    let n = stack.recv(&req, &mut out).expect("reply");
    assert!(n > 0);
    assert_eq!(&out[6..12], &test_config().mac.0);
    assert_eq!(out[HDR + 7], 2);
}

#[test]
fn inbound_icmp_echo_request() {
    let mut stack = Stack::new(test_config());
    let mut out = [0u8; 1514];
    let mut f = std::vec![0u8; HDR + 28];
    f[6..12].copy_from_slice(&peer_mac().0);
    f[0..6].copy_from_slice(&test_config().mac.0);
    f[12] = 0x08;
    f[13] = 0x00;
    let ip = &mut f[HDR..HDR + 20];
    ip[0] = 0x45;
    ip[2] = 0;
    ip[3] = 28;
    ip[8] = 64;
    ip[9] = 1;
    ip[12..16].copy_from_slice(&[10, 0, 2, 2]);
    ip[16..20].copy_from_slice(&[10, 0, 2, 15]);
    let cs = checksum::checksum(ip);
    ip[10] = (cs >> 8) as u8;
    ip[11] = (cs & 0xff) as u8;
    f[HDR + 20] = 8;
    let icmp = &mut f[HDR + 20..HDR + 28];
    let ics = checksum::checksum(icmp);
    icmp[2] = (ics >> 8) as u8;
    icmp[3] = (ics & 0xff) as u8;
    let n = stack.recv(&f, &mut out).expect("echo reply");
    assert!(n > 0);
    assert_eq!(out[HDR + 20], 0);
}

#[test]
fn bad_ipv4_checksum_dropped() {
    let mut stack = Stack::new(test_config());
    let mut out = [0u8; 1514];
    let tx_before = stack.counters().tx_frames;
    let mut f = std::vec![0u8; HDR + 28];
    f[12] = 0x08;
    f[13] = 0x00;
    f[HDR + 10] = 0xff;
    f[HDR + 11] = 0xff;
    assert!(stack.recv(&f, &mut out).is_none());
    assert_eq!(stack.counters().tx_frames, tx_before);
}

#[test]
fn http_get_flow() {
    let mut stack = Stack::new(test_config());
    let mut out = [0u8; 1514];
    stack
        .http_get(Ipv4([10, 0, 2, 100]), 80, b"/", &mut out)
        .unwrap();
    let arp_reply = build_arp_reply(
        [10, 0, 2, 2],
        peer_mac().0,
        [10, 0, 2, 15],
        test_config().mac.0,
    );
    if stack.recv(&arp_reply, &mut out).is_none() {
        stack.after_arp(&mut out).expect("syn after arp");
    }
    assert_eq!(out[HDR + 33], 0x02, "expected SYN segment after ARP");

    let peer_ip = [10, 0, 2, 100];
    let syn_ack = build_tcp_segment(
        test_config().mac.0,
        peer_mac().0,
        peer_ip,
        [10, 0, 2, 15],
        80,
        49152,
        5000,
        0x1001,
        0x12,
        &[],
    );
    let n = stack.recv(&syn_ack, &mut out).expect("http request");
    assert!(
        out[HDR + 20..n].windows(14).any(|w| w == b"GET / HTTP/1.0"),
        "expected GET / HTTP/1.0 in outbound TCP"
    );
    let payload = b"HTTP/1.0 200 OK\r\nContent-Length: 11\r\n\r\nmeuxe-alpha";
    let data_fin = build_tcp_segment(
        test_config().mac.0,
        peer_mac().0,
        peer_ip,
        [10, 0, 2, 15],
        80,
        49152,
        5001,
        0,
        0x11,
        payload,
    );
    stack.recv(&data_fin, &mut out);
    assert_eq!(stack.body(), b"meuxe-alpha");
    match stack.http_status() {
        HttpStatus::Active { status_code, state } => {
            assert_eq!(status_code, 200);
            assert!(state == TcpState::TimeWait || state == TcpState::FinWait);
        }
        _ => panic!("expected active http"),
    }
}

#[test]
fn arp_gateway_for_remote() {
    let mut stack = Stack::new(test_config());
    let mut out = [0u8; 1514];
    stack.ping(Ipv4([1, 2, 3, 4]), &mut out).unwrap();
    let arp = &out[HDR..HDR + 28];
    assert_eq!(&arp[24..28], &[10, 0, 2, 2]);
}
