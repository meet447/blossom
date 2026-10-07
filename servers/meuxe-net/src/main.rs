//! Virtio-net driver with `meuxe-netstack`. Serves terminal RPC on `USER_NET`.

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::mem::MaybeUninit;
use core::ptr::addr_of_mut;
use core::sync::atomic::{fence, Ordering};
use meuxe_abi::{
    NetBoot, SubmissionEntry, SYS_REPORT, SYS_RING_PROCESS, SYS_WAIT_IRQ,
    NET_OFF_IP, NET_OFF_OP, NET_OFF_PATH, NET_OFF_PATH_LEN, NET_OFF_REPLY, NET_OFF_REPLY_LEN,
    NET_OFF_STATUS, NET_OP_GET, NET_OP_INFO, NET_OP_PING, USER_INFO, SQ_OPCODE_RECV,
};
use meuxe_netstack::{Config, HttpStatus, Ipv4, Mac, PingStatus, Stack, TcpState, NET_HDR};
use meuxe_rt::{ring, syscall, yield_once, RING};

global_asm!(
    ".section .text.boot, \"ax\"",
    ".global _start",
    "_start:",
    "call main",
    "1:",
    "jmp 1b"
);

const ENDPOINT: u32 = 1;
const NET_VECTOR: u64 = 36;
const TIMER_VECTOR: u64 = 32;
const RPC_BOX: u64 = RING + 0x808;
const RPC_TAG: u64 = 8;
const MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
static mut RPC_PHASE: u8 = 0;
static mut LOOP_TICKS: u64 = 0;
static mut OUT: [u8; 1600] = [0; 1600];
static mut STACK_STORE: MaybeUninit<Stack> = MaybeUninit::uninit();
static mut REPORT_MSG: [u8; 64] = [0; 64];
const ACKNOWLEDGE: u8 = 1;
const DRIVER: u8 = 2;
const DRIVER_OK: u8 = 4;
const FEATURES_OK: u8 = 8;
const VERSION_1: u32 = 1;
const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;
const AVAIL: u64 = 0x100;
const USED: u64 = 0x200;

struct Dev {
    common: u64,
    notify: u64,
    notify_mul: u32,
    queue_size: u16,
    dma_virt: u64,
    dma_phys: [u64; 16],
    rx_queue: u64,
    tx_queue: u64,
    rx_last: u16,
    tx_last: u16,
}

#[no_mangle]
extern "C" fn main() -> ! {
    syscall(SYS_RING_PROCESS, 0, 0);
    let boot = unsafe { &*(USER_INFO as *const NetBoot) };
    let mut dev = Dev {
        common: boot.common,
        notify: boot.notify,
        notify_mul: boot.notify_mul,
        queue_size: boot.queue_size as u16,
        dma_virt: boot.dma_virt,
        dma_phys: boot.dma_phys,
        rx_queue: boot.dma_virt,
        tx_queue: boot.dma_virt + 4096,
        rx_last: 0,
        tx_last: 0,
    };
    if prepare(boot, &mut dev).is_none() {
        loop {
            yield_once();
        }
    }
    report_mac(&MAC);
    let config = Config {
        mac: Mac(MAC),
        ip: Ipv4([10, 0, 2, 15]),
        prefix: 24,
        gateway: Ipv4([10, 0, 2, 2]),
    };
    let stack = unsafe {
        let slot = &mut *addr_of_mut!(STACK_STORE);
        Stack::init_at(slot.as_mut_ptr(), config);
        slot.assume_init_mut()
    };
    serve(boot, &mut dev, stack);
}

fn serve(boot: &NetBoot, dev: &mut Dev, stack: &mut Stack) -> ! {
    let mut posted = false;
    let mut pending_rpc = false;
    let mut rpc_op = 0u32;
    let mut tcp_logged = false;
    loop {
        let out = unsafe { &mut *core::ptr::addr_of_mut!(OUT) };
        let mut rpc_done = false;
        while let Some(entry) = ring().cq.pop() {
            if entry.user_data == RPC_TAG && entry.result == 0 {
                rpc_done = true;
            }
        }
        if rpc_done {
            posted = false;
            if unsafe { (RPC_BOX as *const u64).read_volatile() } != 0 {
                unsafe {
                    (RPC_BOX as *mut u64).write_volatile(0);
                }
                pending_rpc = true;
                rpc_op = read32(boot.share_virt, NET_OFF_OP);
                tcp_logged = false;
                write32(boot.share_virt, NET_OFF_STATUS, 0);
                unsafe {
                    RPC_PHASE = 0;
                }
            }
        }
        if !posted {
            posted = post_rpc_recv();
        }
        syscall(SYS_RING_PROCESS, 0, 0);

        drain_rx(dev, stack, out, boot);
        drain_tx(dev);

        if pending_rpc {
            if drive_rpc(boot, dev, stack, out, rpc_op, &mut tcp_logged) {
                pending_rpc = false;
            }
        }

        if let Some(n) = stack_tick(boot, stack, out) {
            transmit(dev, &out[..n]);
        }

        if pending_rpc || work_pending(&*dev, stack) {
            yield_once();
        } else if posted {
            syscall(SYS_WAIT_IRQ, NET_VECTOR, TIMER_VECTOR);
        } else {
            yield_once();
        }
    }
}

fn post_rpc_recv() -> bool {
    let recv = SubmissionEntry {
        opcode: SQ_OPCODE_RECV,
        flags: 0,
        cap: ENDPOINT,
        a: RPC_BOX,
        b: 0,
        user_data: RPC_TAG,
    };
    ring().sq.push(recv).is_ok()
}

fn work_pending(dev: &Dev, stack: &Stack) -> bool {
    !matches!(stack.ping_status(), PingStatus::Idle)
        || !matches!(stack.http_status(), HttpStatus::Idle)
        || !caught_up_rx(dev)
        || !caught_up_tx(dev)
}

fn drive_rpc(
    boot: &NetBoot,
    dev: &mut Dev,
    stack: &mut Stack,
    out: &mut [u8],
    op: u32,
    tcp_logged: &mut bool,
) -> bool {
    let phase = unsafe { RPC_PHASE };
    match op {
        NET_OP_INFO => {
            unsafe {
                RPC_PHASE = 0;
            }
            finish_info(boot)
        }
        NET_OP_PING => match phase {
            0 => {
                let ip = read_ip(boot.share_virt);
                if !matches!(stack.ping_status(), PingStatus::Idle) {
                    return false;
                }
                if let Ok(n) = stack.ping(Ipv4(ip), out) {
                    transmit(dev, &out[..n]);
                }
                unsafe {
                    RPC_PHASE = 1;
                }
                false
            }
            _ => {
                poll_stack(boot, dev, stack, out);
                if let PingStatus::Active { received, .. } = stack.ping_status() {
                    if received >= 4 {
                        unsafe {
                            RPC_PHASE = 0;
                        }
                        finish_ping(boot, stack);
                        return true;
                    }
                    return false;
                }
                unsafe {
                    RPC_PHASE = 0;
                }
                write32(boot.share_virt, NET_OFF_STATUS, 2);
                true
            }
        },
        NET_OP_GET => match phase {
            0 => {
                if !matches!(stack.ping_status(), PingStatus::Idle)
                    || !matches!(stack.http_status(), HttpStatus::Idle)
                {
                    return false;
                }
                let ip = read_ip(boot.share_virt);
                let path_len = read32(boot.share_virt, NET_OFF_PATH_LEN) as usize;
                let path_len = path_len.min(256);
                let mut path = [0u8; 256];
                for index in 0..path_len {
                    path[index] = read8(boot.share_virt, NET_OFF_PATH + index as u64);
                }
                match stack.http_get(Ipv4(ip), 80, &path[..path_len], out) {
                    Ok(n) => {
                        if n > 0 {
                            transmit(dev, &out[..n]);
                        }
                    }
                    Err(_) => {
                        write32(boot.share_virt, NET_OFF_STATUS, 2);
                        return true;
                    }
                }
                unsafe {
                    RPC_PHASE = 1;
                }
                false
            }
            _ => {
                poll_stack(boot, dev, stack, out);
                if let HttpStatus::Active { state, .. } = stack.http_status() {
                    if matches!(state, TcpState::TimeWait | TcpState::Closed) {
                        let ip = read_ip(boot.share_virt);
                        if !*tcp_logged {
                            report_tcp(&ip);
                            *tcp_logged = true;
                        }
                        unsafe {
                            RPC_PHASE = 0;
                        }
                        finish_get_done(boot, stack, &ip);
                        return true;
                    }
                    if state == TcpState::Established && !*tcp_logged {
                        report_tcp(&read_ip(boot.share_virt));
                        *tcp_logged = true;
                    }
                    return false;
                }
                if matches!(stack.http_status(), HttpStatus::Idle) {
                    let ip = read_ip(boot.share_virt);
                    unsafe {
                        RPC_PHASE = 0;
                    }
                    finish_get_done(boot, stack, &ip);
                    return true;
                }
                false
            }
        },
        _ => {
            unsafe {
                RPC_PHASE = 0;
            }
            write32(boot.share_virt, NET_OFF_STATUS, 2);
            true
        }
    }
}

fn stack_tick(boot: &NetBoot, stack: &mut Stack, out: &mut [u8]) -> Option<usize> {
    let kernel = unsafe { ((boot.tick_virt) as *const u64).read_volatile() };
    let local = unsafe {
        LOOP_TICKS = LOOP_TICKS.wrapping_add(1);
        LOOP_TICKS
    };
    let ticks = kernel.max(local);
    stack.tick(ticks, out)
}

fn report_mac(_mac: &[u8; 6]) {
    static LINE: &[u8] = b"mac=52:54:00:12:34:56 ip=10.0.2.15 gw=10.0.2.2";
    syscall(SYS_REPORT, LINE.as_ptr() as u64, LINE.len() as u64);
}

fn write_mac(dst: &mut [u8], mac: &[u8; 6]) -> usize {
    let mut pos = 0usize;
    for (index, byte) in mac.iter().enumerate() {
        if index > 0 {
            dst[pos] = b':';
            pos += 1;
        }
        pos += write_hex_byte(&mut dst[pos..], *byte);
    }
    pos
}

fn write_hex_byte(dst: &mut [u8], byte: u8) -> usize {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    dst[0] = HEX[(byte >> 4) as usize];
    dst[1] = HEX[(byte & 0x0f) as usize];
    2
}

fn prepare(boot: &NetBoot, dev: &mut Dev) -> Option<[u8; 6]> {
    let common = dev.common;
    write8(common, 0x14, 0);
    let mut reset = false;
    for _ in 0..100_000 {
        if read8(common, 0x14) == 0 {
            reset = true;
            break;
        }
    }
    if !reset {
        fail(b"prep=rst");
        return None;
    }
    write8(common, 0x14, ACKNOWLEDGE);
    write8(common, 0x14, ACKNOWLEDGE | DRIVER);
    write32(common, 0x00, 1);
    if read32(common, 0x04) & VERSION_1 == 0 {
        fail(b"prep=v1");
        return None;
    }
    write32(common, 0x08, 1);
    write32(common, 0x0c, VERSION_1);
    write32(common, 0x08, 0);
    write32(common, 0x0c, 0);
    write8(common, 0x14, ACKNOWLEDGE | DRIVER | FEATURES_OK);
    for _ in 0..100_000 {
        if read8(common, 0x14) & FEATURES_OK != 0 {
            break;
        }
    }
    if read8(common, 0x14) & FEATURES_OK == 0 {
        fail(b"prep=feat");
        return None;
    }

    let mut mac = MAC;

    if !setup_queue(dev, 0, true) {
        fail(b"prep=rx");
        return None;
    }
    if !setup_queue(dev, 1, false) {
        fail(b"prep=tx");
        return None;
    }
    write8(common, 0x14, ACKNOWLEDGE | DRIVER | FEATURES_OK | DRIVER_OK);
    if boot.device != 0 {
        for index in 0..6 {
            mac[index] = read8(boot.device, index as u64);
        }
    }
    Some(mac)
}

fn fail(code: &[u8]) {
    syscall(SYS_REPORT, code.as_ptr() as u64, code.len() as u64);
}

fn setup_queue(dev: &Dev, index: u16, rx: bool) -> bool {
    let common = dev.common;
    let q = if rx { dev.rx_queue } else { dev.tx_queue };
    write16(common, 0x16, index);
    let max = read16(common, 0x18);
    if max < dev.queue_size {
        return false;
    }
    write16(common, 0x18, dev.queue_size);
    write64(common, 0x20, phys_queue(dev, rx));
    write64(common, 0x28, phys_queue(dev, rx) + AVAIL);
    write64(common, 0x30, phys_queue(dev, rx) + USED);
    write16(common, 0x1a, 0);
    if read16(common, 0x1a) == 0xFFFF {
        return false;
    }
    write16(common, 0x1c, 1);
    let base_page = if rx {
        2u64
    } else {
        2u64 + dev.queue_size as u64
    };
    for slot in 0..dev.queue_size as u64 {
        let page = base_page + slot;
        let buf_virt = dev.dma_virt + page * 4096;
        let buf_phys = dev.dma_phys[page as usize];
        let flags = if rx { DESC_WRITE } else { 0 };
        write_desc(q, slot, buf_phys, 4096, flags, 0);
        if rx {
            write16(q + AVAIL, 4 + slot * 2, slot as u16);
        }
    }
    if rx {
        fence(Ordering::SeqCst);
        write16(q + AVAIL, 2, dev.queue_size);
        fence(Ordering::SeqCst);
        notify(dev, 0);
    }
    true
}

fn phys_queue(dev: &Dev, rx: bool) -> u64 {
    let page = if rx { 0 } else { 1 };
    dev.dma_phys[page]
}

fn ack_isr(boot: &NetBoot) {
    if boot.isr != 0 {
        let _ = read8(boot.isr, 0);
    }
}

fn drain_rx(dev: &mut Dev, stack: &mut Stack, out: &mut [u8], boot: &NetBoot) {
    let idx = read16(dev.rx_queue + USED, 2);
    while dev.rx_last != idx {
        let slot = (dev.rx_last % dev.queue_size) as u64;
        let elem = dev.rx_queue + USED + 4 + slot * 8;
        let id = read32(elem, 0) as u16;
        let len = read32(elem, 4) as usize;
        if id < dev.queue_size {
            let buf = dev.dma_virt + (2 + id as u64) * 4096;
            if len > NET_HDR {
                let frame = unsafe {
                    core::slice::from_raw_parts((buf + NET_HDR as u64) as *const u8, len - NET_HDR)
                };
                if let Some(n) = stack.recv(frame, out) {
                    transmit(dev, &out[..n]);
                }
            }
            requeue_rx(dev, id);
        }
        dev.rx_last = dev.rx_last.wrapping_add(1);
    }
    ack_isr(boot);
}

fn requeue_rx(dev: &Dev, id: u16) {
    let avail = dev.rx_queue + AVAIL;
    let head = read16(avail, 2);
    let slot = (head % dev.queue_size) as u64;
    write16(avail, 4 + slot * 2, id);
    fence(Ordering::SeqCst);
    write16(avail, 2, head.wrapping_add(1));
    fence(Ordering::SeqCst);
    notify(dev, 0);
}

fn drain_tx(dev: &mut Dev) {
    let idx = read16(dev.tx_queue + USED, 2);
    dev.tx_last = idx;
}

fn caught_up_rx(dev: &Dev) -> bool {
    read16(dev.rx_queue + USED, 2) == dev.rx_last
}

fn caught_up_tx(dev: &Dev) -> bool {
    read16(dev.tx_queue + USED, 2) == dev.tx_last
}

fn transmit(dev: &Dev, frame: &[u8]) {
    if frame.is_empty() || frame.len() + NET_HDR > 4096 {
        return;
    }
    let avail = dev.tx_queue + AVAIL;
    let head = read16(avail, 2);
    let id = (head % dev.queue_size) as u64;
    let page = 2u64 + dev.queue_size as u64 + id;
    let buf = dev.dma_virt + page * 4096;
    unsafe {
        for index in 0..NET_HDR {
            ((buf + index as u64) as *mut u8).write_volatile(0);
        }
        for (index, byte) in frame.iter().enumerate() {
            ((buf + NET_HDR as u64 + index as u64) as *mut u8).write_volatile(*byte);
        }
    }
    write_desc(
        dev.tx_queue,
        id,
        dev.dma_phys[page as usize],
        (NET_HDR + frame.len()) as u32,
        0,
        0,
    );
    let slot = (head % dev.queue_size) as u64;
    write16(avail, 4 + slot * 2, id as u16);
    fence(Ordering::SeqCst);
    write16(avail, 2, head.wrapping_add(1));
    fence(Ordering::SeqCst);
    notify(dev, 1);
}

fn poll_stack(boot: &NetBoot, dev: &mut Dev, stack: &mut Stack, out: &mut [u8]) {
    drain_rx(dev, stack, out, boot);
    drain_tx(dev);
    if let Some(n) = stack_tick(boot, stack, out) {
        transmit(dev, &out[..n]);
    }
}

fn report_tcp(ip: &[u8; 4]) {
    let mut msg = [0u8; 48];
    const PREFIX: &[u8] = b"tcp ";
    const PORT: &[u8] = b":80 ";
    const STATE: &[u8] = b"state=established";
    msg[..PREFIX.len()].copy_from_slice(PREFIX);
    let mut pos = PREFIX.len();
    pos += write_ip(&mut msg[pos..], ip);
    msg[pos..pos + PORT.len()].copy_from_slice(PORT);
    pos += PORT.len();
    msg[pos..pos + STATE.len()].copy_from_slice(STATE);
    pos += STATE.len();
    syscall(SYS_REPORT, msg.as_ptr() as u64, pos as u64);
}

fn write_ip(dst: &mut [u8], ip: &[u8; 4]) -> usize {
    let mut pos = 0usize;
    for (index, octet) in ip.iter().enumerate() {
        if index > 0 {
            dst[pos] = b'.';
            pos += 1;
        }
        pos += write_dec(&mut dst[pos..], *octet as u32);
    }
    pos
}

fn write_dec(dst: &mut [u8], mut value: u32) -> usize {
    if value == 0 {
        dst[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 10];
    let mut used = 0usize;
    while value > 0 {
        tmp[used] = b'0' + (value % 10) as u8;
        used += 1;
        value /= 10;
    }
    for index in 0..used {
        dst[index] = tmp[used - 1 - index];
    }
    used
}

fn finish_info(boot: &NetBoot) -> bool {
    write_reply(boot, b"10.0.2.15 gw 10.0.2.2");
    true
}

fn finish_ping(boot: &NetBoot, stack: &mut Stack) {
    write_reply(boot, b"rx=4/4");
    stack.clear_ping();
}

fn finish_get(boot: &NetBoot, stack: &Stack, ip: &[u8; 4]) {
    let status = match stack.http_status() {
        HttpStatus::Active { status_code, .. } => status_code,
        _ => 0,
    };
    let body = stack.body();
    let mut reply = [0u8; 128];
    let mut pos = 0usize;
    pos += write_dec_field(&mut reply[pos..], b"status=", status as u32);
    reply[pos..pos + 7].copy_from_slice(b" bytes=");
    pos += 7;
    pos += write_dec(&mut reply[pos..], body.len() as u32);
    reply[pos..pos + 6].copy_from_slice(b" body=");
    pos += 6;
    let take = body.len().min(reply.len() - pos);
    reply[pos..pos + take].copy_from_slice(&body[..take]);
    pos += take;
    write_reply(boot, &reply[..pos]);
    let _ = ip;
}

fn finish_get_done(boot: &NetBoot, stack: &mut Stack, ip: &[u8; 4]) {
    finish_get(boot, stack, ip);
    stack.clear_http();
}

fn write_dec_field(dst: &mut [u8], label: &[u8], value: u32) -> usize {
    let mut pos = 0usize;
    dst[..label.len()].copy_from_slice(label);
    pos += label.len();
    pos += write_dec(&mut dst[pos..], value);
    pos
}

fn write_reply(boot: &NetBoot, text: &[u8]) {
    let len = text.len().min(128);
    for index in 0..len {
        write8(boot.share_virt, NET_OFF_REPLY + index as u64, text[index]);
    }
    write32(boot.share_virt, NET_OFF_REPLY_LEN, len as u32);
    write32(boot.share_virt, NET_OFF_STATUS, 1);
}

fn read_ip(base: u64) -> [u8; 4] {
    [
        read8(base, NET_OFF_IP),
        read8(base, NET_OFF_IP + 1),
        read8(base, NET_OFF_IP + 2),
        read8(base, NET_OFF_IP + 3),
    ]
}

fn notify(dev: &Dev, queue: u16) {
    write16(dev.common, 0x16, queue);
    let notify_off = read16(dev.common, 0x1e) as u64;
    let doorbell = dev.notify + notify_off * dev.notify_mul as u64;
    unsafe {
        (doorbell as *mut u16).write_volatile(queue);
    }
}

fn write_desc(base: u64, index: u64, addr: u64, len: u32, flags: u16, next: u16) {
    let slot = base + index * 16;
    unsafe {
        (slot as *mut u64).write_volatile(addr);
        ((slot + 8) as *mut u32).write_volatile(len);
        ((slot + 12) as *mut u16).write_volatile(flags);
        ((slot + 14) as *mut u16).write_volatile(next);
    }
}

fn read8(base: u64, off: u64) -> u8 {
    unsafe { ((base + off) as *const u8).read_volatile() }
}

fn read16(base: u64, off: u64) -> u16 {
    unsafe { ((base + off) as *const u16).read_volatile() }
}

fn read32(base: u64, off: u64) -> u32 {
    unsafe { ((base + off) as *const u32).read_volatile() }
}

fn write8(base: u64, off: u64, value: u8) {
    unsafe { ((base + off) as *mut u8).write_volatile(value) }
}

fn write16(base: u64, off: u64, value: u16) {
    unsafe { ((base + off) as *mut u16).write_volatile(value) }
}

fn write32(base: u64, off: u64, value: u32) {
    unsafe { ((base + off) as *mut u32).write_volatile(value) }
}

fn write64(base: u64, off: u64, value: u64) {
    unsafe { ((base + off) as *mut u64).write_volatile(value) }
}
