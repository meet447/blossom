.section .text
.global syscall_entry
syscall_entry:
    swapgs
    mov gs:[24], rsp
    mov rsp, gs:[16]
    push qword ptr gs:[24]
    push rcx
    push r11
    push rbx
    mov rbx, rsp
    mov rdx, rsi
    mov rsi, rdi
    mov rdi, rax
    and rsp, -16
    call syscall_dispatch
    cli
    mov rsp, rbx
    pop rbx
    pop r11
    pop rcx
    pop rsp
    swapgs
    sysretq
