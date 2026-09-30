# Packaging & distribution

Everything needed to turn the `parquetry` binary into a signed, notarized
`Parquetry.app`, a `.zip`/`.dmg`, and a Homebrew cask.

| Path | What |
|---|---|
| `assets/icon/make_icon.py` | Renders `icon_1024.png` and `AppIcon.icns` (`uv run --with pillow python assets/icon/make_icon.py`) |
| `packaging/Info.plist.template` | App Info.plist (`@VERSION@`, `@BUILD@`, `@YEAR@`, `@SPARKLE_FEED_URL@`, `@SPARKLE_PUBLIC_KEY@`), document types, UTIs, `parquetry://` scheme |
| `packaging/sparkle_public_key.txt` | EdDSA public key that update archives must be signed with |
| `packaging/Parquetry.entitlements` | Hardened-runtime entitlements for the app |
| `packaging/bin/parquetry` | CLI launcher, shipped as `Contents/Resources/bin/parquetry` |
| `packaging/homebrew/parquetry.rb` | Cask template, rendered by `scripts/update-cask.sh` |
| `scripts/bundle.sh` | Build + assemble + sign `target/dist/Parquetry.app` |
| `scripts/package.sh` | `target/dist/Parquetry-<v>.zip` and `.dmg` and `SHA256SUMS` |
| `scripts/notarize.sh` | Notarize and staple the app and dmg |
| `scripts/appcast.sh` | Sign the zip and write the Sparkle update feed `target/dist/appcast.xml` |
| `scripts/update-cask.sh` | Render the cask with the real version and sha256 |
| `packaging/scoop/parquetry.json` | Scoop manifest template, rendered by `scripts/update-scoop.sh` |
| `scripts/update-scoop.sh` | Render the Scoop manifest with the real version and sha256 |
| `.cargo/config.toml` | Windows builds link the C runtime statically (no VC++ redistributable needed) |
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
`DUCKDB_EXT_LAYOUT` (`none` default, `repo`, `raw`), `DUCKDB_EXTENSIONS`
(default `httpfs`), `DUCKDB_VERSION` (default: from `libduckdb-sys` in
`Cargo.lock`), `BUILD_NUMBER` (default: git commit count; Sparkle compares it
to decide what's newer), `SKIP_SPARKLE=1`, `SPARKLE_VERSION` (default 2.10.0),
`SPARKLE_FEED_URL`, `SPARKLE_PUBLIC_KEY`. The Quick Look extension is built with
`scripts/build-quicklook.sh` and embedded at `Contents/PlugIns/` when it builds;
if it doesn't, the bundle is made without it and a warning is printed.

Bundle layout:

```
Parquetry.app/Contents/
  Info.plist  PkgInfo
  MacOS/parquetry
  Frameworks/Sparkle.framework                                  automatic updates
  PlugIns/ParquetryQuickLook.appex
  Resources/AppIcon.icns
  Resources/bin/parquetry                                       CLI launcher
  Resources/duckdb_extensions/VERSION                           e.g. v1.5.5
  Resources/duckdb_extensions/osx_arm64/httpfs.duckdb_extension          (raw layout)
  Resources/duckdb_extensions/v1.5.5/osx_arm64/httpfs.duckdb_extension.gz (repo layout)
```

Downloads are cached in `target/duckdb_extensions_cache/` and `target/sparkle_cache/`.

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
   | `SPARKLE_PRIVATE_KEY` | contents of `~/.parquetry/sparkle_ed25519_private.key` (see below) |
5. **Update signing key.** Sparkle only installs updates signed with the EdDSA
   key whose public half is in `packaging/sparkle_public_key.txt`. The private
   key lives in `~/.parquetry/sparkle_ed25519_private.key` and must never be
   committed. **Back it up** (e.g. in a password manager): if it's lost,
   installed copies can't accept updates anymore and users must reinstall by
   hand. To make a new pair (only for a fresh start), run
   `target/sparkle_cache/2.10.0/bin/generate_keys --account parquetry -x key.txt`
   and put the printed public key in `packaging/sparkle_public_key.txt`.

## Signed local release

```sh
export CODESIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)"
UNIVERSAL=1 scripts/bundle.sh
scripts/package.sh
NOTARY_PROFILE=parquetry-notary scripts/notarize.sh      # or APPLE_ID/APPLE_TEAM_ID/APPLE_APP_PASSWORD
scripts/appcast.sh                                        # -> target/dist/appcast.xml
scripts/update-cask.sh 0.1.0 target/dist/Parquetry-0.1.0.zip   # -> target/dist/parquetry.rb
```

`notarize.sh` submits the zip, staples the app, rebuilds the zip and dmg
around the stapled app, then submits and staples the dmg too. Rejected
submissions print the notary log.

## CI release

Push a tag matching the workspace version: `git tag v0.1.0 && git push origin v0.1.0`.
The workflow only runs on tags once the repository variable
`AUTOMATED_RELEASES` is `true` (set it after adding the secrets above:
`gh variable set AUTOMATED_RELEASES --body true`); it can always be started by
hand from the Actions tab. Without it, publish a locally built release:
`gh release create v0.1.0 target/dist/Parquetry-0.1.0.{zip,dmg} target/dist/SHA256SUMS target/dist/appcast.xml`.
`release.yml` checks the tag against `Cargo.toml`, imports the certificate into a
temporary keychain, runs `UNIVERSAL=1 scripts/bundle.sh`, `package.sh` and
`notarize.sh` and `appcast.sh`, uploads the zip, dmg, `SHA256SUMS` and
`appcast.xml` to the GitHub release, and
commits the rendered cask to `OWNER/homebrew-tap`. It skips that last step if
`TAP_GITHUB_TOKEN` isn't set.

`ci.yml` runs `cargo test --workspace` and
`cargo clippy --workspace --all-targets -- -D warnings`. Both are strict: a
warning fails CI. It also lints the scripts, plists and cask, and smoke-tests
the CLI launcher.

## Windows

`release.yml`'s `windows` job builds `parquetry.exe` on `windows-latest`, checks it
doesn't need the Visual C++ runtime, and publishes
`Parquetry-<version>-windows-x64.zip` and its `.sha256` on the release. It runs for
every version tag, after the macOS job when that runs; without the macOS job it
creates the release itself.

Distribution is through a Scoop bucket, `OWNER/scoop-bucket` (public, with a
`bucket/` directory), the Windows counterpart of the Homebrew tap. The job renders
`bucket/parquetry.json` with `scripts/update-scoop.sh` and pushes it when the
`SCOOP_BUCKET_TOKEN` secret (a fine-grained PAT with *Contents: read & write* on the
bucket) is set. The manifest's `checkver`/`autoupdate` also let
`scoop` maintainers' tools bump it from new GitHub releases.

Builds aren't code-signed yet. Scoop's downloads carry no "downloaded from the
internet" mark, so SmartScreen doesn't warn; PCs that only allow signed apps will
block the exe regardless.

Order for a hand-made macOS release: publish the macOS assets first
(`gh release create vX.Y.Z …`, which also creates the tag), so the release is
never "latest" without `appcast.xml`; the tag then triggers the Windows job, which
adds its zip to that release.

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

## Automatic updates

Release bundles embed [Sparkle](https://sparkle-project.org) 2. The app checks
`SUFeedURL` once a day (users can turn that off in Settings, or use
*Parquetry ▸ Check for Updates…*). The default feed is
`https://github.com/tiroger/parquetry/releases/latest/download/appcast.xml`, so
**each release must include `appcast.xml`** and be the repo's *latest* release,
and the release assets must be publicly downloadable. With a private repo the
feed returns 404 and the app quietly finds no updates. To host the feed
elsewhere, build with `SPARKLE_FEED_URL=…` and run `appcast.sh` with
`DOWNLOAD_URL_PREFIX=…`.

Sparkle checks the zip's EdDSA signature and that the new app carries the same
Developer ID, then swaps the whole `.app` and relaunches. The build number
(`CFBundleVersion`) must increase from one release to the next. Homebrew users
can update either way: the cask has `auto_updates true`, so `brew upgrade`
leaves Sparkle-updated installs alone.

Builds without `Sparkle.framework` (`SKIP_SPARKLE=1`, `cargo run`) run normally,
with *Check for Updates…* disabled.

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

* `none` (the default, used by `release.yml`): nothing is bundled. DuckDB
  installs httpfs from extensions.duckdb.org the first time an S3 or HTTP
  location is opened (which needs the network anyway). **Notarized builds must
  use this:** Apple's notary service unpacks `.gz` files and rejects the
  extension binary because it carries DuckDB's signature, not a valid Apple
  one ("The signature of the binary is invalid"). Re-signing it would break
  DuckDB's own signature check.
* `repo`: gzipped files in DuckDB's own repository
  layout, `Resources/duckdb_extensions/<ver>/<platform>/httpfs.duckdb_extension.gz`.
  The app installs from it offline with
  `INSTALL httpfs FROM '<Resources>/duckdb_extensions'; LOAD httpfs;`, which
  copies the file into `extension_directory`. It's verified to work with DuckDB 1.5.5.
  Works for local builds; rejected by notarization (see above).
* `raw`: `Resources/duckdb_extensions/<platform>/httpfs.duckdb_extension`,
  loaded with `LOAD '<path>'`. Fine for local builds; not notarizable.

The engine tries `raw` first, then `repo` (`crates/engine/src/engine.rs`,
`ensure_extension`).

If the app doesn't handle the layout it's given, it falls back to downloading
httpfs on first use.
