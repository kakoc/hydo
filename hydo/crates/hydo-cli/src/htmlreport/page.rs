//! The pieces of the page `hydo report --html` writes: escaping, the style sheet, the one small
//! script (the copy buttons), and the blocks every section is made of. The page loads nothing:
//! no script, style sheet, font or image comes from anywhere but the file itself, so it works
//! opened from disk, attached to a CI job or an issue, and without a network.

/// Text for HTML: `&`, `<`, `>` and both quotes escaped (also right for attribute values).
pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

/// At most `n` characters of `s`, with `…` when cut.
pub fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// At most `max` bytes of `s` (cut at a line end when there is one), and whether it was cut.
pub fn cut_bytes(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    let head = &s[..end];
    let head = head.rfind('\n').map(|i| &head[..i]).unwrap_or(head);
    (head.to_string(), true)
}

/// `<code>` text.
pub fn code(s: &str) -> String {
    format!("<code>{}</code>", esc(s))
}

/// A block of monospace text with a copy button (a schedule, a command line).
pub fn copy_block(text: &str) -> String {
    format!(
        "<div class=\"cmd\"><pre>{}</pre><button type=\"button\" class=\"copy\" data-copy=\"{}\" \
         aria-label=\"Copy to the clipboard\">Copy</button></div>\n",
        esc(text),
        esc(text)
    )
}

/// A list of facts: each label with its value (HTML).
pub fn facts(rows: &[(String, String)]) -> String {
    let mut s = String::from("<dl class=\"facts\">\n");
    for (k, v) in rows {
        s.push_str(&format!("<dt>{}</dt><dd>{v}</dd>\n", esc(k)));
    }
    s.push_str("</dl>\n");
    s
}

/// A coloured word: `bad`, `ok`, `warn` or `note`.
pub fn badge(class: &str, words: &str) -> String {
    format!("<span class=\"badge {class}\">{}</span>", esc(words))
}

/// The colour, the badge's word and a sentence for a verdict kind (`ok`, `violation`, `error`).
pub fn verdict_words(kind: &str) -> (&'static str, &'static str, &'static str) {
    match kind {
        "ok" => ("ok", "passed", "every check held"),
        "violation" => ("bad", "failed", "a check did not hold"),
        "error" => ("warn", "error", "the run did not get to its checks"),
        _ => ("warn", "no verdict", "the run has no verdict"),
    }
}

/// A section with its heading.
pub fn section(id: &str, title: &str, body: &str) -> String {
    format!(
        "<section id=\"{id}\">\n<h2>{}</h2>\n{body}</section>\n",
        esc(title)
    )
}

/// A table: the header cells, then rows of HTML cells, each row with its class.
pub fn table(head: &[&str], rows: &[(String, Vec<String>)]) -> String {
    let mut s = String::from("<div class=\"scroll\"><table>\n<thead><tr>");
    for h in head {
        s.push_str(&format!("<th>{}</th>", esc(h)));
    }
    s.push_str("</tr></thead>\n<tbody>\n");
    for (class, cells) in rows {
        if class.is_empty() {
            s.push_str("<tr>");
        } else {
            s.push_str(&format!("<tr class=\"{class}\">"));
        }
        for c in cells {
            s.push_str(&format!("<td>{c}</td>"));
        }
        s.push_str("</tr>\n");
    }
    s.push_str("</tbody></table></div>\n");
    s
}

/// The whole page.
pub fn document(title: &str, body: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta name=\"generator\" content=\"hydo report --html\">\n\
         <title>{}</title>\n<style>\n{CSS}</style>\n</head>\n<body>\n<main>\n{body}</main>\n\
         <script>\n{JS}</script>\n</body>\n</html>\n",
        esc(title)
    )
}

/// The style sheet: colour tokens for light and dark (the reader's system setting), one column on
/// a phone.
pub const CSS: &str = r#":root{--bg:#fbfbfa;--fg:#1d1f23;--muted:#5b616b;--line:#e1e3e7;--card:#fff;--code:#f2f3f5;
--bad:#a8261b;--bad-bg:#fdeceb;--ok:#17703a;--ok-bg:#e6f4ea;--warn:#8a5a00;--warn-bg:#fdf3d8;
--note:#2c5b9a;--note-bg:#e8f0fb;--add:#e6f4ea;--del:#fdeceb;color-scheme:light dark}
@media (prefers-color-scheme:dark){:root{--bg:#131518;--fg:#e4e6e9;--muted:#9aa1ab;--line:#2c3037;
--card:#1b1e22;--code:#23272d;--bad:#ff8a80;--bad-bg:#3b1f1d;--ok:#6fd38b;--ok-bg:#17301f;
--warn:#e8bb4a;--warn-bg:#352a12;--note:#8ab8ff;--note-bg:#1a2a42;--add:#17301f;--del:#3b1f1d}}
*{box-sizing:border-box}
html{-webkit-text-size-adjust:100%}
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,"Helvetica Neue",Arial,sans-serif}
main{max-width:1080px;margin:0 auto;padding:16px 16px 48px}
h1{font-size:1.45rem;line-height:1.3;margin:.4rem 0 .6rem}
h2{font-size:1.15rem;margin:2rem 0 .6rem;padding-bottom:.3rem;border-bottom:1px solid var(--line)}
h3{font-size:1rem;margin:.2rem 0 .6rem}
h4{font-size:.95rem;margin:1rem 0 .4rem}
p{margin:.4rem 0}
code,pre,.mono{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,"Liberation Mono",monospace;font-size:13px}
code{background:var(--code);padding:1px 4px;border-radius:4px;overflow-wrap:anywhere}
code.fp{white-space:nowrap;overflow-wrap:normal}
pre{background:var(--code);padding:8px 10px;border-radius:6px;margin:.3rem 0;white-space:pre-wrap;overflow-wrap:anywhere}
.lead{font-size:1.02rem}
.muted{color:var(--muted)}
.card{background:var(--card);border:1px solid var(--line);border-radius:8px;padding:12px 16px;margin:14px 0}
.badge{display:inline-block;padding:1px 10px;border-radius:999px;font-weight:600;font-size:.88rem;white-space:nowrap}
.bad{color:var(--bad);background:var(--bad-bg)}.ok{color:var(--ok);background:var(--ok-bg)}
.warn{color:var(--warn);background:var(--warn-bg)}.note{color:var(--note);background:var(--note-bg)}
dl.facts{display:grid;grid-template-columns:max-content minmax(0,1fr);gap:4px 18px;margin:.4rem 0}
dl.facts dt{color:var(--muted)}dl.facts dd{margin:0;overflow-wrap:anywhere}
.cmd{display:flex;gap:8px;align-items:flex-start;margin:.3rem 0}.cmd pre{flex:1;min-width:0;margin:0}
button.copy{font:inherit;font-size:.85rem;padding:6px 12px;border:1px solid var(--line);border-radius:6px;background:var(--card);color:var(--fg);cursor:pointer}
button.copy:hover{border-color:var(--muted)}
.scroll{overflow-x:auto;margin:.3rem 0}
table{border-collapse:collapse;width:100%;font-size:14px}
th,td{text-align:left;padding:4px 8px;border-bottom:1px solid var(--line);vertical-align:top}
th{color:var(--muted);font-weight:600;white-space:nowrap}
td.t,td .t{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:12.5px;overflow-wrap:anywhere}
td.n{white-space:nowrap;font-variant-numeric:tabular-nums;color:var(--muted)}
tr.conv td{background:var(--code);font-weight:600;color:var(--fg)}
tr.del td{background:var(--del)}tr.add td{background:var(--add)}
tr.same td{color:var(--muted)}
.kind{display:inline-block;font-size:.8rem;padding:0 6px;border-radius:4px;white-space:nowrap;background:var(--code)}
.tl label{cursor:pointer;color:var(--muted);font-size:.9rem}
.tl input.all:not(:checked)~.scroll tr.minor{display:none}
details{margin:.5rem 0}details>summary{cursor:pointer;font-weight:600}
nav.toc{display:flex;flex-wrap:wrap;gap:2px 16px;font-size:.92rem;margin:.4rem 0 0}nav.toc a{color:var(--note);text-decoration:none;white-space:nowrap}
footer{margin-top:2.5rem;font-size:.85rem;color:var(--muted)}
@media (max-width:640px){dl.facts{grid-template-columns:minmax(0,1fr)}dl.facts dd{margin-bottom:8px}
.c-clock{display:none}.cmd{flex-direction:column}.cmd pre{width:100%}.cmd button{align-self:flex-end}main{padding:12px 16px 40px}}
"#;

/// The script: the copy buttons (the clipboard API where the browser allows it on a file, else a
/// hidden text area).
pub const JS: &str = r#"document.addEventListener('click',function(e){
var b=e.target.closest&&e.target.closest('button.copy');if(!b)return;
var t=b.getAttribute('data-copy');
function done(){b.textContent='Copied';setTimeout(function(){b.textContent='Copy'},1500)}
function old(){var a=document.createElement('textarea');a.value=t;a.setAttribute('readonly','');
a.style.position='fixed';a.style.opacity='0';document.body.appendChild(a);a.select();
try{document.execCommand('copy');done()}catch(x){}document.body.removeChild(a)}
if(navigator.clipboard&&window.isSecureContext){navigator.clipboard.writeText(t).then(done,old)}else{old()}});
"#;
