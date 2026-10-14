# load: 0x100000
# M6 guest: a compute-heavy tight loop with NO I/O between instructions.
# This is the workload where the instruction clock matters most: thousands of
# instructions execute with no natural VM exits, so the exit-count clock barely
# advances, while the instruction clock ticks on every instruction and can place
# interrupts at exact instruction offsets throughout the loop.
#
# The interrupt handler for vector 0x20 counts deliveries in RAM. After the loop
# the guest checkpoints the accumulator and the interrupt count.
.code64
.global _start

.set IDT_GATE, 0x5200          # IDT base 0x5000 + vector 0x20 * 16
.set COUNTER,  0x320000

_start:
    # --- install IDT gate for vector 0x20 ---
    lea     handler(%rip), %rax
    mov     $IDT_GATE, %edi
    mov     %ax, (%rdi)
    movw    $0x0008, 2(%rdi)
    movb    $0x00, 4(%rdi)
    movb    $0x8E, 5(%rdi)
    mov     %rax, %rdx
    shr     $16, %rdx
    mov     %dx, 6(%rdi)
    mov     %rax, %rdx
    shr     $32, %rdx
    mov     %edx, 8(%rdi)
    movl    $0, 12(%rdi)

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
    sti

    # --- tight arithmetic loop: no I/O, ~6 instructions/iteration ---
    xor     %rax, %rax            # accumulator
    mov     $0x9e3779b1, %r9d     # mixing constant (zero-extended into r9)
    mov     $5000, %rcx           # iterations
3:
    add     %rcx, %rax
    xor     %rcx, %rax
    imul    %rcx, %rax
    add     %r9, %rax
    dec     %rcx
    jnz     3b

    # --- checkpoint accumulator (low 32) and interrupt count ---
    mov     $0x52c, %dx
    out     %eax, %dx             # accumulator low 32 bits
    mov     $COUNTER, %rdi
    mov     (%rdi), %eax
    mov     $0x52c, %dx
    out     %eax, %dx             # interrupts delivered

    # --- exit(0) ---
    xor     %eax, %eax
    mov     $0x528, %dx
    out     %eax, %dx
    hlt

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
    .asciz "hydo64: compute-heavy tight loop\n"
