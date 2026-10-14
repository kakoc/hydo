# load: 0x1000
# M0 guest: 16-bit real mode. Prints a banner over the 16550 UART (port 0x3f8)
# then halts. Proves the KVM run loop, memory mapping, and IO-exit path.
.code16
.global _start
_start:
    cli
    xor     %ax, %ax
    mov     %ax, %ds
    mov     %ax, %es
    mov     %ax, %ss
    mov     $0x7000, %sp
    mov     $0x3f8, %dx          # UART transmit-holding register
    mov     $msg, %si
1:
    lodsb                        # AL <- DS:[SI]; SI++
    test    %al, %al
    jz      2f
    out     %al, %dx             # write byte to console
    jmp     1b
2:
    hlt
    jmp     2b

msg:
    .asciz "hydo: hello from deterministic guest\n"
