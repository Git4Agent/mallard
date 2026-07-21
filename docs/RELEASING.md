# Releasing Mallard

Mallard publishes first-install packages and signed in-app updates from the same
GitHub Release. A pushed `v*` tag starts `.github/workflows/release.yml`, which
builds Apple Silicon and Intel macOS packages plus a Windows x64 NSIS package.
The release remains a draft until it has been installed and verified.

## Ad-hoc tester builds

Until Mallard has an Apple Developer Program membership, macOS packages use
Tauri's explicit ad-hoc signing identity. This makes the bundle structurally
valid on Apple Silicon, but it is not a Developer ID signature and cannot be
notarized. Testers must grant manual Gatekeeper approval after copying Mallard
to Applications; do not describe these artifacts as trusted public macOS
releases.

Before publishing a draft, verify its macOS app bundle with:

```bash
codesign --verify --deep --strict --verbose=4 /path/to/Mallard.app
```

The release workflow performs the same check inside the Tauri build command,
before `tauri-action` creates or uploads a draft. A failed check therefore
prevents release assets from being created.

GitHub Releases is the public source of truth for installers and updates. The
stable updater manifest is always available at:

```text
https://github.com/Git4Agent/mallard/releases/latest/download/latest.json
```

The download website reads the repository's public latest-release API and uses
the returned `browser_download_url` values. No Cloudflare credential, R2
mirror, or GitHub token is required for public downloads.

## Updater signing key

The public updater key is committed in `src-tauri/tauri.conf.json`. The matching
private key is intentionally outside this repository. On the machine where the
updater was introduced it was generated at:

```text
~/.tauri/mallard.key
```

It is passwordless and protected by the containing user directory and owner-only
file permissions. Back it up in an encrypted secrets system before publishing
the first updater-enabled release. Losing this key prevents installed copies
from accepting future updates.

Add the private key content to the repository's Actions secrets:

```bash
gh secret set TAURI_SIGNING_PRIVATE_KEY < ~/.tauri/mallard.key
```

`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` may be omitted for the current key. If the
key is replaced with a password-protected key, create that secret as well.

Updater signing proves that a release came from Mallard. It does not replace
Apple Developer ID signing/notarization or Windows Authenticode signing.

## Platform signing secrets

For a trusted public macOS release, configure these GitHub Actions secrets:

- `APPLE_CERTIFICATE`
- `APPLE_CERTIFICATE_PASSWORD`
- `APPLE_SIGNING_IDENTITY`
- `APPLE_ID`
- `APPLE_PASSWORD`
- `APPLE_TEAM_ID`

Configure a Windows signing certificate and Tauri `signCommand` or certificate
thumbprint before treating the Windows package as a trusted public build. The
workflow can create updater-signed internal installers before those publisher
credentials are available, but operating-system warnings will remain.

## Prepare a release

Merge the release workflow into the repository's default branch before
creating a tag.

1. Update the version in all three files:
   - `package.json`
   - `src-tauri/Cargo.toml`
   - `src-tauri/tauri.conf.json`
2. Run the checks:

   ```bash
   npm run check:release-version -- v0.2.0
   npm run build
   npm run test:integration
   ```

3. Commit the version change, then create and push the matching tag:

   ```bash
   git tag v0.2.0
   git push origin v0.2.0
   ```

4. Wait for all three release jobs. `tauri-action` attaches the installers,
   updater bundles, signatures, and generated `latest.json` to one draft
   GitHub Release.
5. Inspect the draft GitHub Release, edit its release notes, install each
   first-install package, and verify every attached artifact.
6. Publish the draft. GitHub's release URLs immediately expose the versioned
   assets, while `/releases/latest/download/latest.json` resolves to the newly
   published release. The website discovers the same release independently.

Do not replace artifacts under an existing version. Fix the problem, increment
the semantic version, and publish a new signed release.

## Local updater build

On macOS, a local signed updater bundle can be built with:

```bash
TAURI_SIGNING_PRIVATE_KEY="$HOME/.tauri/mallard.key" \
  npm run tauri build -- --bundles app,dmg
```

The normal DMG is the first-install package. The updater consumes the generated
`.app.tar.gz` and `.sig` artifacts. On Windows it consumes the NSIS setup
executable and its `.sig` file. `tauri-action` uploads these files and generates
the multi-platform `latest.json` file automatically. The website independently
discovers the DMG and NSIS installers from the latest GitHub Release API.

## Upgrade verification

Before publishing, verify at minimum:

- no update is offered when the installed version is current;
- macOS Apple Silicon, macOS Intel, and Windows x64 select the correct artifact;
- an update cannot start during Push, Pull, restore, or another busy operation;
- download failure can be retried without changing user files;
- an invalid signature is rejected;
- the app restarts into the new version and shows its release notes once; and
- project registrations, provider profiles, and `~/.codex` / `~/.claude` data
  remain unchanged.
