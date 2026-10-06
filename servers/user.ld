OUTPUT_FORMAT(elf64-x86-64)
ENTRY(_start)

SECTIONS {
    . = 0x400000;

    .text ALIGN(4096) : {
        *(.text.boot)
        *(.text .text.*)
    }

    .rodata ALIGN(4096) : {
        *(.rodata .rodata.*)
    }

    .data ALIGN(4096) : {
        *(.got .got.*)
        *(.data .data.*)
    }

    .bss ALIGN(4096) : {
        *(COMMON)
        *(.bss .bss.*)
    }

    /DISCARD/ : {
        *(.eh_frame .eh_frame_hdr .comment .note.*)
    }
}
