# load: 0x100000
# M4a guest: 64-bit long mode. The VMM has already installed page tables, a GDT,
# and entered long mode. This guest exercises the paravirtual determinism device
# exactly like det.s, and additionally writes a value into RAM so guest memory
# participates in the state fingerprint.
.code64
.global _start
_start:
    # --- banner over UART ---
    lea     banner(%rip), %rsi
    mov     $0x3f8, %dx
1:
    lodsb
    test    %al, %al
    jz      2f
    out     %al, %dx
    jmp     1b
2:
    # --- 4 deterministic random words -> checkpoints ---
    mov     $4, %rcx
3:
    mov     $0x520, %dx          # PV_RANDOM
    in      %dx, %eax
    mov     $0x52c, %dx          # PV_CHECK
    out     %eax, %dx
    dec     %rcx
    jnz     3b

    # --- virtual clock, read twice -> checkpoints ---
    mov     $0x524, %dx          # PV_TIME
    in      %dx, %eax
    mov     $0x52c, %dx
    out     %eax, %dx
    mov     $0x524, %dx
    in      %dx, %eax
    mov     $0x52c, %dx
    out     %eax, %dx

    # --- touch RAM so it is part of the fingerprint ---
    mov     $0x300000, %rdi
    movabs  $0xdeadbeefcafef00d, %rax
    mov     %rax, (%rdi)

    # --- exit(0) ---
    xor     %eax, %eax
    mov     $0x528, %dx          # PV_EXIT
    out     %eax, %dx
    hlt

banner:
    .asciz "hydo64: long mode determinism online\n"
