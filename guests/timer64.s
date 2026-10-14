# load: 0x100000
# M4b guest: 64-bit long mode with a deterministic timer interrupt.
# Installs an interrupt gate for vector 0x20 whose handler increments a counter
# in RAM, enables interrupts, then spins reading the virtual clock (each read is
# a VM exit, giving the VMM injection opportunities). The VMM injects vector 0x20
# on a fixed virtual-clock schedule, so the number of interrupts the guest sees
# is identical on every run. Finally it checkpoints the interrupt count.
.code64
.global _start

.set IDT_GATE, 0x5200          # IDT base 0x5000 + vector 0x20 * 16
.set COUNTER,  0x320000        # interrupt counter in RAM

_start:
    # --- install IDT gate for vector 0x20 ---
    lea     handler(%rip), %rax
    mov     $IDT_GATE, %edi
    mov     %ax, (%rdi)              # offset[0:16]
    movw    $0x0008, 2(%rdi)         # code segment selector
    movb    $0x00, 4(%rdi)           # IST = 0
    movb    $0x8E, 5(%rdi)           # present, DPL0, 64-bit interrupt gate
    mov     %rax, %rdx
    shr     $16, %rdx
    mov     %dx, 6(%rdi)             # offset[16:32]
    mov     %rax, %rdx
    shr     $32, %rdx
    mov     %edx, 8(%rdi)            # offset[32:64]
    movl    $0, 12(%rdi)             # reserved

    # --- banner ---
    lea     banner(%rip), %rsi
    mov     $0x3f8, %dx
1:
    lodsb
    test    %al, %al
    jz      2f
    out     %al, %dx
    jmp     1b
2:
    # --- one deterministic random word -> checkpoint (keeps seed meaningful) ---
    mov     $0x520, %dx
    in      %dx, %eax
    mov     $0x52c, %dx
    out     %eax, %dx

    # --- enable interrupts and spin, generating exits ---
    sti
    mov     $300, %r8
3:
    mov     $0x524, %dx              # PV_TIME read = one VM exit
    in      %dx, %eax
    dec     %r8
    jnz     3b

    # --- checkpoint how many interrupts were delivered ---
    mov     $COUNTER, %rdi
    mov     (%rdi), %eax
    mov     $0x52c, %dx
    out     %eax, %dx

    # --- exit(0) ---
    xor     %eax, %eax
    mov     $0x528, %dx
    out     %eax, %dx
    hlt

# Interrupt handler for vector 0x20: increment the RAM counter and return.
handler:
    push    %rax
    push    %rdi
    mov     $COUNTER, %rdi
    mov     (%rdi), %eax
    inc     %eax
    mov     %eax, (%rdi)
    pop     %rdi
    pop     %rax
    iretq

banner:
    .asciz "hydo64: deterministic timer interrupts\n"
