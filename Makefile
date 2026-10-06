# Meuxe image. Run from the repository root.

LIMINE_VERSION := 12.9.2
LIMINE_DIR := third_party/limine
KERNEL := target/x86_64-unknown-none/release/meuxe-kernel
ISO := target/meuxe.iso
ISO_ROOT := target/iso
OVMF_CODE := /usr/share/OVMF/OVMF_CODE_4M.fd
OVMF_VARS_SRC := /usr/share/OVMF/OVMF_VARS_4M.fd
OVMF_VARS := target/ovmf_vars.fd

.PHONY: all test user initramfs kernel iso verify run clean

USER_RUSTFLAGS := -C relocation-model=static -C code-model=small -C panic=abort -C link-arg=-nostdlib -C link-arg=-z -C link-arg=max-page-size=4096 -C link-arg=-T$(CURDIR)/servers/user.ld
VFS_ELF := target/x86_64-unknown-none/release/meuxe-vfs
BLK_ELF := target/x86_64-unknown-none/release/meuxe-blk
DISK := target/disk.img

all: iso

COMP_ELF := target/x86_64-unknown-none/release/meuxe-compositor
CLIENT_ELF := target/x86_64-unknown-none/release/meuxe-client
FILES_ELF := target/x86_64-unknown-none/release/meuxe-files
CALC_ELF := target/x86_64-unknown-none/release/meuxe-calc
INPUT_ELF := target/x86_64-unknown-none/release/meuxe-input
HELLO_ELF := target/x86_64-unknown-none/release/meuxe-hello
FAULT_ELF := target/x86_64-unknown-none/release/meuxe-fault
NET_ELF := target/x86_64-unknown-none/release/meuxe-net

test:
	cargo test --workspace --exclude meuxe-kernel --exclude meuxe-rt --exclude meuxe-vfs --exclude meuxe-blk --exclude meuxe-compositor --exclude meuxe-client --exclude meuxe-files --exclude meuxe-calc --exclude meuxe-input --exclude meuxe-hello --exclude meuxe-fault --exclude meuxe-net

user:
	RUSTFLAGS="$(USER_RUSTFLAGS)" cargo build -p meuxe-vfs -p meuxe-blk -p meuxe-compositor -p meuxe-client -p meuxe-files -p meuxe-calc -p meuxe-input -p meuxe-hello -p meuxe-fault -p meuxe-net --release --target x86_64-unknown-none

initramfs: user
	mkdir -p target
	cargo run -p meuxe-fs --bin pack -- target/initramfs.bin vfs=$(VFS_ELF) blk=$(BLK_ELF) compositor=$(COMP_ELF) client=$(CLIENT_ELF) files=$(FILES_ELF) calc=$(CALC_ELF) input=$(INPUT_ELF) net=$(NET_ELF)
	cargo run -p meuxe-fs --bin mkfs -- $(DISK) hello=$(HELLO_ELF) fault=$(FAULT_ELF)

kernel: initramfs
	cargo build -p meuxe-kernel --release --target x86_64-unknown-none

kernel-verify: initramfs
	cargo build -p meuxe-kernel --release --target x86_64-unknown-none --features verify

$(LIMINE_DIR)/limine:
	mkdir -p third_party
	curl -fsSL -L "https://github.com/limine-bootloader/limine/releases/download/v$(LIMINE_VERSION)/limine-binary.tar.gz" -o target/limine-binary.tar.gz
	rm -rf $(LIMINE_DIR)
	mkdir -p $(LIMINE_DIR)
	tar -xzf target/limine-binary.tar.gz -C $(LIMINE_DIR) --strip-components=1
	$(MAKE) -C $(LIMINE_DIR) CC=clang

iso: kernel $(LIMINE_DIR)/limine
	rm -rf $(ISO_ROOT)
	mkdir -p $(ISO_ROOT)/boot/limine $(ISO_ROOT)/EFI/BOOT
	cp $(KERNEL) $(ISO_ROOT)/boot/meuxe
	cp limine.conf $(ISO_ROOT)/boot/limine/limine.conf
	cp limine.conf $(ISO_ROOT)/EFI/BOOT/limine.conf
	cp $(LIMINE_DIR)/limine-bios.sys $(ISO_ROOT)/boot/limine/limine-bios.sys
	cp $(LIMINE_DIR)/limine-bios-cd.bin $(ISO_ROOT)/limine-bios-cd.bin
	cp $(LIMINE_DIR)/limine-uefi-cd.bin $(ISO_ROOT)/limine-uefi-cd.bin
	cp $(LIMINE_DIR)/BOOTX64.EFI $(ISO_ROOT)/EFI/BOOT/BOOTX64.EFI
	xorriso -as mkisofs -R -r -J \
		-b limine-bios-cd.bin \
		-no-emul-boot -boot-load-size 4 -boot-info-table \
		--efi-boot limine-uefi-cd.bin \
		-efi-boot-part --efi-boot-image --protective-msdos-label \
		$(ISO_ROOT) -o $(ISO)
	$(LIMINE_DIR)/limine bios-install $(ISO)

verify: kernel-verify $(LIMINE_DIR)/limine
	rm -rf $(ISO_ROOT)
	mkdir -p $(ISO_ROOT)/boot/limine $(ISO_ROOT)/EFI/BOOT target
	cp $(KERNEL) $(ISO_ROOT)/boot/meuxe
	cp limine.conf $(ISO_ROOT)/boot/limine/limine.conf
	cp limine.conf $(ISO_ROOT)/EFI/BOOT/limine.conf
	cp $(LIMINE_DIR)/limine-bios.sys $(ISO_ROOT)/boot/limine/limine-bios.sys
	cp $(LIMINE_DIR)/limine-bios-cd.bin $(ISO_ROOT)/limine-bios-cd.bin
	cp $(LIMINE_DIR)/limine-uefi-cd.bin $(ISO_ROOT)/limine-uefi-cd.bin
	cp $(LIMINE_DIR)/BOOTX64.EFI $(ISO_ROOT)/EFI/BOOT/BOOTX64.EFI
	xorriso -as mkisofs -R -r -J \
		-b limine-bios-cd.bin \
		-no-emul-boot -boot-load-size 4 -boot-info-table \
		--efi-boot limine-uefi-cd.bin \
		-efi-boot-part --efi-boot-image --protective-msdos-label \
		$(ISO_ROOT) -o $(ISO)
	$(LIMINE_DIR)/limine bios-install $(ISO)
	cp $(OVMF_VARS_SRC) $(OVMF_VARS)
	rm -f target/boot.log target/qmp.sock target/qmp.log
	python3 scripts/wait_pointer.py target/boot.log target/qmp.sock > target/qmp.log 2>&1 & \
	waiter=$$!; \
	set +e; \
	timeout 240s qemu-system-x86_64 \
		-machine q35 \
		-cpu qemu64 \
		-m 512M \
		-smp 2 \
		-no-reboot \
		-no-shutdown \
		-display none \
		-serial file:target/boot.log \
		-cdrom $(ISO) \
		-drive if=pflash,format=raw,readonly=on,file=$(OVMF_CODE) \
		-drive if=pflash,format=raw,file=$(OVMF_VARS) \
		-drive file=$(DISK),if=none,format=raw,id=blkdisk \
		-device virtio-blk-pci,drive=blkdisk,disable-legacy=on \
		-netdev user,id=n0,net=10.0.2.0/24,host=10.0.2.2,guestfwd=tcp:10.0.2.100:80-cmd:python3 $(CURDIR)/scripts/http_stdio.py \
		-device virtio-net-pci,netdev=n0,disable-legacy=on,mac=52:54:00:12:34:56 \
		-device virtio-tablet-pci,disable-legacy=on,id=tablet \
		-device virtio-keyboard-pci,disable-legacy=on \
		-qmp unix:target/qmp.sock,server=on,wait=off \
		-device isa-debug-exit,iobase=0xf4,iosize=0x04; \
	code=$$?; \
	kill $$waiter 2>/dev/null || true; \
	set -e; \
	echo "qemu exit $$code"; \
	cat target/boot.log; \
	echo "--- qmp ---"; \
	cat target/qmp.log || true; \
	test $$code -eq 33; \
	grep -q "meuxe: boot ready" target/boot.log; \
	grep -q "meuxe: cpus_online=2" target/boot.log; \
	grep -E -q "meuxe: sched ap_task=8 cpu=1 count=[1-9][0-9]* steals=[1-9]" target/boot.log; \
	grep -q "meuxe: syscall task=9 submit=4" target/boot.log; \
	grep -q "meuxe: user yield status=0" target/boot.log; \
	grep -q "meuxe: tasks max=64 idle=0-7 dyn=24-63" target/boot.log; \
	grep -q "meuxe: irq lanes=32-47" target/boot.log; \
	grep -q "meuxe: sched ready" target/boot.log; \
	grep -q "meuxe: elf=vfs" target/boot.log; \
	grep -q "meuxe: elf=blk" target/boot.log; \
	grep -q "meuxe: cr3 distinct" target/boot.log; \
	grep -q "meuxe: virtio-blk" target/boot.log; \
	grep -q "meuxe: blk super=MXDF" target/boot.log; \
	grep -q "meuxe: blk capacity=131072" target/boot.log; \
	grep -q "meuxe: blk range lba=8 sectors=64 ok" target/boot.log; \
	grep -q "meuxe: vfs mount=MXDF blocks=16384 inodes=256 mounts=1" target/boot.log; \
	grep -q "meuxe: vfs note=meuxe-phase3" target/boot.log; \
	grep -q "meuxe: shell ls=/ bin etc home tmp" target/boot.log; \
	grep -q "meuxe: storage ready" target/boot.log; \
	grep -q "meuxe: desktop fb " target/boot.log; \
	grep -q "meuxe: tablet listening" target/boot.log; \
	grep -q "meuxe: desktop pixel=0xe07a3d" target/boot.log; \
	grep -q "meuxe: desktop hit=1" target/boot.log; \
	grep -q "meuxe: virtio-tablet msix vector=34" target/boot.log; \
	grep -q "meuxe: tablet irq" target/boot.log; \
	grep -q "meuxe: desktop ready" target/boot.log; \
	grep -q "meuxe: calc task=16" target/boot.log; \
	grep -q "meuxe: irq cap task=11 vector=33" target/boot.log; \
	grep -q "meuxe: kbd listening" target/boot.log; \
	grep -q "meuxe: virtio-keyboard msix vector=35" target/boot.log; \
	grep -q "meuxe: kbd irq" target/boot.log; \
	grep -q "meuxe: shell line=hi" target/boot.log; \
	grep -q "meuxe: echo ready" target/boot.log; \
	grep -q "meuxe: blk irq" target/boot.log; \
	grep -q "meuxe: blk write=ok" target/boot.log; \
	grep -q "meuxe: directory ready" target/boot.log; \
	grep -q "meuxe: fs write=ok" target/boot.log; \
	grep -q "meuxe: shell wrote=there" target/boot.log; \
	grep -q "meuxe: write ready" target/boot.log; \
	grep -q "meuxe: shell mkdir=/tmp/d ok" target/boot.log; \
	grep -q "meuxe: shell cat=/home/b onetwo" target/boot.log; \
	grep -q "meuxe: shell rm=/tmp/d ok" target/boot.log; \
	grep -q "meuxe: shell df free=" target/boot.log; \
	grep -q "meuxe: spawn task=24" target/boot.log; \
	grep -q "meuxe: child task=24 says hello from disk" target/boot.log; \
	grep -q "meuxe: exit task=24 code=0" target/boot.log; \
	grep -q "meuxe: shell run=/bin/hello exit=0" target/boot.log; \
	grep -q "meuxe: fault task=25 vector=14 cr2=0xdead0000 killed" target/boot.log; \
	grep -q "meuxe: shell run=/bin/fault exit=fault" target/boot.log; \
	grep -q "meuxe: virtio-net msix vector=36" target/boot.log; \
	grep -q "meuxe: net mac=52:54:00:12:34:56 ip=10.0.2.15 gw=10.0.2.2" target/boot.log; \
	grep -q "meuxe: shell ping=10.0.2.2 rx=4/4" target/boot.log; \
	grep -q "meuxe: tcp 10.0.2.100:80 state=established" target/boot.log; \
	grep -q "meuxe: shell fetch=10.0.2.100 status=200 bytes=11 body=meuxe-alpha" target/boot.log; \
	grep -q "meuxe: alpha ready" target/boot.log

run: iso
	cp $(OVMF_VARS_SRC) $(OVMF_VARS)
	qemu-system-x86_64 \
		-machine q35 \
		-cpu qemu64 \
		-m 512M \
		-smp 2 \
		-no-reboot \
		-serial stdio \
		-cdrom $(ISO) \
		-drive if=pflash,format=raw,readonly=on,file=$(OVMF_CODE) \
		-drive if=pflash,format=raw,file=$(OVMF_VARS) \
		-drive file=$(DISK),if=none,format=raw,id=blkdisk \
		-device virtio-blk-pci,drive=blkdisk,disable-legacy=on \
		-netdev user,id=n0,net=10.0.2.0/24,host=10.0.2.2,guestfwd=tcp:10.0.2.100:80-cmd:python3 $(CURDIR)/scripts/http_stdio.py \
		-device virtio-net-pci,netdev=n0,disable-legacy=on,mac=52:54:00:12:34:56 \
		-device virtio-tablet-pci,disable-legacy=on,id=tablet \
		-device virtio-keyboard-pci,disable-legacy=on

clean:
	cargo clean
	rm -rf $(ISO_ROOT) $(ISO) target/boot.log target/qmp.log target/qmp.sock target/ovmf_vars.fd
