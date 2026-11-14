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

/// A command or a schedule with a copy button.
pub fn copy_block(text: &str) -> String {
    format!(
        "<div class=\"cmdrow\"><pre class=\"cmd\">{}</pre><button type=\"button\" class=\"copy\" \
         data-copy=\"{}\" aria-label=\"Copy to the clipboard\">Copy</button></div>\n",
        esc(text),
        esc(text)
    )
}

/// Plain words with names between backquotes (`` `redis` ``), as hydo-schedule's and
/// hydo-explore's sentences write them: escaped, each quoted name as `<code>`.
pub fn md(s: &str) -> String {
    let mut o = String::new();
    for (i, part) in s.split('`').enumerate() {
        if i % 2 == 1 {
            o.push_str(&code(part));
        } else {
            o.push_str(&esc(part));
        }
    }
    o
}

/// Plain words without the backquotes (for a `<title>`).
pub fn plain(s: &str) -> String {
    s.replace('`', "")
}

/// A rounded label: `bad`, `warn`, `ok` or `note`.
pub fn pill(class: &str, words: &str) -> String {
    format!("<span class=\"pill {class}\">{}</span>", esc(words))
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

/// The style sheet: colour tokens for light and dark (the reader's system setting), one column,
/// readable on a phone.
pub const CSS: &str = r#":root{--bg:#fbfbfa;--fg:#1d1d1f;--muted:#5f5f66;--card:#fff;--line:#e5e5e2;--code:#f3f3f1;
--bad:#b3261e;--bad-bg:#fdecea;--warn:#8f5200;--warn-bg:#fff4e0;--ok:#1e7a3c;--ok-bg:#e8f6ec;
--note:#2457c5;--note-bg:#e8f0fb;--add:#e8f6ec;--del:#fdecea;color-scheme:light dark}
@media (prefers-color-scheme:dark){:root:not([data-theme="light"]){--bg:#141416;--fg:#ececee;--muted:#a0a0a8;
--card:#1d1d20;--line:#2e2e33;--code:#26262a;--bad:#ff8a7a;--bad-bg:#3a1d1a;--warn:#ffb648;--warn-bg:#33270f;
--ok:#5fd38a;--ok-bg:#13301e;--note:#8ab0ff;--note-bg:#1a2a42;--add:#13301e;--del:#3a1d1a}}
:root[data-theme="dark"]{--bg:#141416;--fg:#ececee;--muted:#a0a0a8;--card:#1d1d20;--line:#2e2e33;--code:#26262a;
--bad:#ff8a7a;--bad-bg:#3a1d1a;--warn:#ffb648;--warn-bg:#33270f;--ok:#5fd38a;--ok-bg:#13301e;--note:#8ab0ff;
--note-bg:#1a2a42;--add:#13301e;--del:#3a1d1a}
*{box-sizing:border-box}
html{-webkit-text-size-adjust:100%}
body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.55 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,"Helvetica Neue",Arial,sans-serif}
main{max-width:760px;margin:0 auto;padding:32px 16px 72px}
.eyebrow{color:var(--muted);font-size:14px;margin:0 0 6px}
h1{font-size:28px;line-height:1.25;margin:0 0 10px;overflow-wrap:anywhere}
h2{font-size:19px;margin:34px 0 10px}
h3{font-size:17px;line-height:1.35;margin:0 0 6px;overflow-wrap:anywhere}
h4{font-size:15px;margin:16px 0 6px}
p{margin:8px 0}
.lead{font-size:18px}
.muted{color:var(--muted)}
.small{font-size:14px;color:var(--muted)}
code,pre,.mono{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,"Liberation Mono",monospace;font-size:14px}
code{background:var(--code);padding:1px 5px;border-radius:5px;overflow-wrap:anywhere}
code.fp{white-space:nowrap;overflow-wrap:normal}
pre{background:var(--code);padding:8px 10px;border-radius:8px;margin:6px 0;white-space:pre-wrap;overflow-wrap:anywhere}
.card{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:16px 20px;margin:14px 0}
.pill{display:inline-block;font-size:13px;font-weight:600;padding:2px 10px;border-radius:999px;margin-right:6px;white-space:nowrap}
.badge{display:inline-block;padding:1px 9px;border-radius:999px;font-weight:600;font-size:13px;white-space:nowrap}
.bad{color:var(--bad);background:var(--bad-bg)}.ok{color:var(--ok);background:var(--ok-bg)}
.warn{color:var(--warn);background:var(--warn-bg)}.note{color:var(--note);background:var(--note-bg)}
.verdictline{display:flex;gap:10px;align-items:center;margin:14px 0 4px;flex-wrap:wrap}
ol.steps{margin:4px 0 0;padding-left:22px}ol.steps li{margin:6px 0}
.evidence{border-left:3px solid var(--bad)}
.evidence div{margin:5px 0;padding-left:22px;text-indent:-22px}
.evidence .mark{display:inline-block;width:22px;text-indent:0;font-weight:700}
.evidence .m-ok{color:var(--ok)}.evidence .m-bad{color:var(--bad)}.evidence .m-warn{color:var(--warn)}.evidence .m-note{color:var(--muted)}
.cmdrow{display:flex;gap:8px;align-items:stretch;margin:8px 0}
.cmd{flex:1;min-width:0;margin:0;background:var(--code);border-radius:8px;padding:10px 12px;white-space:pre-wrap;overflow-wrap:anywhere}
button.copy{font:inherit;font-size:14px;border:1px solid var(--line);background:var(--card);color:var(--fg);border-radius:8px;padding:0 14px;cursor:pointer}
button.copy:hover{border-color:var(--muted)}
.problem{display:grid;grid-template-columns:30px minmax(0,1fr);gap:4px 12px}
.num{font-size:22px;font-weight:700;color:var(--muted);line-height:1.2}
.bar{height:8px;border-radius:4px;background:var(--line);overflow:hidden;margin:6px 0 2px}.bar>span{display:block;height:100%}
.bar.look>span{background:var(--bad)}.bar.setting>span{background:var(--warn)}.bar.expected>span{background:var(--ok)}
dl.facts{display:grid;grid-template-columns:max-content minmax(0,1fr);gap:6px 16px;margin:8px 0}
dl.facts dt{color:var(--muted)}dl.facts dd{margin:0;overflow-wrap:anywhere}
details.share{margin-top:40px;border-top:1px solid var(--line);padding-top:14px}
details.share>summary{cursor:pointer;color:var(--muted);font-weight:600}
details.share .card,details.share table{font-size:14px}
details.share h3{margin-top:22px}
details{margin:8px 0}details>summary{cursor:pointer;font-weight:600}
.scroll{overflow-x:auto;margin:6px 0}
table{border-collapse:collapse;width:100%;font-size:14px}
th,td{text-align:left;padding:4px 8px;border-bottom:1px solid var(--line);vertical-align:top}
th{color:var(--muted);font-weight:600;white-space:nowrap}
td.t,td .t{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:12.5px;overflow-wrap:anywhere}
td.n{white-space:nowrap;font-variant-numeric:tabular-nums;color:var(--muted)}
tr.conv td{background:var(--code);font-weight:600;color:var(--fg)}
tr.del td{background:var(--del)}tr.add td{background:var(--add)}
tr.same td{color:var(--muted)}
.kind{display:inline-block;font-size:12px;padding:0 6px;border-radius:4px;white-space:nowrap;background:var(--code)}
.tl label{cursor:pointer;color:var(--muted);font-size:14px}
.tl input.all:not(:checked)~.scroll tr.minor{display:none}
footer{margin-top:28px;font-size:13px;color:var(--muted)}
a{color:var(--note)}
@media (max-width:640px){h1{font-size:24px}.lead{font-size:17px}dl.facts{grid-template-columns:minmax(0,1fr)}dl.facts dd{margin-bottom:8px}
.c-clock{display:none}.cmdrow{flex-direction:column}.cmdrow button{align-self:flex-end;padding:6px 14px}
.problem{grid-template-columns:minmax(0,1fr)}.num{font-size:17px}main{padding:20px 16px 48px}}
"#;

/// The script: the copy buttons (the clipboard API where the browser allows it on a file, else a
/// hidden text area), and a link into the folded details opens them at its target.
pub const JS: &str = r##"document.addEventListener('click',function(e){
var b=e.target.closest&&e.target.closest('button.copy');if(!b)return;
var t=b.getAttribute('data-copy');
function done(){b.textContent='Copied';setTimeout(function(){b.textContent='Copy'},1500)}
function old(){var a=document.createElement('textarea');a.value=t;a.setAttribute('readonly','');
a.style.position='fixed';a.style.opacity='0';document.body.appendChild(a);a.select();
try{document.execCommand('copy');done()}catch(x){}document.body.removeChild(a)}
if(navigator.clipboard&&window.isSecureContext){navigator.clipboard.writeText(t).then(done,old)}else{old()}});
function openTo(){var h=decodeURIComponent(location.hash.slice(1));if(!h)return;
var el=document.getElementById(h);if(!el)return;
for(var p=el;p;p=p.parentElement){if(p.tagName==='DETAILS')p.open=true}el.scrollIntoView()}
document.addEventListener('click',function(e){var a=e.target.closest&&e.target.closest('a[href^="#"]');
if(a)setTimeout(openTo,0)});
window.addEventListener('hashchange',openTo);openTo();
"##;
