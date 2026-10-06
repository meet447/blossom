//! Serial, memory, interrupts, and the early console, through `boot ready`.

use crate::arch::x86_64::{apic, cpu, gdt, idt, ioapic, percpu, pic, serial, syscall};
use crate::boot::{self, BootInfo};
use crate::mm;
use crate::sched;

use super::fail;

pub fn bringup() -> BootInfo {
    serial::init();
    crate::kprintln!("meuxe: hello");

    let boot = match boot::collect() {
        Ok(boot) => boot,
        Err(error) => fail(error),
    };
    crate::kprintln!(
        "meuxe: limine rev {} firmware {}",
        boot.revision,
        boot::firmware_name(boot.firmware)
    );
    crate::kprintln!("meuxe: bootloader {} {}", boot.name(), boot.version());

    let mut usable = 0u64;
    for region in boot.regions() {
        if region.kind == boot::MEMMAP_USABLE {
            usable = usable.saturating_add(region.length);
        }
    }
    crate::kprintln!(
        "meuxe: mem regions={} usable_kib={} hhdm={:#x}",
        boot.region_count,
        usable / 1024,
        boot.hhdm
    );
    crate::kprintln!(
        "meuxe: kernel phys={:#x} virt={:#x}",
        boot.kernel_phys,
        boot.kernel_virt
    );

    if let Err(error) = mm::init_frames(&boot) {
        fail(error);
    }
    crate::kprintln!("meuxe: frames free={}", mm::free_frames());
    match mm::probe_huge_frame() {
        Ok(addr) => crate::kprintln!("meuxe: huge_frame={addr:#x}"),
        Err(error) => fail(error),
    }

    let pml4 = match mm::map_kernel(&boot, mm::frames()) {
        Ok(pml4) => pml4,
        Err(error) => fail(error),
    };
    gdt::init_cpu(0, 0);
    idt::init();
    mm::activate_and_record(pml4);
    crate::kprintln!("meuxe: cr3 live {:#x}", cpu::read_cr3());
    crate::kprintln!("meuxe: post_switch free={}", mm::free_frames());
    super::check_permissions();
    sched::init(boot.bsp_lapic_id);
    percpu::bind(percpu::ptr(0));
    syscall::init_cpu();

    pic::disable();
    crate::kprintln!("meuxe: pic masked");

    let bsp = apic::id(boot.lapic_virt);
    let timer = apic::init(boot.lapic_virt, boot.tsc_hz, boot.pm_port, boot.pm_wide);
    crate::kprintln!(
        "meuxe: lapic={:#x} id={} tsc_hz={} timer_hz=100 source={} counts={}",
        boot.lapic_phys,
        bsp,
        timer.tsc_hz,
        timer.source,
        timer.counts_per_second
    );

    if boot.ioapic_count == 0 {
        fail("madt did not describe an ioapic");
    }
    for apic_info in boot.ioapics.iter().take(boot.ioapic_count) {
        let redirs = ioapic::init(apic_info.virt, bsp);
        crate::kprintln!(
            "meuxe: ioapic phys={:#x} gsi={} redirs={}",
            apic_info.phys,
            apic_info.gsi_base,
            redirs
        );
    }
    crate::kprintln!("meuxe: cpus={}", boot.cpu_count.max(1));

    if let Err(error) = mm::init_heap(boot.heap_virt, boot.heap_size as usize) {
        fail(error);
    }
    {
        let mut values = alloc::vec::Vec::new();
        values.push(1u64);
        values.push(2u64);
        crate::kprintln!("meuxe: heap vec_len={}", values.len());
    }

    let ring = meuxe_abi::SpscRing::<u16, 4>::new();
    let _ = ring.push(meuxe_abi::SQ_OPCODE_SEND);
    crate::kprintln!(
        "meuxe: abi read={} grant={} ring_len={}",
        meuxe_abi::Rights::READ.bits(),
        meuxe_abi::Rights::GRANT.bits(),
        ring.len()
    );

    if let Some(fb) = boot.fb {
        crate::kprintln!(
            "meuxe: fb {}x{} pitch={} bpp={}",
            fb.width,
            fb.height,
            fb.pitch,
            fb.bpp
        );
        crate::console::init(fb);
        cpu::mfence();
        match (crate::console::poke(2, 2, 0xAABBC0), crate::console::peek(2, 2)) {
            (Some(wrote), Some(read)) if wrote == read => {
                crate::kprintln!("meuxe: fb pixel={read:#x}");
            }
            (Some(wrote), Some(read)) => {
                crate::kprintln!("meuxe: fb mismatch wrote={wrote:#x} read={read:#x}");
                fail("framebuffer readback mismatch");
            }
            _ => fail("framebuffer is not 32 bpp"),
        }
        crate::console::splash();
        crate::console::release();
    } else {
        fail("limine did not provide a framebuffer");
    }

    cpu::sti();
    sched::idle_until(8);
    sched::idle_until(140);
    crate::kprintln!("meuxe: ticks={}", sched::ticks());
    crate::kprintln!("meuxe: boot ready");
    boot
}
