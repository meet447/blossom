//! Per-CPU GDT and TSS. User selectors are the ones `sysret` expects:
//! kernel code 0x08, kernel data 0x10, user data 0x18, user code 0x20.

use core::arch::asm;
use core::mem::size_of;
use core::ptr;

const MAX_CPUS: usize = 8;

#[repr(C, packed)]
struct Tss {
    reserved0: u32,
    rsp0: u64,
    rsp1: u64,
    rsp2: u64,
    reserved1: u64,
    ist1: u64,
    ist2: u64,
    ist3: u64,
    ist4: u64,
    ist5: u64,
    ist6: u64,
    ist7: u64,
    reserved2: u64,
    reserved3: u16,
    iopb: u16,
}

impl Tss {
    const fn empty() -> Self {
        Self {
            reserved0: 0,
            rsp0: 0,
            rsp1: 0,
            rsp2: 0,
            reserved1: 0,
            ist1: 0,
            ist2: 0,
            ist3: 0,
            ist4: 0,
            ist5: 0,
            ist6: 0,
            ist7: 0,
            reserved2: 0,
            reserved3: 0,
            iopb: 0,
        }
    }
}

#[repr(C, align(16))]
struct Stack([u8; 8192]);

#[repr(C, packed)]
struct DescriptorPointer {
    limit: u16,
    base: u64,
}

static mut GDTS: [[u64; 7]; MAX_CPUS] = [[0; 7]; MAX_CPUS];
static mut TSSS: [Tss; MAX_CPUS] = [const { Tss::empty() }; MAX_CPUS];
static mut DF_STACK: Stack = Stack([0; 8192]);

pub fn init_cpu(cpu: usize, rsp0: u64) {
    unsafe {
        let df_top = ptr::addr_of!(DF_STACK) as u64 + size_of::<Stack>() as u64;
        let tss = ptr::addr_of_mut!(TSSS).cast::<Tss>().add(cpu);
        ptr::write(
            tss,
            Tss {
                reserved0: 0,
                rsp0,
                rsp1: 0,
                rsp2: 0,
                reserved1: 0,
                ist1: df_top,
                ist2: 0,
                ist3: 0,
                ist4: 0,
                ist5: 0,
                ist6: 0,
                ist7: 0,
                reserved2: 0,
                reserved3: 0,
                iopb: size_of::<Tss>() as u16,
            },
        );

        let gdt = ptr::addr_of_mut!(GDTS).cast::<[u64; 7]>().add(cpu);
        (*gdt)[0] = 0;
        (*gdt)[1] = 0x00AF_9A00_0000_FFFF;
        (*gdt)[2] = 0x00CF_9200_0000_FFFF;
        (*gdt)[3] = 0x00CF_F200_0000_FFFF;
        (*gdt)[4] = 0x00AF_FA00_0000_FFFF;
        write_tss(&mut (*gdt)[5], &mut (*gdt)[6], tss as u64);

        let pointer = DescriptorPointer {
            limit: (size_of::<[u64; 7]>() - 1) as u16,
            base: gdt as u64,
        };
        asm!("lgdt [{ptr}]", ptr = in(reg) &pointer, options(readonly));
        asm!(
            "push {code}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            "mov ax, {data}",
            "mov ds, ax",
            "mov es, ax",
            "mov ss, ax",
            "mov fs, ax",
            "mov gs, ax",
            "mov ax, {tss}",
            "ltr ax",
            code = const 0x08u64,
            data = const 0x10u16,
            tss = const 0x28u16,
            tmp = lateout(reg) _,
            lateout("ax") _,
        );
    }
}

pub fn set_rsp0(cpu: usize, rsp0: u64) {
    unsafe {
        let tss = ptr::addr_of_mut!(TSSS).cast::<Tss>().add(cpu);
        ptr::addr_of_mut!((*tss).rsp0).write_unaligned(rsp0);
    }
}

fn write_tss(low: &mut u64, high: &mut u64, base: u64) {
    let limit = (size_of::<Tss>() - 1) as u64;
    *low = (limit & 0xFFFF)
        | ((base & 0xFFFF) << 16)
        | (((base >> 16) & 0xFF) << 32)
        | (0x89 << 40)
        | (((limit >> 16) & 0xF) << 48)
        | (((base >> 24) & 0xFF) << 56);
    *high = base >> 32;
}
