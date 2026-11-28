#!/bin/sh
# Build the inputs of the web-shop example (Milestone 253) on a machine with Node.js and network:
# both builds of the app (app/dist-fixed, app/dist-buggy), the two builds with the search box of
# Milestone 393 (app/dist-fixed-search, app/dist-buggy-search, for hydo-search.yaml) and the
# tests' node_modules, from the two package-lock.json files. The search box is only in the
# builds with VITE_SEARCH=1; the other two have the bytes they had before it. hydo build then copies them into the image; the guest has no
# network. The Playwright image itself is fetched by `hydo build` into browser/.
set -eu
cd "$(dirname "$0")"
reg=https://registry.npmjs.org/
(cd app && npm ci --registry "$reg" && npx vite build --outDir dist-fixed &&
  VITE_VARIANT=buggy npx vite build --outDir dist-buggy &&
  VITE_SEARCH=1 npx vite build --outDir dist-fixed-search &&
  VITE_SEARCH=1 VITE_VARIANT=buggy npx vite build --outDir dist-buggy-search)
(cd e2e && npm ci --omit=optional --registry "$reg")
ls -d app/dist-fixed app/dist-buggy app/dist-fixed-search app/dist-buggy-search e2e/node_modules/@playwright/test
