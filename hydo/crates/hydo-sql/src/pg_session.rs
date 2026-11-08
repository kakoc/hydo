// The session API hydo-sql adds to hydo-read's PostgreSQL module (included into `crate::pg`, so
// it uses the module's private connection code). Values are read in the text format of the
// simple-query protocol, as strings; errors keep their SQLSTATE.

/// A server error: SQLSTATE and message.
#[derive(Debug, Clone)]
pub struct PgErr {
    pub code: String,
    pub msg: String,
}

/// One statement's result: column names, rows (NULL as None) and the command tag's count.
#[derive(Debug, Clone, Default)]
pub struct PgResult {
    pub tag: String,
    pub cols: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
}

/// Why a request failed: the server's error (with the index of the statement it ended, in a
/// message of several), or the connection (nothing more is known).
#[derive(Debug, Clone)]
pub enum PgFail {
    Server(PgErr, usize),
    Net(String),
}

/// An open session.
pub struct PgSession {
    pg: Pg,
    pub timeout: Duration,
}

fn server_err(b: &[u8]) -> PgErr {
    let mut i = 0;
    let (mut code, mut msg) = (Vec::new(), Vec::new());
    while i < b.len() && b[i] != 0 {
        let t = b[i];
        i += 1;
        let end = b[i..].iter().position(|&c| c == 0).map(|e| i + e).unwrap_or(b.len());
        match t {
            b'C' => code = b[i..end].to_vec(),
            b'M' => msg = b[i..end].to_vec(),
            _ => {}
        }
        i = end + 1;
    }
    PgErr {
        code: lossy(&code),
        msg: lossy(&msg),
    }
}

fn row_values(b: &[u8]) -> Vec<Option<String>> {
    let n = if b.len() >= 2 { u16::from_be_bytes([b[0], b[1]]) as usize } else { 0 };
    let mut out = Vec::with_capacity(n);
    let mut i = 2;
    for _ in 0..n {
        if i + 4 > b.len() {
            break;
        }
        let l = i32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        i += 4;
        if l < 0 {
            out.push(None);
        } else {
            let e = (i + l as usize).min(b.len());
            out.push(Some(lossy(&b[i..e])));
            i = e;
        }
    }
    out
}

impl PgSession {
    /// Connects and authenticates (`dsn`: a URL or key=value, as hydo-read takes it).
    pub fn open(dsn: &str, timeout: Duration) -> Result<PgSession, String> {
        let d = postgres_dsn(dsn, timeout);
        let o = options(&d)?;
        let pg = open(&o, Instant::now() + timeout)?;
        Ok(PgSession { pg, timeout })
    }

    /// Sends `sql` (one statement, or several separated by `;`, which PostgreSQL runs as one
    /// message) and reads every result until the server is ready again.
    pub fn simple(&mut self, sql: &str) -> Result<Vec<PgResult>, PgFail> {
        self.pg.c.deadline = Instant::now() + self.timeout;
        let mut b = sql.as_bytes().to_vec();
        b.push(0);
        self.pg.send(b'Q', &b).map_err(PgFail::Net)?;
        let mut out: Vec<PgResult> = Vec::new();
        let mut cur = PgResult::default();
        let mut err: Option<(PgErr, usize)> = None;
        loop {
            let (t, body) = self.pg.recv().map_err(PgFail::Net)?;
            match t {
                b'T' => cur.cols = row_description(&body).into_iter().map(|c| lossy(&c.name)).collect(),
                b'D' => cur.rows.push(row_values(&body)),
                b'C' => {
                    let mut p = 0;
                    cur.tag = lossy(&cstr(&body, &mut p));
                    out.push(std::mem::take(&mut cur));
                }
                b'I' => out.push(std::mem::take(&mut cur)),
                b'E' => {
                    if err.is_none() {
                        err = Some((server_err(&body), out.len()));
                    }
                    cur = PgResult::default();
                }
                b'Z' => {
                    return match err {
                        Some((e, i)) => Err(PgFail::Server(e, i)),
                        None => Ok(out),
                    }
                }
                _ => {}
            }
        }
    }

    pub fn close(&mut self) {
        let _ = self.pg.send(b'X', &[]);
    }
}

impl PgResult {
    /// The count at the end of the command tag ("UPDATE 1", "INSERT 0 1"), or -1.
    pub fn count(&self) -> i64 {
        self.tag.rsplit(' ').next().and_then(|n| n.parse().ok()).unwrap_or(-1)
    }
}
