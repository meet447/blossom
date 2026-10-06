OUTPUT_FORMAT(elf64-x86-64)
ENTRY(_start)

PHDRS
{
    text   PT_LOAD FLAGS(5);
    rodata PT_LOAD FLAGS(4);
    data   PT_LOAD FLAGS(6);
}

SECTIONS
{
    . = 0xffffffff80000000;

    .text ALIGN(4096) : {
        __text_start = .;
        *(.text.boot)
        *(.text .text.*)
        . = ALIGN(4096);
        __text_end = .;
    } :text

    .rodata ALIGN(4096) : {
        __rodata_start = .;
        *(.rodata .rodata.*)
        . = ALIGN(4096);
        __rodata_end = .;
    } :rodata

    .data ALIGN(4096) : {
        __data_start = .;
        KEEP(*(.requests_start))
        KEEP(*(.requests))
        KEEP(*(.requests_end))
        *(.data .data.*)
        . = ALIGN(4096);
        __data_end = .;
    } :data

    .bss ALIGN(4096) : {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(4096);
        __bss_end = .;
    } :data

    .got ALIGN(4096) : {
        __got_start = .;
        *(.got .got.*)
        *(.got.plt)
        *(.data.rel.ro .data.rel.ro.*)
        . = ALIGN(4096);
        __got_end = .;
    } :data

    __kernel_end = .;

    /DISCARD/ : {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.comment)
        *(.note .note.*)
    }
}
