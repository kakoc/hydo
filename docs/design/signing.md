# Signed, notarized and reproducible Mac programs (Milestone 172)

The Mac release's four programs (`bin/hydo`, `bin/hydo-build`, `bin/hydo-fetch-image` and the
engine `bin/hydo-hvm`) are signed with the project's Developer ID and notarized by Apple, and the
published tarball is still reproducible by anyone from the source. This document says how; the
steps are in docs/RELEASING.md, the code in `hydo/dist/mac/` and
`hydo/crates/hydo-release/src/signatures.rs`.

## The problem

`hydo/dist/mac/build-release.sh` builds the tarball reproducibly: two builds from the same commit
give the same bytes (Milestones 150, 153–158). A Developer ID signature cannot be part of that:
only the key's owner can make it, and every signing gives other bytes (the secure timestamp from
Apple's time-stamp service, and the CMS signature over it). Until 0.2.0 the release shipped the
programs with the linker's ad-hoc signature, and `hydo setup` signed `bin/hydo-hvm` ad hoc on the
user's Mac with the hypervisor entitlement.

## The design

1. **Build unsigned, reproducibly, as before.** `build-release.sh` writes the unsigned tarball
   (the programs carry the linker's ad-hoc signature) and the sources archive. Its packing step is
   `pack-release.sh` (`SHA256SUMS`, no extended attributes, every time 0, a sorted GNU tar without
   owners, `gzip -n`), so the same step can pack a tree again.
2. **Sign once** (`sign-release.sh`, run by the key's owner). Every Mach-O program in `bin/` is
   signed with `codesign --force --options runtime --timestamp -s "Developer ID Application:
   Konstantin Matsiushonak (5C3JA2N5X6)"`: the hardened runtime, a secure timestamp, and for
   `bin/hydo-hvm` the entitlement `com.apple.security.hypervisor` from `hv/hvm/hv.entitlements`
   (the only entitlement; no other exception of the hardened runtime is needed). The identity
   string and the team ID are public: every signature carries them.
3. **Notarize** the four signed programs together: one zip (`ditto -c -k`) submitted with
   `xcrun notarytool submit --keychain-profile hydo-notary --wait`; the submission must be
   `Accepted`, and its log is kept. A ticket cannot be stapled to a plain command-line program
   (`stapler` staples apps, disk images and installer packages only); Gatekeeper finds the ticket
   online by the program's code-directory hash, which is inside the signature. That hash is the
   same in every copy the signature is attached to, so every reproduced program is notarized too.
4. **Take the signatures out.** A signed Mach-O program differs from the unsigned one in exactly
   four places: the signature blob at the end of the file (the code directory with every page's
   hash, the requirements, the entitlements, the CMS signature with the certificate chain and the
   timestamp), which replaces the linker's at the same offset, and three numbers in the header
   (the blob's size in `LC_CODE_SIGNATURE`, the file size of `__LINKEDIT` and its memory size,
   rounded up to 16 KiB). `hydo-release signatures extract` writes each program's blob as
   `bin/<program>.sig`, exactly as codesign wrote it (the data `LC_CODE_SIGNATURE` names), after
   checking that attaching it to the unsigned program gives the signed program byte for byte.
   The blobs, a `SIGNATURES` list (for each program the unsigned sha256, the signed sha256 and the
   path) and a README form the release asset **`hydo-mac-<version>-signatures.tar.gz`** (fixed
   times, owners and order; `gzip` without a time).
5. **Attach** (`attach-signatures.sh`, anyone): unpack the unsigned tarball, `hydo-release
   signatures attach` (each program must have the unsigned sha256, and the result must have the
   signed sha256: the code directory hashes the header with the new numbers, so the result
   verifies exactly as the signed program does), then `pack-release.sh` again. The result is the
   published `hydo-mac-<version>.tar.gz`. `sign-release.sh` makes the signed tarball this same
   way, so the published file is, by construction, what anyone gets from the source and the
   signatures asset.

What a user who checks the release does:

```
git checkout v<version>
sh hydo/dist/mac/build-release.sh out            # the unsigned tarball (as built from the tag)
sh hydo/dist/mac/attach-signatures.sh out/hydo-mac-<version>.tar.gz hydo-mac-<version>-signatures.tar.gz signed
shasum -a 256 signed/hydo-mac-<version>.tar.gz   # the sha256 of the published tarball
```

## What `hydo setup` does with a signed release

`hydo setup` checks `bin/hydo-hvm` against `SHA256SUMS` as every file; when it already has a valid
signature with the hypervisor entitlement (`codesign --verify --strict`, and the entitlement in
`codesign -d --entitlements -`), setup takes no step for it. An engine without the entitlement
(an unsigned build of the release scripts, a cargo build) is signed ad hoc as before, and setup
records the bytes it signed it to. `hydo doctor` prints who signed the engine (the Developer ID
and its team, or "ad hoc").

## What was checked (Milestone 172)

The proof that the signed tarball reproduces from the published signatures, the notarization
results and the checks of the installed programs are in docs/HISTORY.md "Milestone 172".

## Limits

- The signatures asset belongs to one unsigned build: a program built from another commit, or at
  another tree path (the Mach-O `LC_UUID` depends on it; docs/RELEASING.md step 2), has other
  bytes, and `attach` refuses it by sha256.
- A ticket that Apple revokes (a key reported as compromised) makes Gatekeeper refuse the
  programs; a new release with a new signature is then the remedy.
- Only the Mac release is signed: Linux has no comparable code-signing step for programs.
