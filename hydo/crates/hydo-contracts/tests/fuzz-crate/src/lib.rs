//! The fixture of hydo-contracts' tests/fuzz.rs. Each function has a contract, and four have a
//! planted bug that fuzzing must find: `debit`, `avg`, `first_word` and `Account::can_pay`.

/// An account.
pub struct Account {
    pub balance: u64,
    pub last_req: u64,
}

/// Apply the debit `req_id` once. The planted bug: the request ids seen are counted per session
/// (`seen`), so a request that an earlier session applied is applied again.
// hydo: requires amount > 0 && amount <= a.balance
// hydo: ensures req_id > old(a.last_req) || a.balance == old(a.balance)
// hydo: ensures result == a.balance
pub fn debit(a: &mut Account, seen: u64, req_id: u64, amount: u64) -> u64 {
    if req_id <= seen {
        return a.balance;
    }
    a.balance -= amount;
    a.last_req = a.last_req.max(req_id);
    a.balance
}

/// The mean of the values. The planted bug: the sum overflows.
// hydo: requires len(v) > 0
// hydo: ensures result <= 4294967295
pub fn avg(v: &[u32]) -> u32 {
    v.iter().sum::<u32>() / v.len() as u32
}

/// The text before the first space. The contract is wrong on purpose: a word can be longer.
// hydo: ensures len(result) <= 8
pub fn first_word(s: &str) -> &str {
    s.split(' ').next().unwrap_or("")
}

/// Where `x` first is.
// hydo: ensures is_none(result) || some(result) < len(v)
// hydo: ensures is_none(result) || v[some(result)] == x
pub fn position_of(v: &[u8], x: u8) -> Option<usize> {
    v.iter().position(|&y| y == x)
}

impl Account {
    /// Whether the account can pay `amount`. The planted bug: `>` for `>=`.
    // hydo: ensures result == (self.balance >= amount)
    pub fn can_pay(&self, amount: u64) -> bool {
        self.balance > amount
    }
}
