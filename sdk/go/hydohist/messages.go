// The messaging records without an invocation (`hydo guide messages`). This file exists only in
// the public module (sdk/go), not in the frozen copy in hydo/stack, whose order service records
// no messages.

package hydohist

import "strings"

// itemEnc is Enc with the separators of a messaging item, ';' and '=', encoded too, as the Rust
// SDK's item_enc does.
func itemEnc(s string) string {
	return strings.NewReplacer(";", "%3B", "=", "%3D").Replace(Enc(s))
}

// Drained records that the drain of process proc has read every message of group on channel:
// one completion record, without an invocation. A messages check with `no-loss` needs it for each
// group that had to receive the acknowledged messages; use "-" for a queue without groups.
//
//	q := hydohist.New("orders")
//	q.Drained(9, "orders", "-") // the drain read everything at the end
func (h *History) Drained(proc int, channel, group string) {
	h.Record(proc, "ok", "drained", itemEnc(channel), itemEnc(group))
}
