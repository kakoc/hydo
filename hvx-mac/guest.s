// Tiny AArch64 EL1 guests for the HVF capability probes (MMU off, PIC).
// Copied as one blob to IPA 0x10000000; entries are offsets from guest_blob.
	.text
	.balign 4096
	.globl _guest_blob, _guest_blob_end
	.globl _g_hvc, _g_mmio, _g_time, _g_pmu, _g_step, _g_irq, _g_wfi, _g_ids, _g_compute, _g_cntfrq, _g_cntp, _g_vectors, _g_spin, _g_wfe
_guest_blob:
_g_hvc:			// hvc forever (exit latency)
1:	hvc	#0
	b	1b
_g_mmio:		// x2 = unmapped address
	mov	x1, #0x55
	str	x1, [x2]
	ldr	x3, [x2]
	hvc	#9
_g_time:		// x2 = buffer, x3 = count: store cntvct, cntpct per iteration, hvc between
1:	mrs	x0, cntvct_el0
	str	x0, [x2], #8
	mrs	x0, cntpct_el0
	str	x0, [x2], #8
	hvc	#0
	subs	x3, x3, #1
	b.ne	1b
	hvc	#9
_g_cntfrq:
	mrs	x0, cntfrq_el0
	hvc	#9
_g_pmu:
	mrs	x0, id_aa64dfr0_el1
	mrs	x1, pmcr_el0
	mrs	x4, pmccntr_el0
	hvc	#9
_g_step:		// exactly 1 + 100*2 instructions before the hvc
	mov	x0, #100
1:	subs	x0, x0, #1
	b.ne	1b
	hvc	#9
_g_spin:		// x3 iterations of a 2-instruction loop, no exits
1:	subs	x3, x3, #1
	b.ne	1b
	hvc	#9
_g_wfe:
	sevl
	wfe
	wfe
	yield
	hvc	#9
_g_wfi:
	wfi
	hvc	#9
_g_cntp:		// physical timer registers: do accesses exit?
	mov	x0, #1
	msr	cntp_cval_el0, x0
	msr	cntp_ctl_el0, x0
	mrs	x1, cntp_tval_el0
	mrs	x4, cntp_ctl_el0
	mrs	x5, cntv_ctl_el0
	hvc	#9
_g_ids:
	mrs	x0, id_aa64isar0_el1
	mrs	x1, id_aa64mmfr0_el1
	mrs	x4, id_aa64dfr0_el1
	mrs	x5, midr_el1
	mrs	x6, s3_3_c2_c4_0	// RNDR (undefined if FEAT_RNG absent)
	mrs	x7, s3_3_c2_c4_0
	hvc	#9
_g_irq:			// count instructions in x5 until an IRQ arrives
	adr	x0, vectors
	msr	vbar_el1, x0
	isb
	mov	x5, #0
	msr	daifclr, #2
1:	add	x5, x5, #1
	b	1b
_g_compute:		// deterministic mixing over regs, memory (x2) and SIMD; hvc #2 midway
	mov	x3, #0
	ldr	x4, =0x9e3779b97f4a7c15
	fmov	d0, #1.5
1:	eor	x5, x5, x4
	add	x5, x5, x3
	ror	x5, x5, #17
	str	x5, [x2, x3, lsl #3]
	fmul	d0, d0, d0
	fsqrt	d0, d0
	add	x3, x3, #1
	cmp	x3, #2000
	b.ne	2f
	hvc	#2
2:	cmp	x3, #4000
	b.ne	1b
	fmov	x6, d0
	hvc	#9
	.ltorg
	.balign 2048
_g_vectors:
vectors:
	.balign 128		// 0x000 cur EL SP0 sync
	hvc	#8
	.balign 128		// 0x080 SP0 irq
	hvc	#7
	.balign 128
	hvc	#8
	.balign 128
	hvc	#8
	.balign 128		// 0x200 cur EL SPx sync
	hvc	#8
	.balign 128		// 0x280 cur EL SPx irq
	hvc	#7
	.balign 128
	hvc	#8
	.balign 128
	hvc	#8
	.balign 128
_guest_blob_end:
