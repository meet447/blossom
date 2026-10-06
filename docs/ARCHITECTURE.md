# Meuxe architecture

Meuxe is a capability-based microkernel for x86_64. The target shape is closer to seL4, Fuchsia, or Redox than to a monolithic Unix: the kernel keeps address spaces, scheduling, and object capabilities; drivers and the compositor run in isolated userspace and talk through shared rings.

This document describes what is in the tree: boot, the scheduler, storage, the desktop, and the shell. The first alpha, which adds a hierarchical disk, the filesystem command set, program spawn, and a network, is [ALPHA.md](ALPHA.md).

## Invariants

- Addresses that mean physical memory and addresses that mean virtual memory are different types (`PhysicalAddress`, `VirtualAddress<PagingLevel4>`). A virtual address must be canonical.
- Kernel text is present and global, and is neither writable nor no-execute. Rodata is present, global, and no-execute. Data, BSS, and the heap are writable and no-execute.
- There is no ambient uid or gid. A right exists only as bits on a capability (`Read`, `Write`, `Execute`, `Grant`).
- The higher-half direct map uses the offset Limine chose. Replacing CR3 must leave the boot stack and bootloader responses reachable.
- The local APIC timer handler does not allocate, take the console lock, or print. It may try a queue lock and, if that fails, resume the current task.
- I/O APIC lines stay masked. Virtio-blk completes through MSI-X on vector 33. That handler reads the ISR byte, records the event, and sends EOI. It does not allocate, log, or take a lock. x2APIC, SMEP, and SMAP are off.

## Boot

Boot protocol is Limine base revision 6. The kernel is a freestanding Rust binary (`no_std`, `alloc`) linked at `0xffffffff80000000`, entered at `_start` in long mode. `_start` enables SSE and aligns the stack, then jumps to Rust.

Physical memory is a two-level bitmap, not a buddy allocator. Limine's map is sparse: a buddy heap over the whole address space would spend its time on holes. One bit tracks a 4 KiB frame (1 = in use). A 2 MiB allocation is 512 aligned free bits. The bitmap covers at most 16 GiB. Every frame starts allocated; only `USABLE` ranges are freed, then the kernel image, framebuffer, local APIC, and I/O APICs are reserved again.

Virtual memory is a new 4-level page table. The direct map covers usable, ACPI, bootloader, kernel, framebuffer, and reserved-mapped regions at Limine's HHDM offset, using 2 MiB pages where the range fits. The GOP framebuffer is RAM and is mapped write-back so a pixel readback is meaningful. Local APIC and I/O APIC pages are uncached. `EFER.NXE` is required. Installing CR3 clears and restores CR4.PGE so global translations are flushed.

Interrupts: the 8259 PIC is masked. The local APIC is enabled in xAPIC mode. Its timer is calibrated for about 10 ms from Limine's TSC frequency, or the ACPI PM timer if that frequency is missing, then programmed periodic at 100 Hz on vector 32. Each I/O APIC redirection entry is programmed masked, destination the bootstrap processor. Stubs are hand-written so the Rust handler sees one SysV frame; vector 8 uses IST 1.

The kernel heap is 16 MiB of 2 MiB frames at `0xfffffe8000000000`, managed by a first-fit coalescing allocator behind an interrupt-masking lock. `GlobalAlloc` is installed and a two-element `Vec<u64>` is the smoke test.

An early console draws on the GOP framebuffer. The glyphs live in `meuxe-font` (public-domain 8x8, bit 0 on the left). The desktop releases the console before the compositor takes the same pixels, and the shell draws from that same table.

`meuxe-abi` defines `Rights`, `CapHandle`, `CapType`, 32-byte submission entries, 16-byte completion entries, and an SPSC ring. Boot constructs one ring on the kernel stack as a type check. It does not enter ring 3.

## Kernel layout

`kernel/src` is the alpha kernel. A new server is an id in `task.rs`, an initramfs ELF, and a function under `service` that maps pages and installs capabilities before `sched::spawn_user_elf`. Install order on a task is the handle order.

| Path | Role |
| --- | --- |
| `init` | Machine bring-up through `boot ready`, then the scheduler proof |
| `task.rs` | Every task id. The table holds 64. Ids 0–7 are idle slots |
| `exec` | Load an ET_EXEC ELF into a private address space |
| `dev` | Virtio PCI windows and MSI-X completion lanes |
| `service` | Userspace servers. `start` runs storage, then the desktop |
| `verify` | Framebuffer sampling used by `make verify` |
| `sched`, `cap`, `ipc`, `mm`, `arch` | Scheduler, capabilities, rings, paging, x86_64 |

Task ids: 0 is the boot thread and 1 is the idle thread of CPU 1. Ids 2 through 7 stay reserved for later CPUs. 8 is the steal proof, 9 is the ring-3 stub, 10 is the VFS, 11 is virtio-blk, 12 is the compositor, 13 is the terminal, 14 is input, 15 is Files, and 16 is the calculator. 17 is reserved for the network server. 18 is the capability mint target and is not scheduled. Ids 24 through 63 are for programs the shell spawns. Alpha still boots two CPUs.

## Scheduler

Each userspace server has its own PML4. The upper half is the kernel page tables, shared. `exec` loads the ELF into the lower half. The scheduler writes that task's CR3 when it switches to it. The ring-3 stub (task 9) still runs in the kernel page tables.

`meuxe-cap` is the object table and per-task capability space. Handles are 1-based slot indexes; handle 0 is null. Rights are `Read`, `Write`, `Execute`, and `Grant`. Mint requires `Grant` on the source and a subset of its rights. The bootstrap installs, on task 9, endpoint handle 1 (`Read|Write|Grant`) and frame handle 2 (`Read|Write`) for the ring page. It mints a read-only copy into task 4 and rejects a mint of `Read|Execute`.

The ring-3 proof is one page of position-independent code at `0x400000` (user, present, not writable, not no-execute), a user stack at `0x600000`, and a 4 KiB ring page at `0x800000` (user, writable, no-execute). Intermediate page-table entries carry the user bit. The first entry to ring 3 is `iretq`. Later returns use `sysretq`. `EFER.SCE` is set. `STAR` selects kernel CS `0x08` on entry and user CS `0x23` / SS `0x1b` on `sysret`.

Syscall numbers: 0 returns the task id, 1 drains that task's submission queue and returns the completion count, 2 yields and records a status word, 3 reports bytes from a user pointer (`SYS_REPORT`), and 4 sleeps until an MSI-X vector fires (`SYS_WAIT_IRQ`). The ring-3 stub marks SysV caller-saved registers clobbered across `syscall`, because the kernel dispatch is ordinary Rust and only restores `rbx`. The user stub submits NOP, SEND, RECV, and MAP, then checks four successful completions, a mailbox write of the SEND payload, and a non-zero MAP frame number. SEND and RECV rendezvous on the endpoint. A RECV mailbox must be 8-byte aligned inside the ring page. MAP returns the physical frame number (`phys >> 12`) in the completion flags; `SQ_FLAG_MAP_WRITE` requires `Write` on the frame.

Limine's multiprocessor request (xAPIC, not x2APIC) starts the second processor. The application processor loads the kernel CR3 before it touches non-direct-map memory, then installs its own GDT and TSS (`ltr` on a shared busy TSS is a general-protection fault), loads the existing IDT, binds `GS` to its `PerCpu`, and arms the local APIC timer from the bootstrap calibration. The timer vector is still 32.

The scheduler is a fixed task table with a queue per CPU, round-robin, quantum of three ticks, and one work-steal attempt. Task 0 is the boot thread and is requeued when preempted. Each application processor has an idle task that is never queued; its id is the CPU index. Task 8 is spawned onto CPU 0 with affinity for CPU 1, so CPU 1 must steal it. Task 9 (the user stub) is affine to CPU 0. The timer path uses `try_lock` only.

## Not in this kernel yet

- Unmask I/O APIC lines, or program x2APIC, SMEP, or SMAP.
- VirtIO-GPU, VirtIO-Net, NVMe, AHCI, PS/2, or a hierarchical filesystem. VirtIO-Net and the filesystem are the alpha in [ALPHA.md](ALPHA.md). VirtIO-GPU, NVMe, AHCI, PS/2, and ext2 stay out of that alpha.
- More than two CPUs. The table holds 64 tasks and reserves ids 0–7 for idle threads.

## Storage

The scheduler's ring-3 stub still runs in the kernel page tables. After `sched ready`, the kernel loads two static ELF64 executables from an in-memory initramfs. Each gets its own PML4. The upper half is the kernel's page tables, shared. The lower half is private. The scheduler writes CR3 when it switches to a task that has its own PML4.

The block driver is the `meuxe-blk` program. The kernel scans PCI, enables memory space and bus master on virtio-blk, programs MSI-X entry 0 with vector 33, and installs an MMIO capability. It maps the common-config and notify windows uncached into that driver's address space. The VFS reads and writes **MXDF** on the data disk through ranged block I/O; paths are absolute, and the shell commands in [ALPHA.md](ALPHA.md) go through that RPC.

Storage does not include a compositor, VirtIO-GPU, VirtIO-Net, NVMe, AHCI, PS/2, or Ext2. I/O APIC lines stay masked.

## Desktop

After `storage ready` the kernel stops writing the framebuffer. Later log lines are serial only. It loads three more static ELFs from the initramfs, each in its own PML4: `compositor`, `client`, and `input`.

The compositor's address space holds the GOP framebuffer (write-back, the same physical pages as the direct map), a front buffer, a back buffer, and a status page. The kernel installs two endpoints and an MMIO capability for the framebuffer. On the compositor those handles are 1 (the client endpoint), 2 (the input endpoint), and 3 (the framebuffer). The client and the input driver each hold endpoint handle 1 for their own endpoint.

The client fills a 16×16 back buffer with `0x00E07A3D` and `SEND`s one dirty rectangle at (120, 120). The compositor encodes that color for the framebuffer's channel shifts and copies the rectangle onto the screen.

The input program drives a virtio-tablet (modern PCI id `0x1052`, the first such device) on MSI-X vector 34. It posts 32 device-writable event buffers, writes queue vector 0 before enabling the queue, reaches `DRIVER_OK`, and then sets a ready byte. The kernel logs `tablet listening` only after that byte is set, which is the point where a pointer event can land in a posted buffer. `make verify` waits for that line and injects absolute axes over QMP. The driver scales each axis with `value * (span - 1) / 32767` and `SEND`s the pixel on endpoint handle 1. While the left button or touch contact is down, bit 31 of the x coordinate is set. The verify sample has no button, so that bit stays clear. The compositor hit-tests the first event against the fixed 16×16 window and reports that hit once. It draws a 12×16 arrow cursor, outline plus fill, with the hotspot at the tip. The arrow extends down and to the right, so the verify point (128, 128) does not cover pixel (120, 120). Later button-up samples drag that tile while the pointer stays inside it, saving and restoring the pixels underneath. The kernel reads pixel (120, 120) through the direct map and accepts the desktop when that pixel is the encoded window color and the hit is inside the window. `SYS_WAIT_IRQ` logs `meuxe: tablet irq`.

## Shell

The compositor draws a title bar and a border around three windows, using the shared 8×8 font at 2× for the titles `Terminal`, `Files`, and `Calc`. The cells underneath those titles are not moved. Holding the tablet button on a title drags that window; the compositor keeps the app's dirty rectangles in boot coordinates and adds the distance the frame has moved. The button on the right of the title closes the window. Term, Files, and Calc icons on the left of the desktop open a window again. A button press inside the Files window selects the record under the pointer. The calculator sits under the terminal and takes pointer presses on its keys. The verify pointer is a button-up sample, so that first event still hit-tests only the 16×16 window. A bar along the bottom of the screen names the open windows; a button press on a name focuses that window.

`meuxe_ui::theme` is the shared palette: desktop, ink, accent, panel, title, edge, close, and shadow. The calculator's arithmetic and key map live in `meuxe-ui` so a host test can press `12 + 30` without booting.

`meuxe-ui` is the application toolkit. `Canvas` fills rectangles, strokes a one-pixel frame, and draws the shared 8×8 glyphs into a window buffer. `damage` publishes the changed screen rectangle. `key_of` maps the virtio key codes. `Dir` lists, reads, and writes named records on the directory endpoint. The terminal and Files both draw through it, so a new window app uses the same calls.

The terminal client is the shell. Below the desktop window, at (64, 200), it owns a 48×16 cell surface drawn at 2× scale (768×256 pixels). The cells live in a buffer mapped into both the client and the compositor. A rectangle whose origin is at or below that panel is copied from the terminal buffer; the 16×16 window is still copied from the tightly packed back buffer.

A second virtio-input device (PCI id `0x1052`, the second one QEMU enumerates) is the keyboard, on MSI-X vector 35. The kernel maps its queue, event page, and a separate ready byte into the same input task and installs one more endpoint. That endpoint is handle 2 on both the input task and the client. The task sleeps in `SYS_WAIT_IRQ` on vector 34 or 35 when both used rings are caught up and nothing is waiting to send. Only key-down events are forwarded (`EV_KEY` with value 1), packed with bit 63 set so a key cannot look like a pointer. The kernel logs `meuxe: kbd irq` from that syscall. The terminal maps Linux evdev codes for the QWERTY rows, space, enter, and backspace.

The shell keeps a line after the `meuxe> ` prompt. Enter runs `help`, `echo` (the rest of the line after one space), `clear`, `ls`, `cat`, `write`, `fetch` (also `neofetch`), or prints `?`. `fetch` draws a blossom mark beside the os, architecture, framebuffer size, terminal size, and shell. Output of `echo hi` is what the shell check samples. `make verify` waits until `desktop ready` and `kbd listening`, then sends `echo hi` over QMP with no device name (QEMU treats that argument as a console, not a qdev id). The kernel samples the framebuffer with `meuxe-font` and logs `shell line=hi` when cell (0, 2) is `h` and cell (1, 2) is `i` in the terminal foreground on the terminal background.

## Directory

The on-disk log is the directory. There is no Ext2 and no tree: `ls` prints the record names, and `cat NAME` prints that record. A separate `files` program, task 15, owns the window on the right. It lists the same records and, when the pointer is on a name, shows that record. It has its own directory endpoint and its own request page, so the shell's endpoint stays free. The shell and the VFS share one page at `0xF04000`. The request is an operation, a name, and a status word. The shell writes it, `SEND`s on endpoint handle 3 (`user_data` 7), and yields until the status word is set. That endpoint is also handle 3 in the VFS. The VFS answers from a copy of the two sectors, so the storage `note` report can keep pointing at the share page.

After the first read, the block driver writes those 1024 bytes to sector 2 and reads them back through the same MSI-X wait. The kernel logs `meuxe: blk irq` from `SYS_WAIT_IRQ` and `meuxe: blk write=ok` when the read-back starts with `MXLG`. Vector 0 passed to that syscall still means vector 33. I/O APIC lines stay masked.

`make verify` sends a second QMP batch, `ls` and `cat note`, only after `shell line=hi`. Twelve key-downs are 24 events, which fits the keyboard queue of 32. The kernel scans every terminal row for `note hello disk` and `meuxe-phase3`, then logs `meuxe: directory ready`.

## Record write

`write NAME text` appends one record in front of the log terminator. The bytes before that terminator stay put, so `note` remains at the start of sector 0. The VFS copies the updated 1024-byte image into the block driver's buffer and `SEND`s on the existing block endpoint. The driver writes those two sectors back to sector 0 through the same MSI-X wait, then replies. The kernel logs `meuxe: fs write=ok` on that second store. The shell prints the stored text, which for the verify command is `there`.

`make verify` sends `write hi there` only after `directory ready`. Fifteen key-downs are 30 events, which still fits the keyboard queue of 32. The kernel scans for that row, logs `meuxe: shell wrote=there` and `meuxe: write ready`, and writes the debug-exit port. Without `--features verify` the kernel does not call that scan and does not write the port. `make run` stays up so a person can type `ls`, `cat note`, and `write`.

VirtIO-GPU, VirtIO-Net, NVMe, AHCI, PS/2, Ext2, TrueType, and a full Wayland protocol are not in this tree.

## Boot acceptance

`make verify` boots with `-smp 2` under OVMF. The serial log must contain `meuxe: boot ready` and `meuxe: sched ready`. The guest writes `0x10` to port `0xf4`, and QEMU exits with status 33.

Before `boot ready` the log shows a 2 MiB frame, a live CR3, the PTE flags above, a timer source, at least one I/O APIC, a heap vector of length 2, a matching framebuffer pixel, and at least eight timer ticks.

Between the two ready lines the log shows `meuxe: cpus_online=2`, task 8 running on CPU 1 with a non-zero steal count, `meuxe: syscall task=9 submit=4`, and `meuxe: user yield status=0`.

After `sched ready` the log shows distinct CR3 values for the VFS and the block driver, a virtio-blk MMIO window, `meuxe: blk sector=MXLG`, `meuxe: vfs note=meuxe-phase3`, and `meuxe: storage ready`. QEMU is given a raw disk and `virtio-blk-pci` with legacy mode disabled.

After `storage ready` the log shows `meuxe: desktop fb`, `meuxe: virtio-tablet msix vector=34`, `meuxe: tablet listening`, `meuxe: tablet irq`, `meuxe: desktop pixel=0xe07a3d`, `meuxe: desktop hit=1`, and `meuxe: desktop ready`. QEMU also has `virtio-tablet-pci` and `virtio-keyboard-pci`, both with legacy mode disabled. The verify recipe injects the pointer over a QMP socket after the tablet driver has posted its buffers.

After `desktop ready` the log shows `meuxe: virtio-keyboard msix vector=35`, `meuxe: kbd listening`, `meuxe: kbd irq`, `meuxe: shell line=hi`, and `meuxe: echo ready`. The first keystroke batch is down and up for `e c h o spc h i ret`.

After `echo ready` the log shows `meuxe: blk irq`, `meuxe: blk write=ok`, and `meuxe: directory ready`. The second batch is `l s ret c a t spc n o t e ret`.

After `directory ready` the log shows `meuxe: fs write=ok`, `meuxe: shell wrote=there`, and `meuxe: write ready`. The third batch is `w r i t e spc h i spc t h e r e ret`.
