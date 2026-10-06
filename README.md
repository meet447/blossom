# Meuxe

Meuxe is an x86_64 microkernel. Kernel sources are split into `init`, one task-id table, `exec`, `dev`, `service`, and `verify`, so the next server is an id, an initramfs ELF, and a capability install ahead of `spawn_user_elf`. Isolation, capability boundaries, and zero-copy asynchronous I/O are the architecture. Boot brings up Limine, a bitmap frame allocator, higher-half 4-level paging, a local APIC timer, and an early framebuffer console. The scheduler adds a capability space, a shared submission/completion ring, a second processor, and one ring-3 task in the kernel address space. Storage loads two ELF programs into private address spaces: a virtio-blk driver and a VFS that reads an append-only log off that disk. The desktop hands that same framebuffer to a ring-3 compositor. A client paints a 16×16 window, and a virtio-tablet driver delivers a pointer the compositor hit-tests. The pointer is an arrow. Apps draw with `meuxe-ui` (`Canvas`, `damage`, `key_of`, and `Dir`). A bar along the bottom names the open windows. The shell is a terminal window: a virtio keyboard delivers key-down events, and the prompt answers `help`, `echo`, `clear`, `ls`, `cat`, `write`, and `fetch`. A second window, Files, lists the same records and opens the one under a button press. A third window, Calc, sits under the terminal and adds, subtracts, multiplies, and divides from pointer keys. `meuxe-ui` holds the shared palette and that calculator. Title bars drag while the button is held, the title button closes a window, and the Term and Files icons open it again. `ls` and `cat` read named records. `write NAME text` appends one record and stores the log back at sector 0. Virtio-blk, the tablet, and the keyboard sleep on MSI-X completions. `fetch` prints a neofetch-style mark and the machine lines.

`meuxe-abi` holds the capability and ring types. `meuxe-cap` is the object table and slot space. `meuxe-elf` and `meuxe-fs` parse the executables, the initramfs, and the on-disk log. `meuxe-font` is the shared 8×8 glyph table. The layout is [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). The first alpha, covering the filesystem command set, program spawn, and the network, is [docs/ALPHA.md](docs/ALPHA.md).

## Requirements

- Rust 1.83 (the `rust-toolchain.toml` pin) with the `x86_64-unknown-none` target
- `clang`, `make`, `curl`, `xorriso`
- QEMU (`qemu-system-x86_64`) and OVMF (`/usr/share/OVMF/OVMF_CODE_4M.fd`)
- Python 3, used by `make verify` to inject the tablet point, `echo hi`, `ls`, and `cat note` over QMP

The kernel has no crates.io dependencies. Host tests for `meuxe-mm`, `meuxe-abi`, `meuxe-cap`, `meuxe-elf`, `meuxe-fs`, and `meuxe-font` use only the standard library.

## Build and run

```sh
make test          # host tests for the memory and ABI crates
make kernel        # x86_64-unknown-none release kernel
make iso           # hybrid BIOS/UEFI ISO (downloads Limine 12.9.2)
make run           # boot the ISO under OVMF, serial on stdio
make verify        # headless -smp 2 boot; expects exit 33 and "write ready"
```

`make verify` builds the user ELFs, the initramfs, and the kernel with `--features verify`, then boots QEMU with two CPUs, a virtio-blk disk, a virtio-tablet, and a virtio keyboard. After the boot, scheduler, storage, desktop, and shell checks pass, the kernel writes `0x10` to port `0xf4` (`isa-debug-exit`). QEMU then exits with status `(0x10 << 1) | 1` = 33. The tablet point is injected once the driver has posted its buffers. `echo hi` follows `desktop ready` and `kbd listening`. `ls` and `cat note` follow `shell line=hi`. `write hi there` follows `directory ready`. `make run` does not take the verify path, so the machine stays up and the keyboard reaches the shell.

## What a successful boot logs

Serial (and the GOP console, once it is up) prints lines like:

- `meuxe: hello`
- memory-map summary, HHDM offset, kernel physical and virtual bases
- a free 2 MiB frame
- `meuxe: cr3 live` after the kernel's own page tables are installed
- PTE flags for text (read-execute), rodata (read, no-execute), data and heap (read-write, no-execute)
- local APIC timer calibration (`source=tsc` on QEMU) at 100 Hz
- masked I/O APIC redirection entries
- a two-element heap `Vec`
- framebuffer pixel readback
- at least eight timer ticks
- `meuxe: boot ready`
- `meuxe: cpus_online=2`
- task 8 stolen onto CPU 1
- `meuxe: syscall task=9 submit=4`
- `meuxe: user yield status=0`
- `meuxe: sched ready`
- distinct page tables for the VFS and the block driver
- `meuxe: blk sector=MXLG`
- `meuxe: vfs note=meuxe-phase3`
- `meuxe: storage ready`
- `meuxe: desktop fb` followed by the mode, then `meuxe: tablet listening`
- `meuxe: desktop pixel=0xe07a3d`
- `meuxe: desktop hit=1` at the injected point
- `meuxe: virtio-tablet msix vector=34` and `meuxe: tablet irq`
- `meuxe: desktop ready`
- `meuxe: kbd listening`
- `meuxe: virtio-keyboard msix vector=35` and `meuxe: kbd irq`
- `meuxe: shell line=hi`
- `meuxe: echo ready`
- `meuxe: blk irq`
- `meuxe: blk write=ok`
- `meuxe: directory ready`
- `meuxe: fs write=ok`
- `meuxe: shell wrote=there`
- `meuxe: write ready`

## Layout

```
Cargo.toml                  workspace
Makefile                    tests, ISO, QEMU
limine.conf                 boot menu
kernel/                     no_std kernel: init, task ids, exec, dev, service, verify
crates/meuxe-mm             addresses, frame bitmap, linked heap
crates/meuxe-abi            capability and ring types
crates/meuxe-cap            object table, CSpace, SEND/RECV rendezvous
crates/meuxe-elf            ELF64 ET_EXEC parser
crates/meuxe-fs             initramfs archive and append-only log
crates/meuxe-font           shared 8×8 glyphs
crates/meuxe-ui             window canvas, palette, keys, calculator, directory calls
servers/meuxe-vfs           VFS server, ring 3
servers/meuxe-blk           virtio-blk driver, ring 3
servers/meuxe-compositor    framebuffer compositor, ring 3
servers/meuxe-client        terminal shell, ring 3
servers/meuxe-files         file manager window, ring 3
servers/meuxe-calc          calculator window, ring 3
servers/meuxe-input         virtio tablet and keyboard, ring 3
docs/ARCHITECTURE.md        boot, scheduler, storage, desktop, shell
```

VirtIO-GPU, VirtIO-Net, NVMe, AHCI, PS/2, and Ext2 are not in this tree. I/O APIC lines stay masked. Virtio-blk, the tablet, and the keyboard complete through MSI-X.
