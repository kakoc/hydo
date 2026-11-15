#!/bin/sh
# Build the inputs of the web-shop example (Milestone 253) on a machine with Node.js and network:
# both builds of the app (app/dist-fixed, app/dist-buggy) and the tests' node_modules, from the
# two package-lock.json files. hydo build then copies them into the image; the guest has no
# network. The Playwright image itself is fetched by `hydo build` into browser/.
set -eu
cd "$(dirname "$0")"
reg=https://registry.npmjs.org/
(cd app && npm ci --registry "$reg" && npx vite build --outDir dist-fixed &&
  VITE_VARIANT=buggy npx vite build --outDir dist-buggy)
(cd e2e && npm ci --omit=optional --registry "$reg")
ls -d app/dist-fixed app/dist-buggy e2e/node_modules/@playwright/test
