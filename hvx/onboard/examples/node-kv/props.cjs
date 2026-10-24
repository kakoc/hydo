'use strict';
// The properties of nkv, declared once at load time (hydo-assert). Both the server
// (nkv-server.mjs, loaded through the SDK's ESM entry) and the client/oracle (nkv.cjs, through
// the CommonJS entry) load this module, so each process's catalog lists every property, and
// the ESM and CommonJS entries share one SDK state inside the server.
const ha = require('hydo-assert');

module.exports = {
  ha,
  ACKED_DURABLE: ha.always('nkv.acked_durable', 'every acknowledged PUT reads back with its value'),
  LOG_PARSES: ha.alwaysOrUnreachable('nkv.log_line_parses', 'every complete log line parses as PUT k v'),
  LOG_SIZE: ha.always('nkv.log_size', 'the log stays under 1 MiB'),
  OVERWRITE: ha.unreachable('nkv.put.overwrite', 'a PUT never changes the value of a key already stored'),
  RECOVERED: ha.sometimes('nkv.recovered', 'the server replayed a non-empty log at start'),
  TORN: ha.reachable('nkv.torn_tail', 'the log ended in a partial line (cut off by a crash)'),
  PUT_UNACKED: ha.sometimes('nkv.client.put_unacked', 'a PUT got no acknowledgement within the client deadline'),
  SERVES: ha.eventually('nkv.serves_after_start', 'the server answers within 5 s of the oracle start', 5000),
  LOST_REPLY: ha.buggify('nkv.put.lost_reply', 'the PUT is durable but the reply is lost'),
  SLOW_FSYNC: ha.buggify('nkv.put.slow_fsync', 'fdatasync takes 3 s'),
};
