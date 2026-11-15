// hydo's stand-in for @playwright/test in the walker's folder (Milestone 258,
// docs/design/ui-exploration.md). The walker drives the browser with playwright-core alone; the
// watcher hook.cjs asks @playwright/test for the current test's title, and this module answers
// with the walk's step.
'use strict';
module.exports = { test: { info: () => ({ titlePath: [globalThis.__hydoWalkTest || 'walk'] }) } };
