// tiny EL1 guests for the spin probes (MMU off, identity)
	.text
	.p2align 12
	.globl _sg_blob, _sg_blob_end, _sg_count, _sg_flag, _sg_store, _sg_drift, _sg_wfe, _sg_vectors
_sg_blob:
_sg_count:		// x3 iterations of a 2-instruction loop, then hvc #9
1:	subs	x3, x3, #1
	b.ne	1b
	hvc	#9
_sg_flag:		// spin until [x2] == 0 (a pure cycle: no writes, registers constant)
1:	ldr	w0, [x2]
	cbnz	w0, 1b
	hvc	#9
_sg_store:		// spin until [x2]==0, storing the same value to [x2+8] each time
1:	str	x5, [x2, #8]
	ldr	w0, [x2]
	cbnz	w0, 1b
	hvc	#9
_sg_drift:		// spin until [x2]==0, counting iterations in x6 (not a cycle)
1:	add	x6, x6, #1
	ldr	w0, [x2]
	cbnz	w0, 1b
	hvc	#9
_sg_wfe:		// sevl; wfe; wfe; yield; hvc #9
	sevl
	wfe
	wfe
	yield
	hvc	#9
	.p2align 11
_sg_vectors:		// any guest exception -> hvc #8 (report)
	.rept 16
	hvc	#8
	b	.
	.p2align 7
	.endr
_sg_blob_end:
