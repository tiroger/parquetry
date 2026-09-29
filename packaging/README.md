# Packaging & distribution

Everything needed to turn the `parquetry` binary into a signed, notarized
`Parquetry.app`, a `.zip`/`.dmg`, and a Homebrew cask.

| Path | What |
|---|---|
| `assets/icon/make_icon.py` | Renders `icon_1024.png` and `AppIcon.icns` (`uv run --with pillow python assets/icon/make_icon.py`) |
| `packaging/Info.plist.template` | App Info.plist (`@VERSION@`, `@BUILD@`, `@YEAR@`), document types, UTIs, `parquetry://` scheme |
| `packaging/Parquetry.entitlements` | Hardened-runtime entitlements for the app |
| `packaging/bin/parquetry` | CLI launcher, shipped as `Contents/Resources/bin/parquetry` |
| `packaging/homebrew/parquetry.rb` | Cask template, rendered by `scripts/update-cask.sh` |
| `scripts/bundle.sh` | Build + assemble + sign `target/dist/Parquetry.app` |
| `scripts/package.sh` | `target/dist/Parquetry-<v>.zip` and `.dmg` and `SHA256SUMS` |
| `scripts/notarize.sh` | Notarize and staple the app and dmg |
| `scripts/update-cask.sh` | Render the cask with the real version and sha256 |
| `.github/workflows/ci.yml` / `release.yml` | CI, and a tag-triggered release pipeline |

## Local builds

```sh
scripts/bundle.sh                         # release build, ad-hoc signed
SKIP_BUILD=1 PROFILE=debug scripts/bundle.sh   # reuse target/debug/parquetry
UNIVERSAL=1 scripts/bundle.sh             # arm64 + x86_64 (rustup target add x86_64-apple-darwin)
scripts/package.sh                        # zip + dmg + SHA256SUMS
```

`bundle.sh` options: `PROFILE`, `UNIVERSAL=1`, `SKIP_BUILD=1`,
`SKIP_QUICKLOOK=1`, `CODESIGN_IDENTITY` (default `-`, ad-hoc),
`DUCKDB_EXT_LAYOUT` (`repo` default, `raw`, `none`), `DUCKDB_EXTENSIONS`
(default `httpfs`), `DUCKDB_VERSION` (default: from `libduckdb-sys` in
`Cargo.lock`), `BUILD_NUMBER`. The Quick Look extension is built with
`scripts/build-quicklook.sh` and embedded at `Contents/PlugIns/` when it builds;
if it doesn't, the bundle is made without it and a warning is printed.

Bundle layout:

```
Parquetry.app/Contents/
  Info.plist  PkgInfo
  MacOS/parquetry
  PlugIns/ParquetryQuickLook.appex
  Resources/AppIcon.icns
  Resources/bin/parquetry                                       CLI launcher
  Resources/duckdb_extensions/VERSION                           e.g. v1.5.5
  Resources/duckdb_extensions/osx_arm64/httpfs.duckdb_extension          (raw layout)
  Resources/duckdb_extensions/v1.5.5/osx_arm64/httpfs.duckdb_extension.gz (repo layout)
```

Downloads are cached in `target/duckdb_extensions_cache/`.

### Testing an ad-hoc build

Ad-hoc builds are **for this machine only**. They can't be notarized, and
Gatekeeper blocks them once they carry a quarantine flag (e.g. after being
downloaded or AirDropped). To test a copy you moved around:

```sh
xattr -dr com.apple.quarantine /Applications/Parquetry.app
```

## One-time setup for signed releases

1. **Developer ID certificate.** In the Apple Developer portal (or Xcode →
   Settings → Accounts → Manage Certificates), create a *Developer ID
   Application* certificate and install it in your login keychain. Check it:
   `security find-identity -v -p codesigning` should list
   `Developer ID Application: Your Name (TEAMID)`.
2. **Notary credentials.** Create an app-specific password at
   <https://account.apple.com> → Sign-In and Security, then store it once:
   ```sh
   xcrun notarytool store-credentials parquetry-notary \
     --apple-id you@example.com --team-id TEAMID --password abcd-efgh-ijkl-mnop
   ```
   `notarytool` and `stapler` ship with the Command Line Tools. If `xcrun`
   can't find them and you use Xcode.app as developer dir, run
   `sudo xcodebuild -license accept` and
   `sudo xcode-select -s /Applications/Xcode.app/Contents/Developer`.
3. **Homebrew tap.** Create a public GitHub repo `OWNER/homebrew-tap` with a
   `Casks/` directory. Homebrew maps `OWNER/tap` to that repo.
4. **GitHub secrets** (in the `parquetry` repo):

   | Secret | Value |
   |---|---|
   | `MACOS_CERTIFICATE_P12` | `base64 -i DeveloperID.p12 \| pbcopy` (export cert + private key from Keychain Access) |
   | `MACOS_CERTIFICATE_PASSWORD` | password of that .p12 |
   | `APPLE_ID` | Apple ID email |
   | `APPLE_TEAM_ID` | 10-character team ID |
   | `APPLE_APP_PASSWORD` | the app-specific password |
   | `TAP_GITHUB_TOKEN` | fine-grained PAT with *Contents: read & write* on `OWNER/homebrew-tap` |

## Signed local release

```sh
export CODESIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)"
UNIVERSAL=1 DUCKDB_EXT_LAYOUT=repo scripts/bundle.sh
scripts/package.sh
NOTARY_PROFILE=parquetry-notary scripts/notarize.sh      # or APPLE_ID/APPLE_TEAM_ID/APPLE_APP_PASSWORD
scripts/update-cask.sh 0.1.0 target/dist/Parquetry-0.1.0.zip   # -> target/dist/parquetry.rb
```

`notarize.sh` submits the zip, staples the app, rebuilds the zip and dmg
around the stapled app, then submits and staples the dmg too. Rejected
submissions print the notary log.

## CI release

Push a tag matching the workspace version: `git tag v0.1.0 && git push origin v0.1.0`.
`release.yml` checks the tag against `Cargo.toml`, imports the certificate into a
temporary keychain, runs `UNIVERSAL=1 scripts/bundle.sh`, `package.sh` and
`notarize.sh`, uploads the zip, dmg and `SHA256SUMS` to the GitHub release, and
commits the rendered cask to `OWNER/homebrew-tap`. It skips that last step if
`TAP_GITHUB_TOKEN` isn't set.

`ci.yml` runs `cargo test --workspace` and
`cargo clippy --workspace --all-targets -- -D warnings`. Both are strict: a
warning fails CI. It also lints the scripts, plists and cask, and smoke-tests
the CLI launcher.

## Installing (users)

```sh
brew install --cask OWNER/tap/parquetry
# or
brew tap OWNER/tap && brew install --cask parquetry
```

The cask links `parquetry` into Homebrew's `bin`:

```sh
parquetry data.parquet ./partitioned_dir 'logs/*.parquet' s3://bucket/key.parquet
```

Local files and folders are opened with `open -a Parquetry.app <abs path>`.
URLs (`s3://`, `gs://`, `http(s)://`, …) and quoted globs are sent as
`parquetry://open?url=<percent-encoded>`.

## Gatekeeper and notarization

Homebrew doesn't strip the quarantine flag, so the downloaded app has to pass
Gatekeeper. Distributed builds therefore **must** be Developer ID-signed with
the hardened runtime, timestamped, notarized and stapled. Casks with
unnotarized apps also fail `brew audit`. Ad-hoc builds (`CODESIGN_IDENTITY=-`,
the default) are only for local testing.

## Design notes

**Entitlements.** The app is not sandboxed: it opens arbitrary paths, folders
and globs, and reads `~/.aws`. The only entitlement is
`com.apple.security.cs.disable-library-validation`, because DuckDB `dlopen`s
extension files that aren't signed by our team. `com.apple.security.network.client`
only matters inside the App Sandbox, so it is omitted. Outgoing network access
is unrestricted for non-sandboxed apps. The Quick Look extension is sandboxed,
as app extensions must be, and uses `quicklook/QuickLook.entitlements`.

**DuckDB extensions can't be codesigned.** DuckDB appends its own RSA signature
to each `.duckdb_extension`. `codesign` refuses these files ("main executable
failed strict validation"), and any byte change would break DuckDB's signature
check anyway. So `bundle.sh` doesn't sign them. They're sealed as resources by
the app's signature, and `codesign --verify --deep --strict` passes. For
notarization, though, unsigned Mach-O files anywhere in the bundle are expected
to be rejected. That's why there are two layouts:

* `repo` (the default, also used by `release.yml`): gzipped files in DuckDB's own repository
  layout, `Resources/duckdb_extensions/<ver>/<platform>/httpfs.duckdb_extension.gz`.
  The app installs from it offline with
  `INSTALL httpfs FROM '<Resources>/duckdb_extensions'; LOAD httpfs;`, which
  copies the file into `extension_directory`. It's verified to work with DuckDB 1.5.5.
  There's no raw Mach-O in the bundle for the notary service to reject.
* `raw`: `Resources/duckdb_extensions/<platform>/httpfs.duckdb_extension`,
  loaded with `LOAD '<path>'`. Fine for local builds; not notarizable.

The engine tries `raw` first, then `repo` (`crates/engine/src/engine.rs`,
`ensure_extension`).

If the app doesn't handle the layout it's given, it falls back to downloading
httpfs on first use.
