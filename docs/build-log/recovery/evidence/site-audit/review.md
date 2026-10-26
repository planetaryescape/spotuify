# Site dependency audit and narrow release-age exception

Captured 2026-10-26 against fresh `origin/main` SHA `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e`.

## Audit evidence

- `audit-before.json` and `audit-before.exit` are the raw npm audit result from the current base lock (`site/package.json` and `site/package-lock.json` copied from `origin/main`). The result is 12 vulnerabilities: 5 moderate, 7 high.
- `audit-after.json` and `audit-after.exit` are the raw audit result from the candidate lock. The result is zero vulnerabilities.
- `audit-level-check.log` and `audit-level-check.exit` record `npm --prefix site audit --omit=dev --audit-level=moderate`; exit status is 0.
- GitHub Actions run 39856945130 failed the same gate with 4 findings (3 moderate, 1 high): PostCSS selector parsing GHSA-rj75-hqrm-r3gf and sharp GHSA-wq5f-xc86-pv6w. The fresher base audit also identified newly published Astro, devalue, http-cache-semantics, smol-toml, source-map-js, undici, vite, and zod advisories; the compatible audit repair cleared all of them.

## Reviewed release

The exact upstream package is `@expressive-code/core@0.44.3`, in the `expressive-code/expressive-code` repository. `core-0.44.3-registry-review.json` preserves its npm metadata and integrity. Registry publication time is `2026-10-21T08:53:33.147Z`; SHA-512 integrity is `sha512-pffktF0TJoxC08dgNcutl/0LcSnTrjF6vXR39qVpwZ0EyxpwO9zjI7dbni6sO7w3yHpRBO0voLFvhzB+EPJRsA==`. Its metadata has build/test scripts but no `preinstall`, `install`, `postinstall`, or `prepare` lifecycle script.

`core-source-diff.jsonl` contains the package-scoped upstream comparison from tag commit `5c5b4522e0db357f453e2e6c9a46790e83ef6322` (`0.44.2`) to `5e795d7495348c041c474a6f60a7cbb63ee51adb` (`0.44.3`). The production package change is the dependency range `postcss-nested: ^6.0.1` to `^7.0.2`; the core implementation is unchanged. The release also updates its changelog and adds a plugin regression test.

GitHub Advisory Database sources:

- [GHSA-rj75-hqrm-r3gf](https://github.com/advisories/GHSA-rj75-hqrm-r3gf) marks every `postcss-selector-parser` version below `7.1.6` affected and `7.1.6` patched. npm registry metadata lists `6.1.5` as the newest 6.x release; it remains in the advisory's affected range. There is no supported fixed parser 6 release.
- [GHSA-wq5f-xc86-pv6w](https://github.com/advisories/GHSA-wq5f-xc86-pv6w) marks sharp below `0.35.5` affected and `0.35.5` patched.
- [GHSA-98hj-9722-w3j8](https://github.com/advisories/GHSA-98hj-9722-w3j8) and [GHSA-mpr7-6m5w-76rw](https://github.com/advisories/GHSA-mpr7-6m5w-76rw) require Astro `7.3.2` and `7.3.3`, respectively; the direct pin is now `7.3.3`.

npm registry publication timestamps in `postcss-nested-publication.json` and `postcss-selector-parser-publication.json` show `postcss-nested@7.0.2` was published `2024-11-01T16:15:46.284Z` and `postcss-selector-parser@7.1.6` was published `2026-09-03T08:59:01.963Z`; both are older than the seven-day quarantine. Their lockfile integrity values match the registry metadata.

## Release-age policy

The managed npm config remains at `min-release-age=7`. The reviewed exception is only this one update command:

```sh
npm --prefix site update @expressive-code/core --package-lock-only --ignore-scripts --min-release-age-exclude=@expressive-code/core
```

npm's `min-release-age-exclude` configuration exempts the named package only; its transitive dependencies remain subject to the seven-day policy. The package was reviewed before this exception. No npm config file was changed.

## Candidate changes and checks

- `site/package.json`: exact Astro pin `7.2.9` → `7.3.3`; direct sharp range already admits the fixed `0.35.5`, so it remains unchanged.
- `site/package-lock.json`: npm's compatible audit repair plus the reviewed core update; lock resolves `astro@7.3.3`, `sharp@0.35.5`, `@expressive-code/core@0.44.3`, `postcss-nested@7.0.2`, and `postcss-selector-parser@7.1.6` with registry-matching integrity.
- Standard `npm ci` stopped before installation because local strict lifecycle policy rejected `esbuild@0.28.1` and optional `fsevents@2.3.3`; no install scripts were approved. The lock includes platform prebuilts `@esbuild/darwin-arm64@0.28.1` and `@img/sharp-darwin-arm64@0.35.5`. A retry with `npm ci --ignore-scripts` is running as Pueue task 332; its result will be appended here.

The scripts-disabled install completed successfully as Pueue task 332: 363 packages added, 364 audited, zero vulnerabilities. It used only the lock-pinned `@expressive-code/core@0.44.3` exception during the earlier lock update; the install itself had no additional release-age exclusion. `site/node_modules` measures 231 MB.

The baseline site build is queued as Pueue task 334 through `agent-work`; it has not started because the shared queue is at its three-job parallel limit. Its result will be appended after completion.

The first build attempt (Pueue task 334) was canceled by its owner after I found the `agent-work` wrapper can set Rayon to five for child commands. It had generated partial `site/dist` output (38 MB) before cancellation; that generated output is retained. Replacement task 337 is queued with inner `RAYON_NUM_THREADS=4`, `GOMAXPROCS=4`, `CARGO_BUILD_JOBS=4`, `RUST_TEST_THREADS=4`, and `NEXTEST_TEST_THREADS=4`, after root's task 335. The source comparison file `core-source-diff.jsonl` is a valid JSON array.

The replacement build completed successfully as Pueue task 337 with inner worker limits set to four. Astro built 179 pages, Pagefind indexed 179 HTML files, and the build completed with exit status 0. Final measured paths are `site/node_modules` 229 MB and `site/dist` 41 MB. A post-build `npm --prefix site audit --omit=dev --audit-level=moderate` also exited 0 with zero vulnerabilities. Final tracked diff is confined to `site/package.json` and `site/package-lock.json`; site generation did not change other tracked files. Both generated directories are retained pending cleanup authorization.
