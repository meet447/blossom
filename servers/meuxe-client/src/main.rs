//! Terminal on the shared compositor surface.
//!
//! Handle 1 sends dirty rectangles. Handle 2 receives virtio key-down
//! events. The desktop window is painted first so the tablet hit still lands.

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{fence, Ordering};
use meuxe_abi::{
    pack_rect, ClientBoot, SubmissionEntry, ERR_AGAIN, RESULT_OK, SYS_REPORT, SYS_RING_PROCESS, TERM_ACCENT,
    TERM_BG, TERM_COLS, TERM_FG, TERM_ROWS, USER_FS, USER_INFO, WINDOW_COLOR, SQ_OPCODE_RECV,
    SQ_OPCODE_SEND,
};
use meuxe_rt::{ring, syscall, yield_once, RING};
use meuxe_ui::{key_of, Canvas, Key as UiKey};

global_asm!(
    ".section .text.boot, \"ax\"",
    ".global _start",
    "_start:",
    "call main",
    "1:",
    "jmp 1b"
);

const RECT_CAP: u32 = 1;
const KEY_CAP: u32 = 2;
const FS_CAP: u32 = 3;
const FS_TAG: u64 = 7;
const KEY_BOX: u64 = RING + 0x808;
const PROMPT_LEN: u32 = 7;
const PROMPT: &[u8] = b"meuxe> ";
const CELLS: usize = (TERM_COLS as usize) * (TERM_ROWS as usize);

struct Buf {
    bytes: UnsafeCell<[u8; CELLS]>,
}

unsafe impl Sync for Buf {}

static SCREEN: Buf = Buf {
    bytes: UnsafeCell::new([b' '; CELLS]),
};
static INK: Buf = Buf {
    bytes: UnsafeCell::new([0; CELLS]),
};

#[no_mangle]
extern "C" fn main() -> ! {
    let boot = unsafe { &*(USER_INFO as *const ClientBoot) };
    post_recv();
    fill_window(boot);
    send_rect(pack_rect(
        boot.origin_x,
        boot.origin_y,
        boot.width,
        boot.height,
    ));
    reset_screen();
    write_header();
    write_prompt(0, 1);
    let mut row = 1u32;
    let mut col = PROMPT_LEN;
    paint_all(boot, col, row);
    fence(Ordering::SeqCst);
    send_rect(full_rect(boot));
    loop {
        let packed = unsafe { (KEY_BOX as *const u64).read_volatile() };
        if packed != 0 {
            unsafe {
                (KEY_BOX as *mut u64).write_volatile(0);
            }
            let code = packed as u16;
            post_recv();
            on_key(boot, code, &mut row, &mut col);
        }
        yield_once();
    }
}

fn on_key(boot: &ClientBoot, code: u16, row: &mut u32, col: &mut u32) {
    match map_key(code) {
        Key::Ignore => {}
        Key::Enter => enter(boot, row, col),
        Key::Backspace => backspace(boot, *row, col),
        Key::Char(ch) => insert(boot, *row, col, ch),
    }
}

enum Key {
    Ignore,
    Enter,
    Backspace,
    Char(u8),
}

fn map_key(code: u16) -> Key {
    match key_of(code) {
        Some(UiKey::Backspace) => Key::Backspace,
        Some(UiKey::Enter) => Key::Enter,
        Some(UiKey::Char(ch)) => Key::Char(ch),
        None => Key::Ignore,
    }
}

fn insert(boot: &ClientBoot, row: u32, col: &mut u32, ch: u8) {
    let limit = columns(boot);
    if *col + 1 >= limit {
        return;
    }
    let previous = *col;
    set_cell(previous, row, ch, 0);
    *col += 1;
    paint_cell(boot, previous, row, false);
    paint_cell(boot, *col, row, true);
    fence(Ordering::SeqCst);
    send_span(boot, previous, row, *col + 1, row + 1);
}

fn backspace(boot: &ClientBoot, row: u32, col: &mut u32) {
    if *col <= PROMPT_LEN {
        return;
    }
    let previous = *col;
    *col -= 1;
    set_cell(*col, row, b' ', 0);
    paint_cell(boot, *col, row, true);
    paint_cell(boot, previous, row, false);
    fence(Ordering::SeqCst);
    send_span(boot, *col, row, previous + 1, row + 1);
}

fn enter(boot: &ClientBoot, row: &mut u32, col: &mut u32) {
    let mut buf = [0u8; TERM_COLS as usize];
    let start = PROMPT_LEN;
    let end = (*col).min(columns(boot));
    let mut len = 0usize;
    let mut index = start;
    while index < end {
        buf[len] = read_cell(index, *row);
        len += 1;
        index += 1;
    }
    let line = &buf[..len];
    if line == b"clear" {
        reset_screen();
        write_header();
        write_prompt(0, 1);
        *row = 1;
        *col = PROMPT_LEN;
        paint_all(boot, *col, *row);
        fence(Ordering::SeqCst);
        send_rect(full_rect(boot));
        return;
    }
    if line == b"fetch" || line == b"neofetch" {
        paint_cell(boot, *col, *row, false);
        let dirty_from = *row;
        let scrolled = draw_fetch(boot, row);
        *col = PROMPT_LEN;
        if scrolled {
            paint_all(boot, *col, *row);
            fence(Ordering::SeqCst);
            send_rect(full_rect(boot));
            return;
        }
        paint_rows(boot, dirty_from, *row);
        paint_cell(boot, *col, *row, true);
        fence(Ordering::SeqCst);
        send_span(boot, 0, dirty_from, columns(boot), *row + 1);
        return;
    }
    paint_cell(boot, *col, *row, false);
    let dirty_from = *row;
    let mut scrolled = false;
    let mut owned = [0u8; 48];
    let text = if let Some(text) = directory_output(line, &mut owned) {
        Some(text)
    } else {
        command_output(line)
    };
    if let Some(text) = text {
        let (next, moved) = advance(boot, *row);
        *row = next;
        scrolled |= moved;
        let mut cursor = 0u32;
        for byte in text {
            if cursor >= columns(boot) {
                break;
            }
            set_cell(cursor, *row, *byte, 0);
            cursor += 1;
        }
    }
    let (next, moved) = advance(boot, *row);
    *row = next;
    scrolled |= moved;
    write_prompt(0, *row);
    *col = PROMPT_LEN;
    if scrolled {
        paint_all(boot, *col, *row);
        fence(Ordering::SeqCst);
        send_rect(full_rect(boot));
        return;
    }
    paint_rows(boot, dirty_from, *row);
    paint_cell(boot, *col, *row, true);
    fence(Ordering::SeqCst);
    send_span(boot, 0, dirty_from, columns(boot), *row + 1);
}

static mut CWD: [u8; 64] = [b'/'; 64];
static mut CWD_LEN: usize = 1;
static mut REPORT: [u8; 32] = [0; 32];

fn cwd() -> &'static [u8] {
    unsafe { &CWD[..CWD_LEN] }
}

fn set_cwd(path: &[u8]) {
    let n = path.len().min(64);
    unsafe {
        CWD[..n].copy_from_slice(&path[..n]);
        CWD_LEN = n.max(1);
        if CWD_LEN == 0 {
            CWD[0] = b'/';
            CWD_LEN = 1;
        }
    }
}

fn resolve(name: &[u8], dst: &mut [u8; 64]) -> usize {
    if name.first() == Some(&b'/') {
        let n = name.len().min(dst.len());
        dst[..n].copy_from_slice(&name[..n]);
        return n;
    }
    let base = cwd();
    let mut len = 0usize;
    if base == b"/" {
        if len < dst.len() {
            dst[0] = b'/';
            len = 1;
        }
    } else {
        let n = base.len().min(dst.len());
        dst[..n].copy_from_slice(&base[..n]);
        len = n;
        if len < dst.len() {
            dst[len] = b'/';
            len += 1;
        }
    }
    let room = dst.len() - len;
    let n = name.len().min(room);
    dst[len..len + n].copy_from_slice(&name[..n]);
    len + n
}

fn report_ok(prefix: &[u8], path: &[u8]) {
    let mut msg = [0u8; 32];
    let mut len = 0usize;
    for &byte in prefix.iter().chain(path.iter()) {
        if len + 3 >= msg.len() {
            break;
        }
        msg[len] = byte;
        len += 1;
    }
    msg[len..len + 3].copy_from_slice(b" ok");
    report_line(&msg[..len + 3]);
}

fn report_cat(path: &[u8], body: &[u8]) {
    let mut msg = [0u8; 32];
    let mut len = 0usize;
    for &byte in b"cat=".iter().chain(path.iter()) {
        if len + 1 >= msg.len() {
            break;
        }
        msg[len] = byte;
        len += 1;
    }
    if len < msg.len() {
        msg[len] = b' ';
        len += 1;
    }
    for &byte in body {
        if len >= msg.len() {
            break;
        }
        msg[len] = byte;
        len += 1;
    }
    report_line(&msg[..len]);
}

fn report_line(text: &[u8]) {
    let n = text.len().min(32);
    unsafe {
        REPORT[..n].copy_from_slice(&text[..n]);
        syscall(SYS_REPORT, REPORT.as_ptr() as u64, n as u64);
    }
}

fn directory_output<'a>(line: &[u8], owned: &'a mut [u8; 48]) -> Option<&'a [u8]> {
    let mut path = [0u8; 64];
    let mut aux = [0u8; 64];
    if line == b"ls" {
        let n = resolve(b"", &mut path);
        let count = fs_query(1, &path[..n.max(1)], b"", 0, b"", owned);
        return if count == 0 { Some(b"?") } else { Some(&owned[..count]) };
    }
    if line == b"pwd" {
        let base = cwd();
        let n = base.len().min(owned.len());
        owned[..n].copy_from_slice(&base[..n]);
        return Some(&owned[..n]);
    }
    if line == b"df" {
        let count = fs_query(8, b"/", b"", 0, b"", owned);
        if count == 0 {
            return Some(b"?");
        }
        let mut msg = [0u8; 32];
        msg[..3].copy_from_slice(b"df ");
        let n = count.min(29);
        msg[3..3 + n].copy_from_slice(&owned[..n]);
        report_line(&msg[..3 + n]);
        return Some(&owned[..count]);
    }
    if let Some(rest) = strip(line, b"cd ") {
        let n = resolve(rest, &mut path);
        let count = fs_query(4, &path[..n], b"", 0, b"", owned);
        if owned[..count] == *b"dir" {
            set_cwd(&path[..n]);
            let base = cwd();
            let k = base.len().min(owned.len());
            owned[..k].copy_from_slice(&base[..k]);
            return Some(&owned[..k]);
        }
        return Some(b"?");
    }
    if let Some(rest) = strip(line, b"cat ") {
        let n = resolve(rest, &mut path);
        let count = fs_query(2, &path[..n], b"", 0, b"", owned);
        if count == 0 {
            return Some(b"?");
        }
        report_cat(&path[..n], &owned[..count]);
        return Some(&owned[..count]);
    }
    if line == b"cat" {
        return Some(b"?");
    }
    if let Some(rest) = strip(line, b"mkdir ") {
        let n = resolve(rest, &mut path);
        let count = fs_query(5, &path[..n], b"", 0, b"", owned);
        if count == 0 {
            return Some(b"?");
        }
        let mut msg = [0u8; 32];
        let prefix = b"mkdir=";
        let mut len = prefix.len();
        msg[..len].copy_from_slice(prefix);
        let room = 32 - len - 3;
        let copy = n.min(room);
        msg[len..len + copy].copy_from_slice(&path[..copy]);
        len += copy;
        msg[len..len + 3].copy_from_slice(b" ok");
        report_line(&msg[..len + 3]);
        return Some(&owned[..count]);
    }
    if let Some(rest) = strip(line, b"rm ") {
        let n = resolve(rest, &mut path);
        let count = fs_query(6, &path[..n], b"", 0, b"", owned);
        if count == 0 {
            return Some(b"?");
        }
        let mut msg = [0u8; 32];
        let prefix = b"rm=";
        msg[..3].copy_from_slice(prefix);
        let copy = n.min(32 - 6);
        msg[3..3 + copy].copy_from_slice(&path[..copy]);
        let len = 3 + copy;
        msg[len..len + 3].copy_from_slice(b" ok");
        report_line(&msg[..len + 3]);
        return Some(&owned[..count]);
    }
    if let Some(rest) = strip(line, b"touch ") {
        let n = resolve(rest, &mut path);
        let count = fs_query(3, &path[..n], b"", 1, b"", owned);
        return if count == 0 && owned[0] != b'o' {
            let again = fs_query(3, &path[..n], b"", 1, b"", owned);
            if again == 0 { Some(b"ok") } else { Some(&owned[..again]) }
        } else if count == 0 {
            Some(b"ok")
        } else {
            Some(&owned[..count])
        };
    }
    if let Some(rest) = strip(line, b"stat ") {
        let n = resolve(rest, &mut path);
        let count = fs_query(4, &path[..n], b"", 0, b"", owned);
        return if count == 0 { Some(b"?") } else { Some(&owned[..count]) };
    }
    if let Some(rest) = strip(line, b"append ") {
        let Some((name, data)) = split_two(rest) else {
            return Some(b"?");
        };
        let n = resolve(name, &mut path);
        let count = fs_query(3, &path[..n], data, 4, b"", owned);
        if count == 0 {
            return Some(b"?");
        }
        report_ok(b"append=", &path[..n]);
        return Some(&owned[..count]);
    }
    if let Some(rest) = strip(line, b"cp ") {
        let Some((src, dst)) = split_two(rest) else {
            return Some(b"?");
        };
        let n = resolve(src, &mut path);
        let m = resolve(dst, &mut aux);
        let count = fs_query(2, &path[..n], b"", 0, b"", owned);
        if count == 0 {
            return Some(b"?");
        }
        let mut body = [0u8; 48];
        body[..count].copy_from_slice(&owned[..count]);
        let wrote = fs_query(3, &aux[..m], &body[..count], 1 | 2, b"", owned);
        if wrote == 0 {
            return Some(b"?");
        }
        report_ok(b"cp=", &aux[..m]);
        return Some(b"ok");
    }
    if let Some(rest) = strip(line, b"mv ") {
        let Some((src, dst)) = split_two(rest) else {
            return Some(b"?");
        };
        let n = resolve(src, &mut path);
        let m = resolve(dst, &mut aux);
        let count = fs_query(7, &path[..n], b"", 0, &aux[..m], owned);
        if count == 0 {
            return Some(b"?");
        }
        report_ok(b"mv=", &aux[..m]);
        return Some(&owned[..count]);
    }
    if let Some(rest) = strip(line, b"write ") {
        let Some((name, data)) = split_two(rest) else {
            return Some(b"?");
        };
        let n = resolve(name, &mut path);
        let count = fs_query(3, &path[..n], data, 1 | 2, b"", owned);
        if count == 0 {
            return Some(b"?");
        }
        report_ok(b"write=", &path[..n]);
        return Some(&owned[..count]);
    }
    None
}

fn strip<'a>(line: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    if line.len() >= prefix.len() && &line[..prefix.len()] == prefix {
        Some(&line[prefix.len()..])
    } else {
        None
    }
}

fn split_two(rest: &[u8]) -> Option<(&[u8], &[u8])> {
    let split = rest.iter().position(|byte| *byte == b' ')?;
    let name = &rest[..split];
    let data = &rest[split + 1..];
    if name.is_empty() || data.is_empty() {
        None
    } else {
        Some((name, data))
    }
}

fn fs_query(op: u32, path: &[u8], data: &[u8], flags: u32, aux: &[u8], out: &mut [u8; 48]) -> usize {
    let path_len = path.len().min(64);
    let data_len = data.len().min(32);
    let aux_len = aux.len().min(64);
    unsafe {
        (USER_FS as *mut u32).write_volatile(op);
        ((USER_FS + 4) as *mut u32).write_volatile(path_len as u32);
        ((USER_FS + 116) as *mut u32).write_volatile(flags);
        ((USER_FS + 192) as *mut u32).write_volatile(aux_len as u32);
        let slot = (USER_FS + 128) as *mut u8;
        for index in 0..64 {
            let byte = if index < path_len { path[index] } else { 0 };
            slot.add(index).write_volatile(byte);
        }
        let extra = (USER_FS + 196) as *mut u8;
        for index in 0..64 {
            let byte = if index < aux_len { aux[index] } else { 0 };
            extra.add(index).write_volatile(byte);
        }
        ((USER_FS + 80) as *mut u32).write_volatile(data_len as u32);
        let body = (USER_FS + 84) as *mut u8;
        for index in 0..32 {
            let byte = if index < data_len { data[index] } else { 0 };
            body.add(index).write_volatile(byte);
        }
        ((USER_FS + 24) as *mut u32).write_volatile(0);
        ((USER_FS + 28) as *mut u32).write_volatile(0);
    }
    fence(Ordering::SeqCst);
    send_fs();
    for _ in 0..200_000 {
        let status = unsafe { ((USER_FS + 24) as *const u32).read_volatile() };
        if status != 0 {
            let len = unsafe { ((USER_FS + 28) as *const u32).read_volatile() } as usize;
            let len = len.min(out.len());
            let src = (USER_FS + 32) as *const u8;
            for index in 0..len {
                out[index] = unsafe { src.add(index).read_volatile() };
            }
            return len;
        }
        yield_once();
    }
    out[0] = b'?';
    1
}

fn send_fs() {
    loop {
        let send = SubmissionEntry {
            opcode: SQ_OPCODE_SEND,
            flags: 0,
            cap: FS_CAP,
            a: FS_TAG,
            b: 0,
            user_data: FS_TAG,
        };
        if ring().sq.push(send).is_err() {
            yield_once();
            continue;
        }
        syscall(SYS_RING_PROCESS, 0, 0);
        loop {
            match completion(FS_TAG) {
                Some(RESULT_OK) => return,
                Some(ERR_AGAIN) => break,
                Some(_) => break,
                None => yield_once(),
            }
        }
    }
}

const MARK: [&[u8]; 8] = [
    b"      #      ",
    b"   #  #  #   ",
    b"  #   #   #  ",
    b" #    @    # ",
    b"  #   #   #  ",
    b"   #     #   ",
    b"     # #     ",
    b"      |      ",
];
const INFO_COL: u32 = 15;
const EYE: u32 = 0x00F2_C94A;
const PETAL: u32 = 0x00F2_7AA0;
const THROAT: u32 = 0x00C2_1858;

fn draw_fetch(boot: &ClientBoot, row: &mut u32) -> bool {
    let mut scrolled = false;
    let mut res = [0u8; 24];
    let res_len = write_pair(&mut res, boot.screen_w, boot.screen_h);
    let mut term = [0u8; 24];
    let term_len = write_pair(&mut term, columns(boot), rows(boot));
    let fields: [(&[u8], &[u8]); 5] = [
        (b"os", b"meuxe"),
        (b"arch", b"x86_64"),
        (b"res", &res[..res_len]),
        (b"term", &term[..term_len]),
        (b"shell", b"meuxe"),
    ];
    for line in 0..MARK.len() {
        let (next, moved) = advance(boot, *row);
        *row = next;
        scrolled |= moved;
        blank_row(*row);
        for (col, byte) in MARK[line].iter().enumerate() {
            set_cell(col as u32, *row, *byte, mark_ink(*byte));
        }
        match line {
            0 => put_title(*row),
            1 => put(INFO_COL, *row, b"------------", 1),
            2..=6 => put_field(*row, fields[line - 2].0, fields[line - 2].1),
            _ => put_swatches(*row),
        }
    }
    let (next, moved) = advance(boot, *row);
    *row = next;
    scrolled |= moved;
    blank_row(*row);
    write_prompt(0, *row);
    scrolled
}

fn put_title(row: u32) {
    put(INFO_COL, row, b"meuxe", 1);
    put(INFO_COL + 5, row, b"@", 0);
    put(INFO_COL + 6, row, b"meuxe", 1);
}

fn put_field(row: u32, label: &[u8], value: &[u8]) {
    let mut padded = [b' '; 6];
    let copy = label.len().min(padded.len());
    padded[..copy].copy_from_slice(&label[..copy]);
    put(INFO_COL, row, &padded, 1);
    put(INFO_COL + 7, row, value, 0);
}

fn put_swatches(row: u32) {
    let inks = [3u8, 3, 2, 2, 1, 1, 4, 4];
    for (index, ink) in inks.iter().enumerate() {
        set_cell(INFO_COL + index as u32 * 2, row, b'#', *ink);
    }
}

fn put(col: u32, row: u32, text: &[u8], ink: u8) {
    for (index, byte) in text.iter().enumerate() {
        let x = col + index as u32;
        if x >= TERM_COLS {
            break;
        }
        set_cell(x, row, *byte, ink);
    }
}

fn blank_row(row: u32) {
    for col in 0..TERM_COLS {
        set_cell(col, row, b' ', 0);
    }
}

fn mark_ink(byte: u8) -> u8 {
    match byte {
        b'@' => 2,
        b'#' => 3,
        b'|' => 4,
        _ => 0,
    }
}

fn ink_color(ink: u8) -> u32 {
    match ink {
        1 => TERM_ACCENT,
        2 => EYE,
        3 => PETAL,
        4 => THROAT,
        _ => TERM_FG,
    }
}

fn write_pair(dst: &mut [u8], width: u32, height: u32) -> usize {
    if width == 0 || height == 0 {
        dst[0] = b'?';
        return 1;
    }
    let mut len = write_dec(dst, width);
    if len < dst.len() {
        dst[len] = b'x';
        len += 1;
    }
    len + write_dec(&mut dst[len..], height)
}

fn write_dec(dst: &mut [u8], value: u32) -> usize {
    let mut tmp = [0u8; 10];
    let mut rest = value;
    let mut used = 0usize;
    loop {
        tmp[used] = b'0' + (rest % 10) as u8;
        used += 1;
        rest /= 10;
        if rest == 0 || used == tmp.len() {
            break;
        }
    }
    let count = used.min(dst.len());
    for index in 0..count {
        dst[index] = tmp[used - 1 - index];
    }
    count
}

fn command_output(line: &[u8]) -> Option<&[u8]> {
    if line == b"help" {
        Some(b"echo clear ls cat write fetch help")
    } else if line.len() >= 5 && &line[..5] == b"echo " {
        Some(&line[5..])
    } else if line == b"echo" || line.is_empty() {
        None
    } else {
        Some(b"?")
    }
}

fn advance(boot: &ClientBoot, row: u32) -> (u32, bool) {
    if row + 1 >= rows(boot) {
        scroll();
        (rows(boot) - 1, true)
    } else {
        (row + 1, false)
    }
}

fn scroll() {
    let cols = TERM_COLS as usize;
    let rows = TERM_ROWS as usize;
    unsafe {
        let cells = cells();
        let ink = ink();
        for index in 0..(rows - 1) * cols {
            *cells.add(index) = *cells.add(index + cols);
            *ink.add(index) = *ink.add(index + cols);
        }
        for index in (rows - 1) * cols..rows * cols {
            *cells.add(index) = b' ';
            *ink.add(index) = 0;
        }
    }
}

fn reset_screen() {
    unsafe {
        let cells = cells();
        let ink = ink();
        for index in 0..CELLS {
            *cells.add(index) = b' ';
            *ink.add(index) = 0;
        }
    }
}

fn write_header() {
    for (index, byte) in b"meuxe".iter().enumerate() {
        set_cell(index as u32, 0, *byte, 1);
    }
}

fn write_prompt(col: u32, row: u32) {
    for (index, byte) in PROMPT.iter().enumerate() {
        set_cell(col + index as u32, row, *byte, 0);
    }
}

fn paint_all(boot: &ClientBoot, cursor_col: u32, cursor_row: u32) {
    clear_pixels(boot);
    paint_rows(boot, 0, rows(boot) - 1);
    paint_cell(boot, cursor_col, cursor_row, true);
}

fn paint_rows(boot: &ClientBoot, from: u32, to: u32) {
    let mut row = from;
    while row <= to {
        let mut col = 0;
        let width = columns(boot);
        while col < width {
            paint_cell(boot, col, row, false);
            col += 1;
        }
        row += 1;
    }
}

fn term_canvas(boot: &ClientBoot) -> Canvas {
    let scale = boot.scale.max(1);
    Canvas::new(
        boot.term as *mut u32,
        boot.term_stride,
        columns(boot) * 8 * scale,
        rows(boot) * 8 * scale,
    )
}

fn paint_cell(boot: &ClientBoot, col: u32, row: u32, cursor: bool) {
    let width = columns(boot);
    let height = rows(boot);
    if col >= width || row >= height {
        return;
    }
    let scale = boot.scale.max(1);
    let caret = if cursor { Some(TERM_ACCENT) } else { None };
    term_canvas(boot).glyph(
        col * 8 * scale,
        row * 8 * scale,
        read_cell(col, row),
        scale,
        ink_color(read_ink(col, row)),
        TERM_BG,
        caret,
    );
}

fn clear_pixels(boot: &ClientBoot) {
    term_canvas(boot).fill(TERM_BG);
}

fn fill_window(boot: &ClientBoot) {
    Canvas::new(boot.back as *mut u32, boot.width, boot.width, boot.height).fill(WINDOW_COLOR);
    fence(Ordering::SeqCst);
}

fn full_rect(boot: &ClientBoot) -> u64 {
    let scale = boot.scale.max(1);
    pack_rect(
        boot.term_x,
        boot.term_y,
        columns(boot) * 8 * scale,
        rows(boot) * 8 * scale,
    )
}

fn send_span(boot: &ClientBoot, col: u32, row: u32, col_end: u32, row_end: u32) {
    let scale = boot.scale.max(1);
    let x = boot.term_x + col * 8 * scale;
    let y = boot.term_y + row * 8 * scale;
    let w = col_end.saturating_sub(col) * 8 * scale;
    let h = row_end.saturating_sub(row) * 8 * scale;
    if w == 0 || h == 0 {
        return;
    }
    send_rect(pack_rect(x, y, w, h));
}

fn send_rect(packed: u64) {
    let x = (packed & 0xFFFF) as u32;
    let y = ((packed >> 16) & 0xFFFF) as u32;
    let w = ((packed >> 32) & 0xFFFF) as u32;
    let h = ((packed >> 48) & 0xFFFF) as u32;
    meuxe_ui::damage(RECT_CAP, x, y, w, h);
}

fn completion(tag: u64) -> Option<i32> {
    let mut found = None;
    while let Some(entry) = ring().cq.pop() {
        if entry.user_data == tag {
            found = Some(entry.result);
        }
    }
    found
}

fn post_recv() {
    let recv = SubmissionEntry {
        opcode: SQ_OPCODE_RECV,
        flags: 0,
        cap: KEY_CAP,
        a: KEY_BOX,
        b: 0,
        user_data: 2,
    };
    if ring().sq.push(recv).is_err() {
        return;
    }
    syscall(SYS_RING_PROCESS, 0, 0);
}

fn columns(boot: &ClientBoot) -> u32 {
    boot.cols.min(TERM_COLS)
}

fn rows(boot: &ClientBoot) -> u32 {
    boot.rows.min(TERM_ROWS).max(1)
}

fn index(col: u32, row: u32) -> usize {
    row as usize * TERM_COLS as usize + col as usize
}

fn set_cell(col: u32, row: u32, ch: u8, accent: u8) {
    let slot = index(col, row);
    unsafe {
        *cells().add(slot) = ch;
        *ink().add(slot) = accent;
    }
}

fn read_cell(col: u32, row: u32) -> u8 {
    unsafe { *cells().add(index(col, row)) }
}

fn read_ink(col: u32, row: u32) -> u8 {
    unsafe { *ink().add(index(col, row)) }
}

fn cells() -> *mut u8 {
    unsafe { (*SCREEN.bytes.get()).as_mut_ptr() }
}

fn ink() -> *mut u8 {
    unsafe { (*INK.bytes.get()).as_mut_ptr() }
}
