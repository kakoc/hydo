//! ledger: a tiny account server over TCP on loopback, written to show function contracts
//! (`// hydo:` comments, docs/design/contracts.md). One binary, three roles:
//!
//!   ledger serve <port>                     one account, opening balance 1,000,000
//!   ledger debit <port> <req-id> <amount>   prints the new balance; exit 1 without an answer
//!   ledger balance <port>                   prints the balance (readiness and the check)
//!   ledger local <n>                        n debits on a ledger of its own, in this process
//!                                           (what a contract costs per call)
//!
//! A client sends `DEBIT <req-id> <amount>` and waits 1 s for the answer. Without one it sends
//! the same request again on a new connection and waits 3 s more. Each request id must change
//! the balance once. The server remembers the request ids it applied per connection, which is
//! the planted bug: a retry arrives on a new connection, so a request whose answer was late (the
//! server paused) is applied twice. The contract of `apply` says what must hold, and hydo checks
//! it at every call while the system runs under faults.
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

/// The account.
pub struct Ledger {
    pub balance: u64,
    /// The highest request id applied so far (request ids of the one client only grow).
    pub last_req: u64,
    pub applied: u64,
}

/// What the server keeps per connection.
pub struct Conn {
    pub last_req: u64,
}

/// Apply the debit `req_id` of `amount`, once per request id, and return the new balance.
// hydo: requires amount > 0
// hydo: ensures req_id > old(l.last_req) || l.balance == old(l.balance)
// hydo: ensures req_id <= old(l.last_req) || l.balance == old(l.balance) - amount
// hydo: ensures result == l.balance
// hydo: input amount in 1..=10_000
#[inline(never)]
fn apply(l: &mut Ledger, c: &mut Conn, req_id: u64, amount: u64) -> u64 {
    if req_id <= c.last_req {
        // a retry on this connection: applied already
        return l.balance;
    }
    c.last_req = req_id;
    l.balance -= amount;
    l.last_req = l.last_req.max(req_id);
    l.applied += 1;
    l.balance
}

fn serve(port: &str) {
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind");
    let mut l = Ledger {
        balance: 1_000_000,
        last_req: 0,
        applied: 0,
    };
    for s in listener.incoming() {
        let Ok(s) = s else { continue };
        let mut c = Conn { last_req: 0 };
        let mut w = match s.try_clone() {
            Ok(w) => w,
            Err(_) => continue,
        };
        for line in BufReader::new(s).lines() {
            let Ok(line) = line else { break };
            let f: Vec<&str> = line.split_whitespace().collect();
            let answer = match f.as_slice() {
                ["DEBIT", id, amount] => match (id.parse(), amount.parse()) {
                    (Ok(id), Ok(amount)) => format!("OK {}", apply(&mut l, &mut c, id, amount)),
                    _ => "ERR bad numbers".to_string(),
                },
                ["BALANCE"] => format!("OK {}", l.balance),
                _ => "ERR unknown request".to_string(),
            };
            if writeln!(w, "{answer}").is_err() {
                break;
            }
        }
    }
}

/// One request and its answer within `wait`.
fn ask(port: &str, req: &str, wait: Duration) -> Option<String> {
    let mut s = TcpStream::connect(format!("127.0.0.1:{port}")).ok()?;
    s.set_read_timeout(Some(wait)).ok()?;
    writeln!(s, "{req}").ok()?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).ok()?;
    line.strip_prefix("OK ").map(|v| v.trim().to_string())
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let a: Vec<&str> = a.iter().map(String::as_str).collect();
    match a.as_slice() {
        [_, "serve", port] => serve(port),
        [_, "debit", port, id, amount] => {
            let req = format!("DEBIT {id} {amount}");
            let r = ask(port, &req, Duration::from_millis(1000))
                .or_else(|| ask(port, &req, Duration::from_millis(3000)));
            match r {
                Some(b) => println!("{b}"),
                None => std::process::exit(1),
            }
        }
        [_, "local", n] => {
            let mut l = Ledger {
                balance: u64::MAX,
                last_req: 0,
                applied: 0,
            };
            let n: u64 = n.parse().unwrap_or(0);
            for id in 1..=n {
                apply(&mut l, &mut Conn { last_req: 0 }, id, 1);
            }
            println!("{}", l.balance);
        }
        [_, "balance", port] => match ask(port, "BALANCE", Duration::from_millis(2000)) {
            Some(b) => println!("{b}"),
            None => std::process::exit(1),
        },
        _ => {
            eprintln!("usage: ledger serve <port> | debit <port> <req-id> <amount> | balance <port>");
            std::process::exit(2);
        }
    }
}
