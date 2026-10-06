//! Privileged registers, port I/O, and the QEMU debug exit.

use core::arch::asm;

pub const EFER: u32 = 0xC000_0080;
pub const EFER_NXE: u64 = 1 << 11;
pub const IA32_APIC_BASE: u32 = 0x1B;

pub fn outb(port: u16, value: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
    }
}

pub fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    value
}

pub fn outw(port: u16, value: u16) {
    unsafe {
        asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags));
    }
}

pub fn inw(port: u16) -> u16 {
    let value: u16;
    unsafe {
        asm!("in ax, dx", out("ax") value, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    value
}

pub fn outl(port: u16, value: u32) {
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags));
    }
}

pub fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe {
        asm!("in eax, dx", out("eax") value, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    value
}

pub fn rdmsr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags));
    }
    ((high as u64) << 32) | low as u64
}

pub fn wrmsr(msr: u32, value: u64) {
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nomem, nostack, preserves_flags)
        );
    }
}

pub fn rdtsc() -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        asm!("rdtsc", out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags));
    }
    ((high as u64) << 32) | low as u64
}

pub fn read_cr2() -> u64 {
    let value: u64;
    unsafe { asm!("mov {}, cr2", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

pub fn read_cr3() -> u64 {
    let value: u64;
    unsafe { asm!("mov {}, cr3", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

/// Install `pml4` and flush global TLB entries by toggling CR4.PGE.
pub fn write_cr3(pml4: u64) {
    unsafe {
        let cr4: u64;
        asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags));
        // One block so the PGE clear, CR3 write, and PGE restore cannot be reordered.
        asm!(
            "mov cr4, {cleared}",
            "mov cr3, {pml4}",
            "mov cr4, {restored}",
            cleared = in(reg) cr4 & !(1 << 7),
            pml4 = in(reg) pml4,
            restored = in(reg) cr4 | (1 << 7),
            options(nostack)
        );
    }
}

pub fn nx_enabled() -> bool {
    rdmsr(EFER) & EFER_NXE != 0
}

/// Physical base of the local APIC, with x2APIC turned off when firmware left it on.
pub fn lapic_physical_base() -> u64 {
    let mut base = rdmsr(IA32_APIC_BASE);
    if base & (1 << 10) != 0 {
        base &= !((1 << 10) | (1 << 11));
        wrmsr(IA32_APIC_BASE, base);
        base |= 1 << 11;
        wrmsr(IA32_APIC_BASE, base);
    } else {
        base |= 1 << 11;
        wrmsr(IA32_APIC_BASE, base);
    }
    rdmsr(IA32_APIC_BASE) & 0x000F_FFFF_FFFF_F000
}

pub fn read_flags() -> u64 {
    let flags: u64;
    unsafe {
        asm!("pushfq", "pop {flags}", flags = out(reg) flags, options(preserves_flags));
    }
    flags
}

pub fn restore_flags(flags: u64) {
    unsafe {
        asm!("push {flags}", "popfq", flags = in(reg) flags);
    }
}

pub fn cli() {
    unsafe { asm!("cli", options(nomem, nostack, preserves_flags)) };
}

pub fn sti() {
    unsafe { asm!("sti", options(nomem, nostack, preserves_flags)) };
}

pub fn hlt() {
    unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) };
}

pub fn mfence() {
    unsafe { asm!("mfence", options(nostack, preserves_flags)) };
}

pub fn halt_forever() -> ! {
    cli();
    loop {
        hlt();
    }
}

/// QEMU `isa-debug-exit` at port 0xf4. The process status is `(code << 1) | 1`.
pub fn debug_exit(code: u32) -> ! {
    outl(0xf4, code);
    halt_forever();
}
