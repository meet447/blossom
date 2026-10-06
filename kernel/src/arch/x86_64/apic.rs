//! Local APIC and its timer, calibrated from the TSC or the ACPI PM timer.

use super::cpu;
use core::sync::atomic::{AtomicU64, Ordering};

const TPR: u64 = 0x80;
const EOI: u64 = 0xB0;
const SVR: u64 = 0xF0;
const LVT_TIMER: u64 = 0x320;
const LVT_LINT0: u64 = 0x350;
const LVT_LINT1: u64 = 0x360;
const LVT_ERROR: u64 = 0x370;
const INITIAL: u64 = 0x380;
const CURRENT: u64 = 0x390;
const DIVIDE: u64 = 0x3E0;

const TIMER_VECTOR: u32 = 32;
const MASK: u32 = 1 << 16;
const PERIODIC: u32 = 1 << 17;
const PM_HZ: u64 = 3_579_545;
const CALIBRATE_PM_TICKS: u32 = 35_795;

static LAPIC_VIRT: AtomicU64 = AtomicU64::new(0);
static COUNTS_PER_SECOND: AtomicU64 = AtomicU64::new(0);

pub struct TimerInfo {
    pub tsc_hz: u64,
    pub counts_per_second: u64,
    pub source: &'static str,
}

pub fn init(virt: u64, tsc_hz: u64, pm_port: u16, pm_wide: bool) -> TimerInfo {
    LAPIC_VIRT.store(virt, Ordering::Release);
    let mut base = cpu::rdmsr(cpu::IA32_APIC_BASE);
    base |= 1 << 11;
    cpu::wrmsr(cpu::IA32_APIC_BASE, base);

    write(virt, SVR, 0x1FF);
    write(virt, TPR, 0);
    mask(virt, LVT_LINT0);
    mask(virt, LVT_LINT1);
    mask(virt, LVT_ERROR);
    write(virt, DIVIDE, 0x3);

    let (counts_per_second, source) = calibrate(virt, tsc_hz, pm_port, pm_wide);
    COUNTS_PER_SECOND.store(counts_per_second, Ordering::Release);
    let initial = (counts_per_second / 100).max(1) as u32;
    write(virt, LVT_TIMER, TIMER_VECTOR | PERIODIC);
    write(virt, INITIAL, initial);

    TimerInfo {
        tsc_hz,
        counts_per_second,
        source,
    }
}

/// Arm the local APIC timer on an application processor.
///
/// The BSP already measured `counts_per_second`; every core shares that TSC rate.
pub fn init_ap() {
    let virt = LAPIC_VIRT.load(Ordering::Acquire);
    let mut base = cpu::rdmsr(cpu::IA32_APIC_BASE);
    base |= 1 << 11;
    cpu::wrmsr(cpu::IA32_APIC_BASE, base);
    write(virt, SVR, 0x1FF);
    write(virt, TPR, 0);
    mask(virt, LVT_LINT0);
    mask(virt, LVT_LINT1);
    mask(virt, LVT_ERROR);
    write(virt, DIVIDE, 0x3);
    let counts = COUNTS_PER_SECOND.load(Ordering::Acquire).max(1000);
    write(virt, LVT_TIMER, TIMER_VECTOR | PERIODIC);
    write(virt, INITIAL, (counts / 100).max(1) as u32);
}

pub fn eoi() {
    let virt = LAPIC_VIRT.load(Ordering::Acquire);
    if virt != 0 {
        write(virt, EOI, 0);
    }
}

pub fn id(virt: u64) -> u32 {
    (read(virt, 0x20) >> 24) & 0xFF
}

fn calibrate(virt: u64, tsc_hz: u64, pm_port: u16, pm_wide: bool) -> (u64, &'static str) {
    write(virt, LVT_TIMER, TIMER_VECTOR | MASK);
    write(virt, INITIAL, u32::MAX);
    let source = if tsc_hz > 1_000_000 {
        let start = cpu::rdtsc();
        let target = start + tsc_hz / 100;
        while cpu::rdtsc() < target {
            core::hint::spin_loop();
        }
        "tsc"
    } else if pm_port != 0 {
        wait_pm(pm_port, pm_wide, CALIBRATE_PM_TICKS);
        "pm-timer"
    } else {
        let start = cpu::rdtsc();
        while cpu::rdtsc().wrapping_sub(start) < 50_000_000 {
            core::hint::spin_loop();
        }
        "rdtsc-spin"
    };
    let current = read(virt, CURRENT);
    let elapsed = u32::MAX.wrapping_sub(current) as u64;
    let per_second = match source {
        "tsc" => elapsed.saturating_mul(100),
        "pm-timer" => elapsed.saturating_mul(PM_HZ) / CALIBRATE_PM_TICKS as u64,
        _ => elapsed.saturating_mul(100),
    };
    (per_second.max(1000), source)
}

fn wait_pm(port: u16, wide: bool, ticks: u32) {
    let start = read_pm(port, wide);
    loop {
        let now = read_pm(port, wide);
        let delta = if wide {
            now.wrapping_sub(start)
        } else {
            now.wrapping_sub(start) & 0x00FF_FFFF
        };
        if delta >= ticks {
            break;
        }
        core::hint::spin_loop();
    }
}

fn read_pm(port: u16, wide: bool) -> u32 {
    let value = cpu::inl(port);
    if wide {
        value
    } else {
        value & 0x00FF_FFFF
    }
}

fn mask(base: u64, offset: u64) {
    write(base, offset, read(base, offset) | MASK);
}

fn read(base: u64, offset: u64) -> u32 {
    unsafe { ((base + offset) as *const u32).read_volatile() }
}

fn write(base: u64, offset: u64, value: u32) {
    unsafe { ((base + offset) as *mut u32).write_volatile(value) }
}
