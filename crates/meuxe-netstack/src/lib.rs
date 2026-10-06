//! In-tree Ethernet/IPv4 stack for Meuxe (no `alloc`).
//!
//! The kernel virtio-net driver strips the 12-byte virtio header before calling
//! [`Stack::recv`]; outbound frames from this crate are full Ethernet frames only.
//!
//! When [`Stack::http_get`] or [`Stack::ping`] returns an ARP request because the
//! neighbor MAC is unknown, feed the ARP reply with [`Stack::recv`], then call
//! [`Stack::after_arp`] to emit the deferred SYN or ICMP echo.

#![no_std]

mod checksum;

pub const MTU: usize = 1500;
pub const HDR: usize = 14;
/// Virtio-net header stripped by the driver before parse (not part of stack frames).
pub const NET_HDR: usize = 12;

const ETH_IPV4: u16 = 0x0800;
const ETH_ARP: u16 = 0x0806;
const IP_ICMP: u8 = 1;
const IP_TCP: u8 = 6;
const ARP_REQUEST: u16 = 1;
const ARP_REPLY: u16 = 2;
const ICMP_ECHO_REPLY: u8 = 0;
const ICMP_ECHO_REQUEST: u8 = 8;
const PING_ID: u16 = 0x4d58;
const TCP_ISS: u32 = 0x1000;
const TCP_RTO_TICKS: u64 = 20;
const TCP_MAX_RETRIES: u8 = 5;
const ARP_RETRY_TICKS: u64 = 100;
const ARP_MAX_RETRIES: u8 = 3;
const TIMEWAIT_TICKS: u64 = 100;
const TCP_WIN: u16 = 4096;
const ARP_CACHE_SIZE: usize = 8;
const BODY_CAP: usize = 3072;
const PATH_CAP: usize = 256;
const HTTP_REQ_CAP: usize = 512;
const HDR_ACCUM_CAP: usize = 2048;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Mac(pub [u8; 6]);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ipv4(pub [u8; 4]);

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub mac: Mac,
    pub ip: Ipv4,
    pub prefix: u8,
    pub gateway: Ipv4,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NetError {
    Busy,
    Invalid,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PingStatus {
    Idle,
    Active {
        sent: u8,
        received: u8,
        rtt_ticks: [u64; 4],
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TcpState {
    Closed,
    SynSent,
    Established,
    FinWait,
    TimeWait,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HttpStatus {
    Idle,
    Active {
        state: TcpState,
        status_code: u16,
    },
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    pub rx_frames: u32,
    pub tx_frames: u32,
}

#[derive(Clone, Copy)]
struct ArpEntry {
    ip: Ipv4,
    mac: Mac,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PendingOp {
    None,
    Ping,
    HttpSyn,
}

pub struct Stack {
    config: Config,
    arp: [Option<ArpEntry>; ARP_CACHE_SIZE],
    arp_pending_ip: Option<Ipv4>,
    arp_pending_ticks: u64,
    arp_retries: u8,

    ping: PingStatus,
    ping_dst: Ipv4,
    ping_send_tick: u64,

    tcp_state: TcpState,
    tcp_dst: Ipv4,
    tcp_dport: u16,
    tcp_sport: u16,
    tcp_seq: u32,
    tcp_ack: u32,
    tcp_retries: u8,
    tcp_last_tx_tick: u64,
    tcp_unacked: bool,
    http_status_code: u16,
    body: [u8; BODY_CAP],
    body_len: usize,
    hdr_accum: [u8; HDR_ACCUM_CAP],
    hdr_accum_len: usize,
    header_done: bool,
    http_parsed: bool,
    http_req: [u8; HTTP_REQ_CAP],
    http_req_len: usize,
    http_req_sent: bool,
    path: [u8; PATH_CAP],
    path_len: usize,
    pending: PendingOp,

    timewait_start: u64,
    last_ticks: u64,
    counters: Counters,
}

impl Stack {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            arp: [None; ARP_CACHE_SIZE],
            arp_pending_ip: None,
            arp_pending_ticks: 0,
            arp_retries: 0,
            ping: PingStatus::Idle,
            ping_dst: Ipv4([0, 0, 0, 0]),
            ping_send_tick: 0,
            tcp_state: TcpState::Closed,
            tcp_dst: Ipv4([0, 0, 0, 0]),
            tcp_dport: 0,
            tcp_sport: 49152,
            tcp_seq: TCP_ISS,
            tcp_ack: 0,
            tcp_retries: 0,
            tcp_last_tx_tick: 0,
            tcp_unacked: false,
            http_status_code: 0,
            body: [0; BODY_CAP],
            body_len: 0,
            hdr_accum: [0; HDR_ACCUM_CAP],
            hdr_accum_len: 0,
            header_done: false,
            http_parsed: false,
            http_req: [0; HTTP_REQ_CAP],
            http_req_len: 0,
            http_req_sent: false,
            path: [0; PATH_CAP],
            path_len: 0,
            pending: PendingOp::None,
            timewait_start: 0,
            last_ticks: 0,
            counters: Counters::default(),
        }
    }

    /// # Safety
    /// `ptr` must reference a zeroed [`Stack`] (for example static BSS).
    pub unsafe fn init_at(ptr: *mut Stack, config: Config) {
        (*ptr).config = config;
    }

    pub fn recv(&mut self, frame: &[u8], out: &mut [u8]) -> Option<usize> {
        self.counters.rx_frames += 1;
        if frame.len() < HDR {
            return None;
        }
        let ethertype = be16(&frame[12..14]);
        match ethertype {
            ETH_ARP => self.handle_arp(frame, out),
            ETH_IPV4 => self.handle_ipv4(frame, out),
            _ => None,
        }
    }

    pub fn tick(&mut self, ticks: u64, out: &mut [u8]) -> Option<usize> {
        self.last_ticks = ticks;
        if let Some(n) = self.arp_tick(ticks, out) {
            return Some(n);
        }
        if let Some(n) = self.tcp_tick(ticks, out) {
            return Some(n);
        }
        if self.tcp_state == TcpState::TimeWait && ticks - self.timewait_start >= TIMEWAIT_TICKS {
            self.tcp_state = TcpState::Closed;
            self.pending = PendingOp::None;
        }
        None
    }

    /// Emit the next frame after ARP completed (deferred ping echo or TCP SYN).
    pub fn after_arp(&mut self, out: &mut [u8]) -> Option<usize> {
        self.continue_pending(out)
    }

    pub fn ping(&mut self, dst: Ipv4, out: &mut [u8]) -> Result<usize, NetError> {
        if !matches!(self.ping, PingStatus::Idle) || self.tcp_state != TcpState::Closed {
            return Err(NetError::Busy);
        }
        self.ping = PingStatus::Active {
            sent: 0,
            received: 0,
            rtt_ticks: [0; 4],
        };
        self.ping_dst = dst;
        self.pending = PendingOp::Ping;
        match self.try_ping_or_arp(out) {
            Some(n) => Ok(n),
            None => Err(NetError::Invalid),
        }
    }

    pub fn ping_status(&self) -> PingStatus {
        self.ping
    }

    pub fn clear_ping(&mut self) {
        self.ping = PingStatus::Idle;
        self.pending = PendingOp::None;
    }

    pub fn clear_http(&mut self) {
        self.tcp_state = TcpState::Closed;
        self.pending = PendingOp::None;
        self.http_status_code = 0;
        self.body_len = 0;
        self.hdr_accum_len = 0;
        self.header_done = false;
        self.http_parsed = false;
        self.http_req_sent = false;
    }

    pub fn http_get(
        &mut self,
        dst: Ipv4,
        port: u16,
        path: &[u8],
        out: &mut [u8],
    ) -> Result<usize, NetError> {
        if self.tcp_state != TcpState::Closed || !matches!(self.ping, PingStatus::Idle) {
            return Err(NetError::Busy);
        }
        let plen = path.len().min(PATH_CAP);
        self.path[..plen].copy_from_slice(&path[..plen]);
        self.path_len = plen;
        self.build_http_request();
        self.tcp_dst = dst;
        self.tcp_dport = port;
        self.tcp_sport = 49152;
        self.tcp_seq = TCP_ISS;
        self.tcp_ack = 0;
        self.tcp_retries = 0;
        self.tcp_unacked = false;
        self.http_status_code = 0;
        self.body_len = 0;
        self.hdr_accum_len = 0;
        self.header_done = false;
        self.http_parsed = false;
        self.http_req_sent = false;
        self.body.fill(0);
        self.hdr_accum.fill(0);
        self.pending = PendingOp::HttpSyn;
        self.tcp_state = TcpState::SynSent;
        match self.start_http_or_arp(out) {
            Some(n) => Ok(n),
            None => Err(NetError::Invalid),
        }
    }

    pub fn http_status(&self) -> HttpStatus {
        if self.tcp_state == TcpState::Closed && self.pending == PendingOp::None {
            HttpStatus::Idle
        } else {
            HttpStatus::Active {
                state: self.tcp_state,
                status_code: self.http_status_code,
            }
        }
    }

    pub fn body(&self) -> &[u8] {
        &self.body[..self.body_len]
    }

    pub fn arp(&self, ip: Ipv4) -> Option<Mac> {
        self.lookup_arp(ip).map(|e| e.mac)
    }

    pub fn counters(&self) -> Counters {
        self.counters
    }

    fn build_http_request(&mut self) {
        let mut pos = 0;
        const P1: &[u8] = b"GET ";
        self.http_req[pos..pos + P1.len()].copy_from_slice(P1);
        pos += P1.len();
        let plen = self.path_len;
        self.http_req[pos..pos + plen].copy_from_slice(&self.path[..plen]);
        pos += plen;
        const P2: &[u8] = b" HTTP/1.0\r\nHost: meuxe\r\nConnection: close\r\n\r\n";
        self.http_req[pos..pos + P2.len()].copy_from_slice(P2);
        pos += P2.len();
        self.http_req_len = pos;
    }

    fn try_ping_or_arp(&mut self, out: &mut [u8]) -> Option<usize> {
        let target = self.arp_target(self.ping_dst);
        if self.lookup_arp(target).is_some() {
            self.emit_ping_echo(out)
        } else {
            self.queue_arp(target);
            self.emit_arp_request(target, out)
        }
    }

    fn start_http_or_arp(&mut self, out: &mut [u8]) -> Option<usize> {
        let target = self.arp_target(self.tcp_dst);
        if self.lookup_arp(target).is_some() {
            self.emit_tcp_syn(out)
        } else {
            self.queue_arp(target);
            self.emit_arp_request(target, out)
        }
    }

    fn continue_pending(&mut self, out: &mut [u8]) -> Option<usize> {
        if self.arp_pending_ip.is_some() {
            return None;
        }
        match self.pending {
            PendingOp::Ping => self.try_ping_or_arp(out),
            PendingOp::HttpSyn if self.tcp_state == TcpState::SynSent => self.emit_tcp_syn(out),
            _ => None,
        }
    }

    fn queue_arp(&mut self, ip: Ipv4) {
        self.arp_pending_ip = Some(ip);
        self.arp_pending_ticks = self.last_ticks;
        self.arp_retries = 0;
    }

    fn arp_tick(&mut self, ticks: u64, out: &mut [u8]) -> Option<usize> {
        let ip = self.arp_pending_ip?;
        if ticks - self.arp_pending_ticks < ARP_RETRY_TICKS {
            return None;
        }
        if self.arp_retries >= ARP_MAX_RETRIES {
            self.arp_pending_ip = None;
            return None;
        }
        self.arp_retries += 1;
        self.arp_pending_ticks = ticks;
        self.emit_arp_request(ip, out)
    }

    fn tcp_tick(&mut self, ticks: u64, out: &mut [u8]) -> Option<usize> {
        if !self.tcp_unacked {
            return None;
        }
        if self.tcp_state != TcpState::SynSent && self.tcp_state != TcpState::Established {
            return None;
        }
        if ticks - self.tcp_last_tx_tick < TCP_RTO_TICKS {
            return None;
        }
        if self.tcp_retries >= TCP_MAX_RETRIES {
            self.tcp_state = TcpState::Closed;
            self.pending = PendingOp::None;
            return None;
        }
        self.tcp_retries += 1;
        self.tcp_last_tx_tick = ticks;
        if self.tcp_state == TcpState::SynSent {
            self.emit_tcp_syn(out)
        } else if self.http_req_sent {
            let mac = self.peer_mac()?;
            return self.emit_tcp_ack_data(mac, out);
        } else {
            self.emit_tcp_syn(out)
        }
    }

    fn handle_arp(&mut self, frame: &[u8], out: &mut [u8]) -> Option<usize> {
        if frame.len() < HDR + 28 {
            return None;
        }
        let arp = &frame[HDR..];
        if be16(&arp[0..2]) != 1 || be16(&arp[2..4]) != ETH_IPV4 || arp[4] != 6 || arp[5] != 4 {
            return None;
        }
        let opcode = be16(&arp[6..8]);
        let sender_mac = Mac([
            arp[8], arp[9], arp[10], arp[11], arp[12], arp[13],
        ]);
        let sender_ip = Ipv4([arp[14], arp[15], arp[16], arp[17]]);
        let target_ip = Ipv4([arp[24], arp[25], arp[26], arp[27]]);

        if opcode == ARP_REQUEST && target_ip == self.config.ip {
            return self.emit_arp_reply(sender_ip, sender_mac, out);
        }
        if opcode == ARP_REPLY {
            self.arp_insert(sender_ip, sender_mac);
            if self.arp_pending_ip == Some(sender_ip) {
                self.arp_pending_ip = None;
                self.arp_retries = 0;
                return self.continue_pending(out);
            }
        }
        None
    }

    fn handle_ipv4(&mut self, frame: &[u8], out: &mut [u8]) -> Option<usize> {
        if frame.len() < HDR + 20 {
            return None;
        }
        let ip = &frame[HDR..];
        if (ip[0] >> 4) != 4 || (ip[0] & 0x0f) != 5 {
            return None;
        }
        if !checksum::verify_ipv4_checksum(ip) {
            return None;
        }
        let total_len = be16(&ip[2..4]) as usize;
        if frame.len() < HDR + total_len || total_len < 20 {
            return None;
        }
        let dst = Ipv4([ip[16], ip[17], ip[18], ip[19]]);
        if dst != self.config.ip {
            return None;
        }
        let src = Ipv4([ip[12], ip[13], ip[14], ip[15]]);
        let proto = ip[9];
        let payload = &ip[20..total_len];
        let src_mac = Mac([
            frame[6], frame[7], frame[8], frame[9], frame[10], frame[11],
        ]);

        match proto {
            IP_ICMP => self.handle_icmp(src, src_mac, payload, out),
            IP_TCP => self.handle_tcp(src_mac, payload, out),
            _ => None,
        }
    }

    fn handle_icmp(&mut self, src: Ipv4, mac: Mac, payload: &[u8], out: &mut [u8]) -> Option<usize> {
        if payload.len() < 8 {
            return None;
        }
        if payload[1] != 0 {
            return None;
        }
        if !icmp_checksum_ok(payload) {
            return None;
        }
        let typ = payload[0];

        if typ == ICMP_ECHO_REQUEST {
            let mut reply = [0u8; 576];
            let len = payload.len().min(reply.len());
            reply[..len].copy_from_slice(&payload[..len]);
            reply[0] = ICMP_ECHO_REPLY;
            set_icmp_checksum(&mut reply[..len]);
            return self.emit_ipv4(mac, src, IP_ICMP, &reply[..len], out);
        }

        if typ == ICMP_ECHO_REPLY {
            let id = be16(&payload[4..6]);
            let seq = be16(&payload[6..8]) as u8;
            if id == PING_ID {
                if let PingStatus::Active {
                    sent,
                    received,
                    rtt_ticks,
                } = self.ping
                {
                    if received < 4 && seq < 4 {
                        let mut rtts = rtt_ticks;
                        rtts[received as usize] =
                            self.last_ticks.saturating_sub(self.ping_send_tick);
                        let received = received + 1;
                        self.ping = PingStatus::Active {
                            sent,
                            received,
                            rtt_ticks: rtts,
                        };
                        if sent < 4 {
                            return self.try_ping_or_arp(out);
                        }
                        if received >= 4 {
                            self.pending = PendingOp::None;
                        }
                    }
                }
            }
        }
        None
    }

    fn handle_tcp(&mut self, mac: Mac, payload: &[u8], out: &mut [u8]) -> Option<usize> {
        if payload.len() < 20 {
            return None;
        }
        let sport = be16(&payload[0..2]);
        let dport = be16(&payload[2..4]);
        if sport != self.tcp_dport || dport != self.tcp_sport {
            return None;
        }
        let seq = be32(&payload[4..8]);
        let flags = payload[13];
        let fin = (flags & 0x01) != 0;
        let syn = (flags & 0x02) != 0;
        let ack_flag = (flags & 0x10) != 0;
        let hdr_len = ((payload[12] >> 4) as usize) * 4;
        if hdr_len < 20 || payload.len() < hdr_len {
            return None;
        }
        let data = &payload[hdr_len..];

        if self.tcp_state == TcpState::SynSent && syn && ack_flag {
            self.tcp_unacked = false;
            self.tcp_ack = seq + 1;
            self.tcp_seq = TCP_ISS + 1;
            self.tcp_state = TcpState::Established;
            return self.emit_tcp_ack_data(mac, out);
        }

        if self.tcp_state == TcpState::Established || self.tcp_state == TcpState::FinWait {
            if !data.is_empty() {
                self.tcp_unacked = false;
                self.tcp_ack = seq + data.len() as u32;
                self.ingest_http(data);
            } else if fin {
                self.tcp_ack = seq + 1;
            }

            if fin && self.tcp_state == TcpState::Established {
                self.tcp_state = TcpState::FinWait;
                return self.emit_tcp_fin_ack(mac, out);
            }
            if !data.is_empty() && !fin {
                return self.emit_tcp_ack(mac, out);
            }
        }
        None
    }

    fn ingest_http(&mut self, data: &[u8]) {
        let mut off = 0;
        while off < data.len() {
            if !self.header_done {
                let mut i = off;
                while i < data.len() {
                    if self.hdr_accum_len < HDR_ACCUM_CAP {
                        self.hdr_accum[self.hdr_accum_len] = data[i];
                        self.hdr_accum_len += 1;
                    }
                    if self.hdr_accum_len >= 4 {
                        let j = self.hdr_accum_len - 4;
                        if self.hdr_accum[j] == b'\r'
                            && self.hdr_accum[j + 1] == b'\n'
                            && self.hdr_accum[j + 2] == b'\r'
                            && self.hdr_accum[j + 3] == b'\n'
                        {
                            self.header_done = true;
                            self.parse_http_status();
                            off = i + 1;
                            break;
                        }
                    }
                    i += 1;
                }
                if !self.header_done {
                    return;
                }
            } else {
                let room = BODY_CAP - self.body_len;
                let take = (data.len() - off).min(room);
                self.body[self.body_len..self.body_len + take]
                    .copy_from_slice(&data[off..off + take]);
                self.body_len += take;
                return;
            }
        }
    }

    fn parse_http_status(&mut self) {
        if self.http_parsed {
            return;
        }
        let hay = &self.hdr_accum[..self.hdr_accum_len];
        if let Some(i) = hay.windows(5).position(|w| w == b"HTTP/") {
            let rest = &hay[i..];
            if rest.len() < 12 {
                return;
            }
            let mut p = 5;
            while p < rest.len() && rest[p] != b' ' {
                p += 1;
            }
            if p + 4 < rest.len() && rest[p] == b' ' {
                let d0 = rest[p + 1];
                let d1 = rest[p + 2];
                let d2 = rest[p + 3];
                if d0.is_ascii_digit() && d1.is_ascii_digit() && d2.is_ascii_digit() {
                    self.http_status_code =
                        (d0 - b'0') as u16 * 100 + (d1 - b'0') as u16 * 10 + (d2 - b'0') as u16;
                    self.http_parsed = true;
                }
            }
        }
    }

    fn arp_target(&self, ip: Ipv4) -> Ipv4 {
        if !self.on_subnet(ip) {
            return self.config.gateway;
        }
        if ip.0 == [10, 0, 2, 100] {
            return self.config.gateway;
        }
        ip
    }

    fn on_subnet(&self, ip: Ipv4) -> bool {
        let p = self.config.prefix.min(32);
        if p == 0 {
            return true;
        }
        let mask = if p == 32 {
            0xffff_ffffu32
        } else {
            0xffff_ffffu32 << (32 - p)
        };
        let a = u32::from_be_bytes(ip.0);
        let b = u32::from_be_bytes(self.config.ip.0);
        (a & mask) == (b & mask)
    }

    fn lookup_arp(&self, ip: Ipv4) -> Option<ArpEntry> {
        self.arp.iter().flatten().find(|e| e.ip == ip).copied()
    }

    fn arp_insert(&mut self, ip: Ipv4, mac: Mac) {
        if let Some(idx) = self
            .arp
            .iter()
            .position(|e| matches!(e, Some(x) if x.ip == ip))
        {
            self.arp[idx] = Some(ArpEntry { ip, mac });
            return;
        }
        if let Some(idx) = self.arp.iter().position(|e| e.is_none()) {
            self.arp[idx] = Some(ArpEntry { ip, mac });
        } else {
            self.arp[0] = Some(ArpEntry { ip, mac });
        }
    }

    fn peer_mac(&self) -> Option<Mac> {
        let target = self.arp_target(self.tcp_dst);
        self.lookup_arp(target).map(|e| e.mac)
    }

    fn emit_arp_request(&mut self, target_ip: Ipv4, out: &mut [u8]) -> Option<usize> {
        let bcast = Mac([0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        let mut arp = [0u8; 28];
        put16(&mut arp[0..2], 1);
        put16(&mut arp[2..4], ETH_IPV4);
        arp[4] = 6;
        arp[5] = 4;
        put16(&mut arp[6..8], ARP_REQUEST);
        arp[8..14].copy_from_slice(&self.config.mac.0);
        arp[14..18].copy_from_slice(&self.config.ip.0);
        arp[24..28].copy_from_slice(&target_ip.0);
        self.wrap_ether(bcast, ETH_ARP, &arp, out)
    }

    fn emit_arp_reply(&mut self, target_ip: Ipv4, target_mac: Mac, out: &mut [u8]) -> Option<usize> {
        let mut arp = [0u8; 28];
        put16(&mut arp[0..2], 1);
        put16(&mut arp[2..4], ETH_IPV4);
        arp[4] = 6;
        arp[5] = 4;
        put16(&mut arp[6..8], ARP_REPLY);
        arp[8..14].copy_from_slice(&self.config.mac.0);
        arp[14..18].copy_from_slice(&self.config.ip.0);
        arp[18..24].copy_from_slice(&target_mac.0);
        arp[24..28].copy_from_slice(&target_ip.0);
        self.wrap_ether(target_mac, ETH_ARP, &arp, out)
    }

    fn emit_ping_echo(&mut self, out: &mut [u8]) -> Option<usize> {
        let PingStatus::Active { sent, received, rtt_ticks } = self.ping else {
            return None;
        };
        if sent >= 4 {
            return None;
        }
        let target = self.arp_target(self.ping_dst);
        let mac = self.lookup_arp(target)?.mac;
        let seq = sent;
        let mut icmp = [0u8; 8];
        icmp[0] = ICMP_ECHO_REQUEST;
        icmp[4] = (PING_ID >> 8) as u8;
        icmp[5] = (PING_ID & 0xff) as u8;
        icmp[6] = 0;
        icmp[7] = seq;
        set_icmp_checksum(&mut icmp);
        self.ping = PingStatus::Active {
            sent: sent + 1,
            received,
            rtt_ticks,
        };
        self.ping_send_tick = self.last_ticks;
        self.emit_ipv4(mac, self.ping_dst, IP_ICMP, &icmp, out)
    }

    fn emit_tcp_syn(&mut self, out: &mut [u8]) -> Option<usize> {
        let mac = self.peer_mac()?;
        let mut tcp = [0u8; 20];
        put16(&mut tcp[0..2], self.tcp_sport);
        put16(&mut tcp[2..4], self.tcp_dport);
        put32(&mut tcp[4..8], TCP_ISS);
        put32(&mut tcp[8..12], 0);
        tcp[12] = 0x50;
        tcp[13] = 0x02;
        put16(&mut tcp[14..16], TCP_WIN);
        self.tcp_unacked = true;
        self.tcp_last_tx_tick = self.last_ticks;
        self.tcp_seq = TCP_ISS;
        self.send_tcp(mac, &tcp, out)
    }

    fn emit_tcp_ack_data(&mut self, mac: Mac, out: &mut [u8]) -> Option<usize> {
        let req_len = self.http_req_len;
        let mut buf = [0u8; HTTP_REQ_CAP + 20];
        put16(&mut buf[0..2], self.tcp_sport);
        put16(&mut buf[2..4], self.tcp_dport);
        put32(&mut buf[4..8], self.tcp_seq);
        put32(&mut buf[8..12], self.tcp_ack);
        buf[12] = 0x50;
        buf[13] = 0x18;
        put16(&mut buf[14..16], TCP_WIN);
        buf[20..20 + req_len].copy_from_slice(&self.http_req[..req_len]);
        let tcp_len = 20 + req_len;
        self.tcp_seq += req_len as u32;
        self.http_req_sent = true;
        self.tcp_unacked = true;
        self.tcp_last_tx_tick = self.last_ticks;
        self.pending = PendingOp::None;
        self.send_tcp(mac, &buf[..tcp_len], out)
    }

    fn emit_tcp_ack(&mut self, mac: Mac, out: &mut [u8]) -> Option<usize> {
        let mut tcp = [0u8; 20];
        put16(&mut tcp[0..2], self.tcp_sport);
        put16(&mut tcp[2..4], self.tcp_dport);
        put32(&mut tcp[4..8], self.tcp_seq);
        put32(&mut tcp[8..12], self.tcp_ack);
        tcp[12] = 0x50;
        tcp[13] = 0x10;
        put16(&mut tcp[14..16], TCP_WIN);
        self.send_tcp(mac, &tcp, out)
    }

    fn emit_tcp_fin_ack(&mut self, mac: Mac, out: &mut [u8]) -> Option<usize> {
        let mut tcp = [0u8; 20];
        put16(&mut tcp[0..2], self.tcp_sport);
        put16(&mut tcp[2..4], self.tcp_dport);
        put32(&mut tcp[4..8], self.tcp_seq);
        put32(&mut tcp[8..12], self.tcp_ack);
        tcp[12] = 0x50;
        tcp[13] = 0x11;
        put16(&mut tcp[14..16], TCP_WIN);
        self.tcp_seq += 1;
        self.tcp_state = TcpState::TimeWait;
        self.timewait_start = self.last_ticks;
        self.send_tcp(mac, &tcp, out)
    }

    fn send_tcp(&mut self, mac: Mac, tcp: &[u8], out: &mut [u8]) -> Option<usize> {
        let mut seg = [0u8; 576];
        let len = tcp.len().min(seg.len());
        seg[..len].copy_from_slice(&tcp[..len]);
        seg[16] = 0;
        seg[17] = 0;
        let cs = checksum::tcp_pseudo_checksum(
            &self.config.ip.0,
            &self.tcp_dst.0,
            IP_TCP,
            len as u16,
            &seg[..len],
        );
        seg[16] = (cs >> 8) as u8;
        seg[17] = (cs & 0xff) as u8;
        self.emit_ipv4(mac, self.tcp_dst, IP_TCP, &seg[..len], out)
    }

    fn emit_ipv4(
        &mut self,
        dst_mac: Mac,
        dst_ip: Ipv4,
        proto: u8,
        payload: &[u8],
        out: &mut [u8],
    ) -> Option<usize> {
        let total = 20 + payload.len();
        let mut ip = [0u8; 20 + 1500];
        ip[0] = 0x45;
        put16(&mut ip[2..4], total as u16);
        ip[8] = 64;
        ip[9] = proto;
        ip[12..16].copy_from_slice(&self.config.ip.0);
        ip[16..20].copy_from_slice(&dst_ip.0);
        let cs = checksum::checksum(&ip[..20]);
        ip[10] = (cs >> 8) as u8;
        ip[11] = (cs & 0xff) as u8;
        ip[20..20 + payload.len()].copy_from_slice(payload);
        self.wrap_ether(dst_mac, ETH_IPV4, &ip[..total], out)
    }

    fn wrap_ether(&mut self, dst: Mac, ethertype: u16, payload: &[u8], out: &mut [u8]) -> Option<usize> {
        let len = HDR + payload.len();
        if out.len() < len {
            return None;
        }
        out[0..6].copy_from_slice(&dst.0);
        out[6..12].copy_from_slice(&self.config.mac.0);
        put16(&mut out[12..14], ethertype);
        out[HDR..len].copy_from_slice(payload);
        self.counters.tx_frames += 1;
        Some(len)
    }
}

fn icmp_checksum_ok(payload: &[u8]) -> bool {
    let csum = be16(&payload[2..4]);
    let mut tmp = [0u8; 576];
    let len = payload.len().min(tmp.len());
    tmp[..len].copy_from_slice(&payload[..len]);
    tmp[2] = 0;
    tmp[3] = 0;
    checksum::checksum(&tmp[..len]) == csum
}

fn set_icmp_checksum(buf: &mut [u8]) {
    buf[2] = 0;
    buf[3] = 0;
    let cs = checksum::checksum(buf);
    buf[2] = (cs >> 8) as u8;
    buf[3] = (cs & 0xff) as u8;
}

fn be16(b: &[u8]) -> u16 {
    ((b[0] as u16) << 8) | b[1] as u16
}

fn be32(b: &[u8]) -> u32 {
    ((b[0] as u32) << 24)
        | ((b[1] as u32) << 16)
        | ((b[2] as u32) << 8)
        | b[3] as u32
}

fn put16(b: &mut [u8], v: u16) {
    b[0] = (v >> 8) as u8;
    b[1] = (v & 0xff) as u8;
}

fn put32(b: &mut [u8], v: u32) {
    b[0] = (v >> 24) as u8;
    b[1] = (v >> 16) as u8;
    b[2] = (v >> 8) as u8;
    b[3] = (v & 0xff) as u8;
}

#[cfg(test)]
mod tests;
