# load: 0x1000
# M1 guest: exercises the hydo paravirtual determinism device.
# Prints a banner, then reads 4 deterministic random words and the virtual clock
# (twice), recording each as a checkpoint, then exits with code 0. On a normal
# hypervisor the random/time values would differ run-to-run; under hydo they are
# identical for a given seed.
.code16
.global _start
_start:
    cli
    xor     %ax, %ax
    mov     %ax, %ds
    mov     %ax, %es
    mov     %ax, %ss
    mov     $0x7000, %sp

    # --- banner over UART ---
    mov     $0x3f8, %dx
    mov     $banner, %si
1:
    lodsb
    test    %al, %al
    jz      2f
    out     %al, %dx
    jmp     1b
2:
    # --- 4 deterministic random words -> checkpoints ---
    mov     $4, %cx
3:
    mov     $0x520, %dx          # PV_RANDOM
    in      %dx, %eax            # eax <- next PRNG word (32-bit IN)
    mov     $0x52c, %dx          # PV_CHECK
    out     %eax, %dx            # record checkpoint
    loop    3b

    # --- virtual clock, read twice -> checkpoints ---
    mov     $0x524, %dx          # PV_TIME
    in      %dx, %eax
    mov     $0x52c, %dx
    out     %eax, %dx

    mov     $0x524, %dx          # PV_TIME again
    in      %dx, %eax
    mov     $0x52c, %dx
    out     %eax, %dx

    # --- exit(0) ---
    xor     %eax, %eax
    mov     $0x528, %dx          # PV_EXIT
    out     %eax, %dx

    hlt

banner:
    .asciz "hydo: determinism device online\n"
