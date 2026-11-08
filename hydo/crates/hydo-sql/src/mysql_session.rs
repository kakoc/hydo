// The session API hydo-sql adds to hydo-read's MySQL module (included into `crate::mysql`, so it
// uses the module's private connection code). Values are the text protocol's strings; errors keep
// their error number.

/// One statement's result: column names, rows (NULL as None) and the affected-row count.
#[derive(Debug, Clone, Default)]
pub struct MyResult {
    pub affected: i64,
    pub cols: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
}

/// Why a statement failed: the server's error packet, or the connection.
#[derive(Debug, Clone)]
pub enum MyFail {
    Server(u16, String),
    Net(String),
}

pub struct MySession {
    my: My,
    pub timeout: Duration,
}

fn fail_text(f: Fail) -> String {
    match f {
        Fail::Bad => "driver: bad connection".into(),
        Fail::Err(e) => e,
    }
}

impl MySession {
    /// Connects and authenticates (`dsn`: the driver form or a mysql:// URL).
    pub fn open(dsn: &str, timeout: Duration) -> Result<MySession, String> {
        let d = mysql_dsn(dsn, timeout).map_err(|e| e.to_string())?;
        let cfg = parse_dsn(&d)?;
        let my = connect(&cfg, Instant::now() + timeout).map_err(fail_text)?;
        Ok(MySession { my, timeout })
    }

    /// Runs one statement.
    pub fn exec(&mut self, sql: &str) -> Result<MyResult, MyFail> {
        self.my.c.deadline = Instant::now() + self.timeout;
        let net = |f: Fail| MyFail::Net(fail_text(f));
        let d = command(&mut self.my, sql).map_err(net)?;
        let server = |d: &[u8]| {
            let no = if d.len() >= 3 { u16::from_le_bytes([d[1], d[2]]) } else { 0 };
            MyFail::Server(no, error_packet(d))
        };
        match d.first() {
            Some(0x00) => {
                let mut p = 1;
                let n = lenenc_int(&d, &mut p).flatten().unwrap_or(0);
                return Ok(MyResult { affected: n as i64, ..MyResult::default() });
            }
            Some(0xff) => return Err(server(&d)),
            Some(0xfb) => return Err(MyFail::Net("LOCAL INFILE is not supported".into())),
            _ => {}
        }
        let mut pos = 0;
        let n = lenenc_int(&d, &mut pos).flatten().ok_or_else(|| MyFail::Net("malformed packet".into()))? as usize;
        let mut r = MyResult { affected: -1, ..MyResult::default() };
        for _ in 0..n {
            let d = self.my.read_packet().map_err(net)?;
            if d.first() == Some(&0xff) {
                return Err(server(&d));
            }
            let mut p = 0;
            let mut name = Vec::new();
            for i in 0..5 {
                let s = lenenc_str(&d, &mut p).ok_or_else(|| MyFail::Net("malformed packet".into()))?;
                if i == 4 {
                    name = s.unwrap_or_default().to_vec();
                }
            }
            r.cols.push(lossy(&name));
        }
        let d = self.my.read_packet().map_err(net)?;
        if d.first() == Some(&0xff) {
            return Err(server(&d));
        }
        loop {
            let d = self.my.read_packet().map_err(net)?;
            if d.first() == Some(&0xfe) && d.len() < 9 {
                return Ok(r);
            }
            if d.first() == Some(&0xff) {
                return Err(server(&d));
            }
            let mut p = 0;
            let mut row = Vec::with_capacity(n);
            for _ in 0..n {
                match lenenc_str(&d, &mut p) {
                    None => return Err(MyFail::Net("EOF".into())),
                    Some(None) => row.push(None),
                    Some(Some(b)) => row.push(Some(lossy(b))),
                }
            }
            r.rows.push(row);
        }
    }

    pub fn close(&mut self) {
        self.my.seq = 0;
        let _ = self.my.write_packet(&[1]); // COM_QUIT
    }
}
