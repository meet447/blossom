# Meuxe first alpha

This is the plan for the first alpha. Boot, the scheduler, the flat log, the desktop, and the current shell already work and stay as they are. Alpha adds the core that a person can actually use: a hierarchical disk, a complete filesystem command set, programs that start and exit, and a network.

## What alpha means

`make run` boots Meuxe on QEMU q35 with two CPUs. The desktop still shows the terminal, Files, and Calc. The terminal is a shell over a real disk: `ls /`, `mkdir /tmp`, `write /tmp/a hello`, `cp /tmp/a /tmp/b`, `mv /tmp/b /home/b`, `cat /home/b`, `rm /tmp/a`, and `df`. Those files are still there on the next boot. The same shell runs a program stored on that disk (`run /bin/hello` prints a line and exits 0). `run /bin/fault` faults, the shell reports it, and the kernel keeps running. The machine has one NIC at a static address. `ping 10.0.2.2` gets four replies, and `fetch 10.0.2.100` prints the status line and body of an HTTP GET. Drivers and the TCP stack stay in ring 3. The kernel keeps address spaces, scheduling, capabilities, IRQ routing, and IPC. `make verify` proves the path from serial lines and still exits 33.

## Already in the tree

Do not rebuild this. Alpha extends it.

- Limine boot, bitmap frame allocator (16 GiB), 4-level paging, 16 MiB heap, local APIC timer at 100 Hz, two CPUs, round-robin with a quantum of three ticks and one work-steal.
- Capability space with `Endpoint`, `Frame`, and `Mmio` installed at boot. `Irq`, `CNode`, and `IoPort` exist in the type enum and are never created.
- Ring IPC: `NOP`, `SEND`, `RECV`, `MAP`. `MAP` returns a physical frame number and does not install page-table entries. The ring-3 stub is the only caller. Drivers do not use it. The kernel maps every driver window before `spawn_user_elf`.
- Five syscalls: `TASK_ID` (0), `RING_PROCESS` (1), `YIELD` (2), `REPORT` (3), `WAIT_IRQ` (4).
- Fixed task table of 16. Kernel stacks are 16 KiB each (`KStack` in `kernel/src/sched/mod.rs`). Ids: 0 boot, 1 CPU-1 idle, 2 calculator, 4 mint target (not scheduled), 8 steal proof, 9 ring-3 stub, 10 VFS, 11 virtio-blk, 12 compositor, 13 terminal, 14 input, 15 Files. Free: 3, 5, 6, 7. A seventeenth task does not fit. A third CPU would take id 2 and collide with the calculator.
- User page faults log and then `halt_forever` (`kernel/src/arch/x86_64/idt.rs`).
- IRQ lanes are vectors 33, 34, and 35 only (`kernel/src/dev/irq.rs`, `BASE = 33`, `COUNT = 3`). Vector 0 passed to `WAIT_IRQ` means 33. Any task may wait on those vectors. The I/O APIC stays masked.
- Storage is virtio-blk in userspace. The on-disk format is MXLG, a 1024-byte append-only flat log at LBA 0. The driver reads that log and proof-writes LBA 2. VFS operations are list, read, and append. RPC limits: 16-byte names, 32-byte writes, 48-byte replies.
- Initramfs (MXFS) embeds the seven server ELFs in the kernel image. The kernel spawns each one once. There is no spawn, exit, or wait for a program read from disk.
- Shell builtins: `help`, `echo`, `clear`, `ls`, `cat`, `write`, `fetch` / `neofetch`. Any other line prints `?`.
- Desktop: software framebuffer, virtio-tablet, virtio-keyboard, Files, Calc.
- No guest networking. `make verify` is the acceptance test and exits 33.

## What is missing

### Kernel core

| Gap | Why alpha needs it |
| --- | --- |
| Task table of 16, idle id equal to CPU index | A net server and spawned programs do not fit. Id 2 is both the calculator and the idle thread a third CPU would need. |
| No task exit or slot reclaim | A program cannot finish, and its address space cannot be reused. |
| User page fault halts the machine | Running disk programs is unsafe until a fault kills that task only. |
| Spawn only from the initramfs, only at boot | The shell cannot run an ELF it read from disk. |
| `WAIT_IRQ` is not capability-checked | Four drivers will share the vector space. An `Irq` capability should name the vector a task may sleep on. |
| IRQ lanes stop at vector 35 | virtio-net needs vector 36. |
| No userspace timer wake | TCP retransmit needs a tick. A server can only block on a device vector today. |

### Block and filesystem

| Gap | Why alpha needs it |
| --- | --- |
| Block driver touches LBA 0 and LBA 2, one request at a time | A filesystem needs ranged reads and writes and the device capacity. |
| MXLG is a 1024-byte flat log: no directories, delete, rename, or seek | The command set below cannot be implemented on it. |
| VFS RPC is three operations with tiny payloads | Creating, removing, renaming, stating, and copying files needs a path-based protocol that can move an ELF. |

### Shell

The shell has no working directory and no `cd`, `pwd`, `mkdir`, `rm`, `mv`, `cp`, `touch`, `stat`, `df`, or `append`. `ls`, `cat`, and `write` take a bare record name, not a path.

### Processes

No spawn, exit, wait, or argument passing. No on-disk programs. `ps` has nothing to read.

### Network

No virtio-net driver, no Ethernet, ARP, IPv4, ICMP, or TCP, and no shell commands that use them.

### Verification

`make verify` pins the flat log (`blk sector=MXLG`, `vfs note=meuxe-phase3`) and types only `echo`, `ls`, `cat`, and `write`.

## Out of this alpha

VirtIO-GPU, Wayland, ext2 or any foreign filesystem, NVMe, AHCI, PS/2, more than two CPUs, TLS, DNS, DHCP, UDP, IPv6, uids and permissions, a POSIX syscall table, file descriptors, pipes, background jobs, signals, userspace `mmap` (`MAP` stays a frame-number lookup), a journal, TrueType, x2APIC, SMEP, and SMAP. The initramfs stays MXFS and still carries the servers. User programs come from the data disk.

## Design

### Task table

`task::MAX` becomes 64. Kernel stacks grow with it: 64 × 16 KiB is 1 MiB of BSS. Ids 0 through 7 are reserved for idle threads, one per CPU index, so the calculator collision goes away even though alpha still boots two CPUs.

| Id | Role |
| --- | --- |
| 0 | Bootstrap processor boot thread |
| 1 | CPU 1 idle |
| 2–7 | Reserved for later CPUs. Not scheduled |
| 8 | Steal proof (unchanged; verify still greps `ap_task=8`) |
| 9 | Ring-3 stub (unchanged; verify still greps `task=9`) |
| 10 | VFS |
| 11 | virtio-blk |
| 12 | Compositor |
| 13 | Terminal |
| 14 | Input |
| 15 | Files |
| 16 | Calculator (moved from 2) |
| 17 | Network server |
| 18 | Capability mint target (moved from 4, still not scheduled) |
| 19–23 | Spare static servers |
| 24–63 | Spawned programs |

`sched` allocates 24–63 from a bitmap that starts with bits 0–23 set, and frees a slot when the child is reaped.

### Filesystem: MXDF

MXDF replaces MXLG on the data disk. Magic `0x4644_584D` ("MXDF"), version 1. It lives in `meuxe-fs` behind a small block trait so the same code runs in the VFS and in host tests. The crate stays `no_std` and does not allocate. The VFS keeps a fixed block cache.

Alpha geometry: 4096-byte blocks, a 64 MiB image (16384 blocks), 256 inodes.

| Blocks | Contents |
| --- | --- |
| 0 | Superblock: magic, version, block size, block count, inode count, bitmap block, inode table, first data block, root inode, mount count, free blocks, label |
| 1 | Block bitmap |
| 2–5 | Inode table, 64-byte inodes |
| 6– | File and directory data |

An inode holds a kind (file or directory), link count, size, a tick timestamp, eight direct block numbers, and one indirect block. That caps a file near 4 MiB, enough for the alpha programs. Inode 0 is unused. Inode 1 is `/`.

A directory entry is 32 bytes: inode, kind, name length, and a 24-byte name. A name is 1–24 bytes and contains no slash. `.` and `..` are resolved by the path walker and are not stored. A zero inode is a free slot. Paths on the wire are absolute, at most 128 bytes, at most eight components.

Write order is data blocks, then the inode, then the bitmap, then the directory entry. There is no journal. `mkfs` rebuilds a broken image. The VFS increments `mount_count` at start so a second boot can show the disk survived.

`packlog` becomes `mkfs`. It writes a 64 MiB image containing `/bin/hello`, `/bin/fault`, `/etc/motd`, `/home/note` (body `meuxe-phase3`, so the existing note check still means something), `/home/hello`, and an empty `/tmp`.

VFS clients keep a 4 KiB share page. The request carries an operation, flags, offset, length, a path, and an auxiliary path (the rename target). The payload sits at offset 512 and may be up to 3584 bytes. The reply overwrites the header with a result code and a length.

| Op | Behavior |
| --- | --- |
| list | Directory entries, continued with an offset in the flags |
| read | Bytes at an offset, at most 3584 |
| write | Bytes at an offset. Flags: create, truncate, append |
| stat | Kind, size, links, time, inode. A flag on `/` returns free-space counts for `df` |
| mkdir | Create a directory |
| unlink | Remove a file or an empty directory |
| rename | Move a path. The target must not already exist |

Errors reuse the ABI codes (`ERR_INVAL`, `ERR_BADF`, `ERR_NOMEM`, `ERR_PERM`, `ERR_FAULT`). `cp` is a shell loop of read and write. `touch` is a create with an empty write.

### Block driver

The VFS and block driver share nine pages at `USER_SHARE` (`0xE20000`). That window ends at `0xE29000`, below the keyboard event page at `0xE30000`. Page 0 is the request. Pages 1–8 are a 32 KiB data window. A request is a read, a write, or an info query, plus an LBA and a sector count up to 64. The driver builds one virtio-blk request with a header, one descriptor per page, and a status byte. Pages need not be physically contiguous because the kernel mapped them. Info returns the device capacity and logs `meuxe: blk capacity=131072`. The LBA 2 proof write goes away. `blk write=ok` stays, emitted on the first successful VFS write.

### Shell commands

Every command except `run` is a builtin. `run` is the only spawn.

| Command | Backing |
| --- | --- |
| `help`, `echo`, `clear`, `neofetch` | Local |
| `pwd`, `cd` | Shell working directory. Relative paths are resolved before the RPC |
| `ls`, `cat`, `stat` | list, read, stat |
| `write`, `append`, `touch` | write with truncate, append, or create |
| `mkdir`, `rm` | mkdir, unlink |
| `mv` | rename |
| `cp` | read/write loop |
| `df` | stat of `/` with the free-space flag |
| `run <path> [arg]` | Read the ELF, `SYS_SPAWN`, wait, print the child's line and exit |
| `ps` | Task-state bytes the kernel mirrors into the info page |
| `net` | MAC, address, gateway, counters, ARP cache |
| `ping <ip>` | Four ICMP echoes |
| `fetch <ip> [path]` | One TCP HTTP GET |

`fetch` stops being an alias of `neofetch`. `neofetch` remains the system-info command. Anything else prints `?`. Each command reports one `meuxe: shell …` line through `SYS_REPORT` so verify can grep a single line.

### Processes

Two syscalls and one ring operation.

- `SYS_SPAWN` (5) takes the address and length of an ELF the caller has already read into its own memory. The terminal gets a 256 KiB window at `USER_IMAGE` (`0x14000000`), above the calculator buffer. The kernel checks the range, loads the ELF into a fresh address space, allocates an id from 24–63, maps a user stack and the child's ring page, and maps one shared frame at `USER_SHARE` in the child and at `USER_CHILD` (`0xE40000`) in the parent. The optional argument is copied into that frame first. The child receives an endpoint back to the parent, its ring frame, and the share frame. The parent receives a `CNode` capability for that child. Alpha allows one live child per parent.
- `SYS_EXIT` (6) marks the task a zombie, records the code, and wakes a parent parked on that `CNode`.
- `WAIT` is ring opcode 4 on the `CNode`. It completes with the exit code, or `ERR_FAULT` if the child was killed. The kernel then drops the address space, the frames, and the id.

The child writes its output into the share and sends on its endpoint. The parent reads `USER_CHILD` and draws the line. `/bin/hello` writes `hello from disk` and exits 0. `/bin/fault` loads from `0xdead0000`.

A user page fault in a spawned task logs `meuxe: fault task=N vector=14 cr2=… killed`, exits that task with `ERR_FAULT`, and schedules something else. A fault in a static server or in the kernel still halts. Losing the block driver or the compositor is not recoverable here.

`ps` reads a 64-byte state table the kernel writes at `USER_INFO + 512` on each state change.

### Network

`meuxe-net` is a ring-3 server, task 17. The kernel finds virtio-net (PCI device ids `0x1000` and `0x1041`), programs MSI-X entry 0 to vector 36 for receive, transmit, and config, and maps the BAR, notify, and ISR windows the same way it maps virtio-blk. It also maps a 64 KiB DMA arena at `USER_NET_DMA` (`0xE50000` through `0xE5FFFF`, still below `USER_FRONT` at `0xF00000`) and a share page at `USER_NET` (`0xF08000`, the next free page after `USER_CALC_PICK`). The driver does not call `MAP`. The kernel installs an `Irq` capability for vector 36 and an endpoint shared with the terminal.

The stack is a new `no_std` crate, `meuxe-netstack`, with no allocator and no crates.io dependencies. Modules: Ethernet, an eight-entry ARP cache, IPv4 without fragments or options, ICMP echo, and TCP. Host tests check golden packets.

Static configuration, no DHCP: `10.0.2.15/24`, gateway `10.0.2.2`. The MAC comes from the device config (QEMU's `52:54:00:12:34:56`). ARP for the gateway covers anything outside the subnet.

TCP is one connection, active open only. The window is 4 KiB. One segment is in flight. The retransmit timer is 20 ticks, five retries. The client sends the request, reads until FIN, and closes. `TIME_WAIT` is one second because only one connection exists.

Time comes from the existing timer. `WAIT_IRQ` may take vector 32 as a second wake, and the timer interrupt signals that lane before it preempts. The tick count is mirrored into the info page. That is a 10 ms clock without a new syscall. Only the net server should sleep on the timer lane.

The terminal RPC on `USER_NET` is info, ping (four echoes, round-trip in ticks), and HTTP GET (status, and at most 3 KiB of body). The net server runs the whole exchange and replies once.

QEMU user networking:

```
-netdev user,id=n0,net=10.0.2.0/24,host=10.0.2.2,guestfwd=tcp:10.0.2.100:80-cmd:python3 scripts/http_stdio.py
-device virtio-net-pci,netdev=n0,disable-legacy=on
```

`scripts/http_stdio.py` answers one request with `HTTP/1.0 200 OK` and the body `meuxe-alpha`. SliRP answers `ping 10.0.2.2` itself. `make run` can `fetch` any literal address SliRP can reach.

### IRQs

Lanes cover vectors 32 through 47. The timer path signals lane 32. Device vectors are handed out in probe order: block 33, tablet 34, keyboard 35, net 36, so the existing verify lines stay true. `WAIT_IRQ` checks the caller's `Irq` capabilities. Passing 0 still means the task's first `Irq` capability, so the block driver can keep passing zeros. Vector 32 is allowed for every task as the tick wake.

## Phases

Each phase keeps `make verify` exiting 33. Later phases may replace a grep that the phase itself retires. Phases 5 and 6 need phase 0 and can proceed beside the filesystem work once the task id and the IRQ lane exist.

### Phase 0 — Task table and IRQ lanes

`kernel/src/task.rs`, `sched/mod.rs`, `dev/irq.rs`, `dev/pci.rs`, `arch/x86_64/idt.rs`, `arch/x86_64/syscall.rs`, `cap/mod.rs`, `service/storage.rs`, `service/desktop.rs`, and the ABI comments.

Verify adds `meuxe: tasks max=64 idle=0-7 dyn=24-63`, `meuxe: calc task=16`, `meuxe: irq lanes=32-47`, and `meuxe: irq cap task=11 vector=33`. `ap_task=8`, `task=9`, and the tablet and keyboard MSI-X lines stay.

### Phase 1 — Ranged block I/O

`servers/meuxe-blk`, the VFS request path (still MXLG, stored in block 0 of a 64 MiB image), the ABI share size, `service/storage.rs`, `packlog`, and the Makefile.

Verify adds `meuxe: blk capacity=131072` and `meuxe: blk range lba=8 sectors=64 ok`.

### Phase 2 — MXDF

`crates/meuxe-fs` (`mxdf` plus `mkfs`, host tests for create, read, unlink, rename, mkdir, a full disk, and remount), `servers/meuxe-vfs`, the ABI operation layout, the shell paths for `ls` / `cat` / `write`, Files listing `/home`, the Makefile, and `docs/ARCHITECTURE.md`.

Verify replaces `blk sector=MXLG` with `meuxe: blk super=MXDF`, adds `meuxe: vfs mount=MXDF blocks=16384 inodes=256 mounts=1` and `meuxe: shell ls=/ bin etc home tmp`, and keeps `meuxe: vfs note=meuxe-phase3` as the body of `/home/note`.

### Phase 3 — Filesystem commands

The shell working directory and `pwd`, `cd`, `stat`, `append`, `touch`, `mkdir`, `rm`, `mv`, `cp`, `df`. `scripts/wait_pointer.py` types the next command only after the previous `meuxe: shell` line.

Verify types `mkdir /tmp/d`, `write /tmp/d/a one`, `append /tmp/d/a two`, `cp /tmp/d/a /tmp/d/b`, `mv /tmp/d/b /home/b`, `cat /home/b`, `rm /tmp/d/a`, `rm /tmp/d`, and `df`. It greps `shell mkdir=/tmp/d ok`, `shell cat=/home/b onetwo`, `shell rm=/tmp/d ok`, and `shell df free=`.

### Phase 4 — Spawn, exit, and faults

ABI (`SYS_SPAWN`, `SYS_EXIT`, `WAIT`, `USER_IMAGE`, `USER_CHILD`), syscall dispatch, the page-fault path, the scheduler's zombie state and dynamic ids, `exec` teardown, IPC wait, `CNode` mint and revoke, `meuxe-rt` `exit`, new `meuxe-hello` and `meuxe-fault` servers, and the shell `run` and `ps`.

Verify types `run /bin/hello`, `run /bin/fault`, and `ps`. It greps `spawn task=24`, `child task=24 says hello from disk`, `exit task=24 code=0`, `shell run=/bin/hello exit=0`, `fault task=25 vector=14 cr2=0xdead0000 killed`, and `shell run=/bin/fault exit=fault`. QEMU still exits 33, not the fault-halt code.

### Phase 5 — virtio-net, ARP, ICMP

New `meuxe-netstack` (Ethernet, ARP, IPv4, ICMP, host tests), new `meuxe-net`, PCI probe, `service/net.rs`, the timer lane, shell `net` and `ping`, and the QEMU netdev flags.

Verify adds `meuxe: virtio-net msix vector=36`, `meuxe: net mac=52:54:00:12:34:56 ip=10.0.2.15 gw=10.0.2.2`, and `meuxe: shell ping=10.0.2.2 rx=4/4`.

### Phase 6 — TCP fetch

`meuxe-netstack` TCP with a scripted peer test, `NET_OP_GET` in the net server, shell `fetch`, and `scripts/http_stdio.py` behind `guestfwd`.

Verify adds `meuxe: tcp 10.0.2.100:80 state=established` and `meuxe: shell fetch=10.0.2.100 status=200 bytes=11 body=meuxe-alpha`.

### Phase 7 — Close the alpha

README, architecture doc, this checklist, the shell help text, and a final `meuxe: alpha ready` line after the services report. `make test` includes the new crates. The verify timeout moves from 90 seconds to 150, and every wait in the QMP script stays tied to a log line.

## Acceptance

`make test` passes, including MXDF and the network stack. `make verify` exits 33 and the log contains:

```
meuxe: tasks max=64 idle=0-7 dyn=24-63
meuxe: irq lanes=32-47
meuxe: blk capacity=131072
meuxe: blk super=MXDF
meuxe: vfs mount=MXDF blocks=16384 inodes=256 mounts=1
meuxe: vfs note=meuxe-phase3
meuxe: shell ls=/ bin etc home tmp
meuxe: shell mkdir=/tmp/d ok
meuxe: shell cat=/home/b onetwo
meuxe: shell rm=/tmp/d ok
meuxe: shell df free=
meuxe: spawn task=24
meuxe: child task=24 says hello from disk
meuxe: exit task=24 code=0
meuxe: shell run=/bin/hello exit=0
meuxe: fault task=25 vector=14 cr2=0xdead0000 killed
meuxe: shell run=/bin/fault exit=fault
meuxe: virtio-net msix vector=36
meuxe: net mac=52:54:00:12:34:56 ip=10.0.2.15 gw=10.0.2.2
meuxe: shell ping=10.0.2.2 rx=4/4
meuxe: tcp 10.0.2.100:80 state=established
meuxe: shell fetch=10.0.2.100 status=200 bytes=11 body=meuxe-alpha
meuxe: alpha ready
```

The pre-existing greps stay, except `blk sector=MXLG`. A second `make run` logs `mounts=2`, and `cat /home/b` still prints `onetwo`. `run /bin/fault` leaves the desktop, Files, Calc, and the shell responsive.

## Risks

- Moving the calculator from id 2 and the mint target from id 4 will break any check that assumed those numbers. Search for `CALC`, `MINT_TARGET`, and the CPU-index idle special case before phase 0. The dynamic bitmap must never return an id below 24.
- The flat log is wired through `packlog`, the VFS, the shell, Files, and several verify lines. Phase 1 keeps MXLG in block 0 of the larger image so ranged I/O lands before the format changes. Phase 2 switches the format in one step and keeps `vfs note=meuxe-phase3`. `mkfs` has to be a dependency of `run` and `verify`. A bad superblock logs `meuxe: vfs bad superblock`.
- Device probe order is fixed (block, tablet, keyboard, net) so vectors 33–36 match the log lines. virtio-net uses one MSI-X entry for receive and transmit, and the driver drains both queues on every wake. The timer lane fires every 10 ms. Only the net server should sleep on it.
- The TCP stack is stop-and-wait on one connection. Checksums, sequence numbers, and the retransmit timer get golden-packet tests and a scripted handshake-GET-FIN test. SliRP answers pings to `10.0.2.2` and resets unknown hosts. A stuck connection must time out and return status 0 so the shell does not hang.
- Each new command is another QMP key batch, and the keyboard queue holds 32 events. The script waits for that command's log line before typing the next one. Ping is about four seconds. The verify timeout becomes 150 seconds.
- Killing a task from the page-fault handler has to clear a parked endpoint and drop the task from both run queues. Run `/bin/fault` twice and check that `ps` shows the slot reused.
