.section .rodata.user_stub
.global user_stub_start
user_stub_start:
    mov eax, 0
    syscall
    mov r12, rax
    mov r15, 0x800000

    mov word ptr [r15], 0
    mov qword ptr [r15 + 24], 1

    mov word ptr [r15 + 32], 1
    mov dword ptr [r15 + 36], 1
    mov rax, 0x4d45555845320001
    mov qword ptr [r15 + 40], rax
    mov qword ptr [r15 + 56], 2

    mov word ptr [r15 + 64], 2
    mov dword ptr [r15 + 68], 1
    mov rax, 0x800800
    mov qword ptr [r15 + 72], rax
    mov qword ptr [r15 + 88], 3

    mov word ptr [r15 + 96], 3
    mov word ptr [r15 + 98], 1
    mov dword ptr [r15 + 100], 2
    mov qword ptr [r15 + 120], 4

    mov dword ptr [r15 + 516], 4

    mov eax, 1
    syscall
    mov r13, rax

    xor r14d, r14d
    cmp r13, 4
    je 1f
    or r14d, 1
1:
    cmp dword ptr [r15 + 1284], 4
    je 2f
    or r14d, 2
2:
    mov rax, 0x4d45555845320001
    cmp qword ptr [r15 + 2048], rax
    je 3f
    or r14d, 4
3:
    cmp dword ptr [r15 + 1032], 0
    jne 4f
    cmp dword ptr [r15 + 1048], 0
    jne 4f
    cmp dword ptr [r15 + 1064], 0
    jne 4f
    cmp dword ptr [r15 + 1080], 0
    je 5f
4:
    or r14d, 8
5:
    cmp dword ptr [r15 + 1084], 0
    jne 6f
    or r14d, 16
6:
    mov edi, r14d
    mov rsi, r12
    mov eax, 2
    syscall
7:
    xor edi, edi
    mov eax, 2
    syscall
    jmp 7b
.global user_stub_end
user_stub_end:
