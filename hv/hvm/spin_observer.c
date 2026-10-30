/*
 * hvx-mac/spin_observer.c -- resolving exit-free spin loops deterministically (spin/RESEARCH.md).
 * Included by hvm.c (it uses hvm.c's statics); not a separate translation unit.
 *
 * Exact-repeat loops are always handled (HVM_SPIN_OBSERVE=0 turns it off: the old SPIN watchdog).
 * Counter loops need the copy-on-write checkpoint taken at every real exit, which is ~20x slower,
 * so that mode is opt-in: HVM_SPIN_REWIND=1 (hvmx: "auto", re-run a schedule with it on when the
 * plain observer ends it as SPIN; see explore/serve.c).
 */
/*
 * ---------------- exit-free spins: "a provable cycle is idle" ----------------
 * A guest that loops without any exit gives us no deterministic instruction at
 * which to deliver an interrupt (PROBE.md: no instruction counter on M2). But on a
 * single vCPU with no exits, nothing outside the guest can change guest memory, so
 * a loop that returns to EXACTLY the same register state without writing memory
 * will repeat forever. Every iteration is then the same state, so delivering the
 * pending interrupt at a canonical point of the cycle gives the same guest state no
 * matter WHEN we noticed the loop -- the host-timed kick is only a hint.
 *
 * On a kick (no exit for kick_s of host time): single-step the guest (exact) while
 * hashing its full register state after each step, with all of RAM write-protected
 * at stage 2. If a state repeats and one more full period repeats it again with no
 * write fault, the cycle is proven. Then step to the cycle's canonical state (the
 * one with the smallest hash among states where IRQs are unmasked), jump virtual
 * time to the timer deadline exactly as an idle WFI does, and resume: the interrupt
 * is delivered at the next entry. Anything else -- a real exit during stepping, an
 * exclusive load (stepping would break it), WFE/SEV, no repeat within the step
 * budget -- leaves the guest exactly as free-running would and resumes it.
 * Statistics live outside struct mach: the number of kicks is host-timed.
 */
static uint64_t sp_dbgreg, sp_kicks, sp_checks, sp_resolved, sp_notcycle, sp_aborted, sp_steps, sp_writes;
struct sst { uint64_t x[31], pc, cpsr, sp0, sp1, elr, spsr, fpcr, fpsr, tp0, tpro, tp1; hv_simd_fp_uchar16_t q[32]; };
static void sst_take(struct sst *s)
{
	for (int i = 0; i < 31; i++) s->x[i] = rd(HV_REG_X0 + i);
	s->pc = rd(HV_REG_PC); s->cpsr = rd(HV_REG_CPSR) & ~(1ULL << 21);   /* PSTATE.SS is ours */
	s->fpcr = rd(HV_REG_FPCR); s->fpsr = rd(HV_REG_FPSR);
	hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_SP_EL0, &s->sp0); hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_SP_EL1, &s->sp1);
	hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_ELR_EL1, &s->elr); hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_SPSR_EL1, &s->spsr);
	hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_TPIDR_EL0, &s->tp0); hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_TPIDRRO_EL0, &s->tpro);
	hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_TPIDR_EL1, &s->tp1);
	for (int i = 0; i < 32; i++) CHK(hv_vcpu_get_simd_fp_reg(vcpu, HV_SIMD_FP_REG_Q0 + i, &s->q[i]));
}
static uint64_t sst_hash(const struct sst *s)
{
	const uint8_t *b = (const uint8_t *)s; uint64_t h = 0xcbf29ce484222325ULL;
	for (size_t i = 0; i < sizeof *s; i++) { h ^= b[i]; h *= 0x100000001b3ULL; }
	return h;
}
/* stage-1 walk (4 KiB granule) to fetch the guest instruction at a virtual PC; 0 if not mapped */
static int fetch_insn(uint64_t va, uint32_t *out)
{
	uint64_t tcr = 0, ttbr = 0, sctlr = 0;
	hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_SCTLR_EL1, &sctlr);
	uint64_t pa;
	if (!(sctlr & 1)) pa = va;
	else {
		hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_TCR_EL1, &tcr);
		int hi = (va >> 63) & 1;
		int tsz = hi ? (tcr >> 16) & 63 : tcr & 63;
		hv_vcpu_get_sys_reg(vcpu, hi ? HV_SYS_REG_TTBR1_EL1 : HV_SYS_REG_TTBR0_EL1, &ttbr);
		int bits = 64 - tsz;                                  /* VA bits */
		int level = 4 - (bits - 12 + 8) / 9;                  /* first level for a 4K granule */
		uint64_t table = ttbr & 0x0000fffffffffffeULL;
		for (;;) {
			int shift = 12 + 9 * (3 - level);
			uint64_t idx = (va >> shift) & ((level == 4 - (bits - 12 + 8) / 9) ? ((1ULL << (bits - shift)) - 1) : 511);
			uint64_t dpa = (table & ~0xfffULL) + idx * 8;
			if (dpa < RAM_BASE || dpa + 8 > RAM_BASE + ram_size) return 0;
			uint64_t d; memcpy(&d, ram + (dpa - RAM_BASE), 8);
			if (!(d & 1)) return 0;
			if (level == 3) { pa = (d & 0x0000fffffffff000ULL) | (va & 0xfff); break; }
			if (!(d & 2)) { uint64_t m = (1ULL << shift) - 1; pa = (d & 0x0000fffffffff000ULL & ~m) | (va & m); break; }
			table = d & 0x0000fffffffff000ULL; level++;
		}
	}
	if (pa < RAM_BASE || pa + 4 > RAM_BASE + ram_size) return 0;
	memcpy(out, ram + (pa - RAM_BASE), 4);
	return 1;
}

/*
 * ---------------- counter loops (HVM_SPIN_REWIND=1) ----------------
 * A loop such as `while (flag) n++;` never returns to the same register state, so the
 * exact-cycle rule above cannot resolve it: the state at the kick is S_entry + k*D, and k
 * (passes since the loop was entered) depends on WHEN the host-timed kick came.
 *
 * Rule: (1) prove the loop never ends without an interrupt: across two observed passes the
 * full register state changes only in some X registers, by the same constant D each pass
 * ("induction registers"); memory is unchanged by a pass; and, decoding every instruction
 * of one stepped period, no instruction reads an induction register except the
 * `add/sub Xi, Xi, #imm` that updates it. Then the control flow and every load are
 * independent of the induction registers, so every later pass repeats the same path: the
 * loop is infinite, and EVERY run that reaches it is eventually kicked. (2) Deliver at a
 * point fixed by the guest alone: rewind to a checkpoint taken at the last real exit E,
 * re-execute deterministically with a hardware breakpoint at the loop's canonical PC p*
 * (the smallest PC of the period where IRQs are unmasked), and deliver at the first
 * arrival there. Execution from E is deterministic and the breakpoint is exact, so the
 * delivery state does not depend on the kick time.
 *
 * The checkpoint is copy-on-write: at every real exit the registers and machine state are
 * saved and the pages written since the previous exit are re-protected; the first write
 * to a page after an exit faults (stage 2), the page's old bytes are kept, and the page is
 * unprotected. Rewinding copies those bytes back. Instructions outside a small decoded set
 * make the proof fail (the run then stays "invalid", as without this mode).
 */
static int rw_on;                                     /* HVM_SPIN_REWIND */
static uint64_t sp_aff_proven, sp_aff_resolved, sp_ck_faults, sp_ck_takes, sp_rewinds;
struct cow { uint64_t pg; uint8_t *data; };
static struct cow *ckp; static int n_ck, cap_ck;      /* pages written since the checkpoint, with their bytes at E */
static struct vsnap ck_vs; static struct mach ck_M; static int ck_valid;
static int ck_has(uint64_t pg) { for (int i = 0; i < n_ck; i++) if (ckp[i].pg == pg) return 1; return 0; }
static void ck_note(uint64_t pg)                      /* first write to pg since E: keep its bytes */
{
	if (!rw_on || ck_has(pg)) return;
	if (n_ck == cap_ck) { cap_ck = cap_ck ? cap_ck * 2 : 64; ckp = realloc(ckp, sizeof *ckp * cap_ck); }
	ckp[n_ck].pg = pg; ckp[n_ck].data = malloc(0x4000); memcpy(ckp[n_ck].data, ram + (pg - RAM_BASE), 0x4000); n_ck++;
	sp_ck_faults++;
}
static void ck_protect_state(void)                    /* all of RAM read-only except the pages already saved */
{
	CHK(hv_vm_protect(RAM_BASE, ram_size, HV_MEMORY_READ | HV_MEMORY_EXEC));
	for (int i = 0; i < n_ck; i++) CHK(hv_vm_protect(ckp[i].pg, 0x4000, HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC));
}
static void ck_take(void)                             /* at a real exit, before the next entry */
{
	for (int i = 0; i < n_ck; i++) { CHK(hv_vm_protect(ckp[i].pg, 0x4000, HV_MEMORY_READ | HV_MEMORY_EXEC)); free(ckp[i].data); }
	n_ck = 0;
	if (!ck_valid) CHK(hv_vm_protect(RAM_BASE, ram_size, HV_MEMORY_READ | HV_MEMORY_EXEC));
	vtake(&ck_vs); ck_M = M; ck_valid = 1; sp_ck_takes++;
}
static void ck_rewind(void)
{
	for (int i = 0; i < n_ck; i++) memcpy(ram + (ckp[i].pg - RAM_BASE), ckp[i].data, 0x4000);
	vput(&ck_vs); M = ck_M; sp_rewinds++;
	ck_protect_state();
}
/* registers an instruction reads; 0 = not in the decoded set. *upd = the X register of a
 * self-update `add/sub Xd, Xd, #imm` (it may read an induction register), else -1. */
static int insn_reads(uint32_t i, uint32_t *rmask, int *upd)
{
	int rn = (i >> 5) & 31, rt = i & 31, rm = (i >> 16) & 31;
	*rmask = 0; *upd = -1;
#define R(r) do { if ((r) != 31) *rmask |= 1u << (r); } while (0)
	if (i == 0xd503201f || i == 0xd503203f) return 1;                         /* NOP, YIELD */
	if ((i & 0xfc000000) == 0x14000000) return 1;                             /* B */
	if ((i & 0xff000010) == 0x54000000) return 1;                             /* B.cond (flags) */
	if ((i & 0x7e000000) == 0x34000000 || (i & 0x7e000000) == 0x36000000) { R(rt); return 1; }   /* CBZ/CBNZ, TBZ/TBNZ */
	if ((i & 0x3fc00000) == 0x39400000) { R(rn); return 1; }                  /* LDR{B,H,W,X} unsigned imm */
	if ((i & 0x3fc00000) == 0x39000000) { R(rn); R(rt); return 1; }           /* STR{B,H,W,X} unsigned imm */
	if ((i & 0x3ffffc00) == 0x08dffc00) { R(rn); return 1; }                  /* LDAR{B,H,W,X} */
	if ((i & 0x7f800000) == 0x11000000 || (i & 0x7f800000) == 0x51000000) {    /* ADD/SUB imm (no flags) */
		if (rn == rt && rn != 31) { *upd = rn; return 1; }
		R(rn); return 1; }
	if ((i & 0x7f800000) == 0x31000000 || (i & 0x7f800000) == 0x71000000) { R(rn); return 1; }  /* ADDS/SUBS imm (CMP/CMN) */
	if ((i & 0x7f200000) == 0x6b000000 || (i & 0x7f200000) == 0x2b000000) { R(rn); R(rm); return 1; }  /* ADDS/SUBS reg */
#undef R
	return 0;
}
#define PG 0x4000ULL                                  /* stage-2 protection granule (16 KiB host pages) */
#define OB_MAXHITS 64
static FILE *splog;
/* observer: a hardware breakpoint at the kick PC, all of RAM write-protected at stage 2 */
static int ob_hits, ob_have_prev, sp_protected;
static uint64_t ob_pc, ob_mdscr, ob_bvr, ob_bcr;
static struct sst ob_prev, ob_prev2;
static int ob_unch_prev;                              /* the pass ending at ob_prev left memory unchanged */
static uint64_t rp_pc;                                /* replay: breakpoint at the counter loop's canonical PC */
static struct cow *dirty; static int n_dirty, cap_dirty;   /* pages written since the last hit, with their old bytes */
static void sp_protect(int on)
{
	if (!on && rw_on && ck_valid) ck_protect_state();   /* back to the checkpoint's protection, not all-writable */
	else CHK(hv_vm_protect(RAM_BASE, ram_size, on ? (HV_MEMORY_READ | HV_MEMORY_EXEC) : (HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC)));
	sp_protected = on;
}
static void dirty_add(uint64_t pg)
{
	ck_note(pg);
	for (int i = 0; i < n_dirty; i++) if (dirty[i].pg == pg) return;
	if (n_dirty == cap_dirty) { cap_dirty = cap_dirty ? cap_dirty * 2 : 64; dirty = realloc(dirty, sizeof *dirty * cap_dirty); }
	dirty[n_dirty].pg = pg; dirty[n_dirty].data = malloc(PG); memcpy(dirty[n_dirty].data, ram + (pg - RAM_BASE), PG); n_dirty++;
	CHK(hv_vm_protect(pg, PG, HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC));
	sp_writes++;
}
static int dirty_unchanged(void)
{ for (int i = 0; i < n_dirty; i++) if (memcmp(dirty[i].data, ram + (dirty[i].pg - RAM_BASE), PG)) return 0; return 1; }
static void dirty_reset(void)                          /* re-protect what was written, forget the copies */
{
	for (int i = 0; i < n_dirty; i++) { CHK(hv_vm_protect(dirty[i].pg, PG, HV_MEMORY_READ | HV_MEMORY_EXEC)); free(dirty[i].data); }
	n_dirty = 0;
}
#define BCR_ON ((0xfULL << 5) | (3 << 1) | 1)           /* BAS = all, PMC = EL1 + EL0, enabled */
static void ob_arm(uint64_t pc)
{
	if (!ob_on) {
		hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_MDSCR_EL1, &ob_mdscr);
		hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_DBGBVR0_EL1, &ob_bvr);
		hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_DBGBCR0_EL1, &ob_bcr);
		sp_protect(1);
		CHK(hv_vcpu_set_trap_debug_reg_accesses(vcpu, true));
		ob_on = 1;
	} else dirty_reset();
	ob_pc = pc; ob_hits = 0; ob_have_prev = 0; ob_unch_prev = 0;
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_MDSCR_EL1, ob_mdscr | (1 << 15) | (1 << 13)));   /* MDE | KDE */
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_DBGBVR0_EL1, pc));
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_DBGBCR0_EL1, BCR_ON));
	sp_checks++;
	if (splog) fprintf(splog, "arm at exit %llu pc 0x%llx cpsr 0x%llx\n", M.exits, pc, rd(HV_REG_CPSR));
}
static void ob_disarm(void)
{
	if (!ob_on) return;
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_DBGBCR0_EL1, ob_bcr));
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_DBGBVR0_EL1, ob_bvr));
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_MDSCR_EL1, ob_mdscr));
	wr(HV_REG_CPSR, rd(HV_REG_CPSR) & ~(1ULL << 21));
	for (int i = 0; i < n_dirty; i++) free(dirty[i].data);
	n_dirty = 0;
	if (sp_protected) sp_protect(0);
	CHK(hv_vcpu_set_trap_debug_reg_accesses(vcpu, false));
	ob_on = 0; ob_stepover = 0;
}
/* while armed, guest accesses to MDSCR_EL1 and the debug registers trap (EC 0x18, op0 = 2): emulate them on
 * the guest's own values, so the observer is invisible and a guest write is never clobbered at disarm.
 * Returns 1 if handled (not a guest-visible exit). */
static int ob_debug_reg(void)
{
	uint64_t s = esr();
	int op0 = (s >> 20) & 3, op2 = (s >> 17) & 7, op1 = (s >> 14) & 7, crn = (s >> 10) & 15, rt = (s >> 5) & 31, crm = (s >> 1) & 15, rdir = s & 1;
	if (op0 != 2 || op1 != 0 || crn != 0) return 0;
	if (!(op2 >= 4 || (crm == 2 && op2 == 2))) return 0;                /* only DBGB/WVR, DBGB/WCR, MDSCR_EL1 */
	hv_sys_reg_t r = (hv_sys_reg_t)((op0 << 14) | (op1 << 11) | (crn << 7) | (crm << 3) | op2);
	uint64_t *virt = r == HV_SYS_REG_MDSCR_EL1 ? &ob_mdscr : r == HV_SYS_REG_DBGBVR0_EL1 ? &ob_bvr : r == HV_SYS_REG_DBGBCR0_EL1 ? &ob_bcr : NULL;
	if (rdir) { uint64_t v = 0; if (virt) v = *virt; else hv_vcpu_get_sys_reg(vcpu, r, &v); xset(rt, v); }
	else { uint64_t v = xget(rt); if (virt) *virt = v; else hv_vcpu_set_sys_reg(vcpu, r, v); }
	if (r == HV_SYS_REG_MDSCR_EL1)                                       /* keep our MDE|KDE (+SS while stepping over) */
		CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_MDSCR_EL1, ob_mdscr | (1 << 15) | (1 << 13) | (ob_stepover ? 1 : 0)));
	skip();
	sp_dbgreg++;
	return 1;
}
static int is_store_exclusive(uint32_t i) { return (i & 0x3fc00000) == 0x08000000; }
static int is_wfe_sev(uint32_t i) { return i == 0xd503205f || i == 0xd503209f || i == 0xd50320bf; }   /* WFE, SEV, SEVL */
/* one exact single step inside a PROVEN cycle (never used before a proof); 0 = ok */
static int cyc_step(void)
{
	uint32_t insn;
	if (!fetch_insn(rd(HV_REG_PC), &insn) || is_store_exclusive(insn) || is_wfe_sev(insn)) return -1;
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_DBGBCR0_EL1, 0));
	for (;;) {
		CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_MDSCR_EL1, ob_mdscr | 1 | (1 << 13) | (1 << 15)));
		wr(HV_REG_CPSR, rd(HV_REG_CPSR) | (1ULL << 21));
		CHK(hv_vcpu_run(vcpu));
		if (ex->reason == HV_EXIT_REASON_CANCELED) continue;
		uint64_t e = (ex->exception.syndrome >> 26) & 0x3f, ipa = ex->exception.physical_address;
		if (e == 0x32 || e == 0x33) { sp_steps++; return 0; }
		if (e == 0x24 && sp_protected && ipa >= RAM_BASE && ipa < RAM_BASE + ram_size) { dirty_add(ipa & ~(PG - 1)); continue; }
		return -2;                                    /* a real exit inside a proven cycle: impossible */
	}
}

/* registers that differ between two states (X0-X30 only; any other difference -> -1) */
static int sst_xdiff(const struct sst *a, const struct sst *b, uint64_t d[31], uint32_t *mask)
{
	struct sst x = *a, y = *b; *mask = 0;
	for (int i = 0; i < 31; i++) { d[i] = a->x[i] - b->x[i]; if (d[i]) *mask |= 1u << i; x.x[i] = y.x[i] = 0; }
	return memcmp(&x, &y, sizeof x) ? -1 : 0;
}
/* cur, prev, prev2 at three consecutive breakpoint hits: an affine counter loop? Prove it by stepping one
 * period and decoding it (see "counter loops"). Returns 1 and the canonical PC if proven infinite. */
static int aff_prove(const struct sst *cur, uint64_t *pstar)
{
	uint64_t d1[31], d2[31]; uint32_t m1, m2;
	if (sst_xdiff(cur, &ob_prev, d1, &m1) || sst_xdiff(&ob_prev, &ob_prev2, d2, &m2) || !m1 || m1 != m2) return 0;
	for (int i = 0; i < 31; i++) if (d1[i] != d2[i]) return 0;
	dirty_reset();
	struct sst st; uint64_t best = ~0ULL; int steps = 0;
	for (;;) {
		uint32_t insn, rmask; int upd;
		sst_take(&st);
		if (steps && st.pc == cur->pc) break;
		if (!(st.cpsr & 0x80) && st.pc < best) best = st.pc;
		if (!fetch_insn(st.pc, &insn) || !insn_reads(insn, &rmask, &upd) || (rmask & m1)) return 0;
		if (steps++ > 4096 || cyc_step()) return 0;
	}
	uint64_t d3[31]; uint32_t m3;
	if (sst_xdiff(&st, cur, d3, &m3) || m3 != m1) return 0;
	for (int i = 0; i < 31; i++) if (d3[i] != d1[i]) return 0;
	if (!dirty_unchanged() || best == ~0ULL) return 0;
	*pstar = best;
	return 1;
}
enum { OB_CONT, OB_RESOLVED, OB_DEADLOCK, OB_GAVEUP, OB_REWOUND, OB_NEEDREWIND };
/* A proven exit-free loop is an idle wait (an exit-free one): deliver as idle_wait() does -- the
 * line if an interrupt is pending, else jump virtual time to the next event (timer deadline or
 * virtio completion). Nothing armed: the guest waits forever for nothing (deadlock). */
static int spin_deliver(void)
{
	if (irq_line()) return OB_RESOLVED;
	uint64_t next = next_event();
	if (next > M.vtime) { M.vtime = next; M.n_wfi_jump++; return OB_RESOLVED; }
	if (next) return OB_RESOLVED;
	return OB_DEADLOCK;
}
/* the breakpoint at ob_pc was hit (the instruction there has not executed) */
static int ob_hit(void)
{
	struct sst cur; sst_take(&cur);
	if (ob_have_prev && !memcmp(&cur, &ob_prev, sizeof cur) && dirty_unchanged()) {
		/* one full iteration returned to exactly this state and left memory as it was: on one vCPU with no
		 * exits it repeats forever. Enumerate one period by exact stepping, then deliver at the canonical state. */
		static struct sst st[4097]; static uint64_t h[4097];
		dirty_reset();
		st[0] = cur; h[0] = sst_hash(&cur);
		int p = 0;
		for (;;) {
			if (p >= 4096 || cyc_step()) { ob_disarm(); sp_notcycle++; return OB_GAVEUP; }
			p++; sst_take(&st[p]);
			if (!memcmp(&st[p], &st[0], sizeof cur)) break;
			h[p] = sst_hash(&st[p]);
		}
		if (!dirty_unchanged()) { ob_disarm(); sp_notcycle++; return OB_GAVEUP; }
		int best = -1;
		for (int k = 0; k < p; k++) if (!(st[k].cpsr & 0x80) && (best < 0 || h[k] < h[best])) best = k;
		if (best < 0) { ob_disarm(); return OB_DEADLOCK; }
		for (int k = 0; k < best; k++) if (cyc_step()) { fprintf(stderr, "hvm: spin: cycle changed\n"); exit(3); }
		ob_disarm();
		sp_resolved++;
		if (splog) fprintf(splog, "  resolved: period %d, canonical +%d at pc 0x%llx\n", p, best, rd(HV_REG_PC));
		return spin_deliver();
	}
	if (!rw_on && ob_have_prev >= 2 && ob_unch_prev && dirty_unchanged()) {
		/* without the checkpoint a counter loop cannot be resolved: once one is proven, stop at once
		 * (SPIN) instead of waiting out HVM_SPIN_SECS, so hvmx's automatic rewind re-run starts early */
		uint64_t pstar;
		if (aff_prove(&cur, &pstar)) { ob_disarm(); sp_aff_proven++; return OB_NEEDREWIND; }
	}
	if (rw_on && ck_valid && ob_have_prev >= 2 && ob_unch_prev && dirty_unchanged()) {
		uint64_t pstar;
		if (aff_prove(&cur, &pstar)) {
			sp_aff_proven++;
			if (splog) fprintf(splog, "  counter loop proven at pc 0x%llx, canonical pc 0x%llx: rewind to exit %llu\n", cur.pc, pstar, ck_M.exits);
			ob_disarm();
			ck_rewind();
			rp_pc = pstar; rp_on = 1;
			hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_MDSCR_EL1, &ob_mdscr);
			hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_DBGBVR0_EL1, &ob_bvr);
			hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_DBGBCR0_EL1, &ob_bcr);
			CHK(hv_vcpu_set_trap_debug_reg_accesses(vcpu, true));
			CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_MDSCR_EL1, ob_mdscr | (1 << 15) | (1 << 13)));
			CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_DBGBVR0_EL1, rp_pc));
			CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_DBGBCR0_EL1, BCR_ON));
			return OB_REWOUND;
		}
	}
	if (++ob_hits > OB_MAXHITS) { ob_disarm(); sp_notcycle++; if (splog) fprintf(splog, "  gave up after %d hits\n", OB_MAXHITS); return OB_GAVEUP; }
	ob_unch_prev = ob_have_prev ? dirty_unchanged() : 0;
	ob_prev2 = ob_prev; ob_prev = cur; ob_have_prev = ob_have_prev >= 2 ? 2 : ob_have_prev + 1;
	dirty_reset();
	/* step over the breakpointed instruction: breakpoint off, one software step */
	uint32_t insn;
	if (!fetch_insn(cur.pc, &insn) || is_store_exclusive(insn)) { ob_disarm(); sp_notcycle++; return OB_GAVEUP; }
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_DBGBCR0_EL1, 0));
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_MDSCR_EL1, ob_mdscr | 1 | (1 << 13) | (1 << 15)));
	wr(HV_REG_CPSR, rd(HV_REG_CPSR) | (1ULL << 21));
	ob_stepover = 1;
	return OB_CONT;
}

static void rp_disarm(void)
{
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_DBGBCR0_EL1, ob_bcr));
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_DBGBVR0_EL1, ob_bvr));
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_MDSCR_EL1, ob_mdscr));
	wr(HV_REG_CPSR, rd(HV_REG_CPSR) & ~(1ULL << 21));
	CHK(hv_vcpu_set_trap_debug_reg_accesses(vcpu, false));
	rp_on = 0; ob_stepover = 0;
}
/* replay reached the canonical PC (the instruction there has not executed) */
static int rp_hit(void)
{
	if (rd(HV_REG_CPSR) & 0x80) {                    /* IRQs masked here: step over, keep counting */
		CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_DBGBCR0_EL1, 0));
		CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_MDSCR_EL1, ob_mdscr | 1 | (1 << 13) | (1 << 15)));
		wr(HV_REG_CPSR, rd(HV_REG_CPSR) | (1ULL << 21));
		ob_stepover = 1;
		return OB_CONT;
	}
	rp_disarm();
	sp_aff_resolved++;
	if (splog) fprintf(splog, "  replay delivered at pc 0x%llx\n", rd(HV_REG_PC));
	return spin_deliver();
}
static void ob_stepover_done(void)
{
	ob_stepover = 0;
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_MDSCR_EL1, ob_mdscr | (1 << 15) | (1 << 13)));
	wr(HV_REG_CPSR, rd(HV_REG_CPSR) & ~(1ULL << 21));
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_DBGBCR0_EL1, BCR_ON));
}
