# sourced by the commands (helper_* files are not commands). /etc/hydo/prelude.sh holds the
# history recorder hydo_hist (every manifest with history checks has it).
. /etc/hydo/prelude.sh
R() { timeout 2 /bin/redis-cli "$@" 2>/dev/null; }   # a 2 s client deadline
list_tok() { local l; l=$(tr '\n' ',' | sed 's/,$//'); echo "[$l]"; }   # LRANGE lines -> [e1,e2,...]
