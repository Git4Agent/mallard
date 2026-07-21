# Releasing Mallard

Mallard publishes first-install packages and signed in-app updates from the same
GitHub Release. A pushed `v*` tag starts `.github/workflows/release.yml`, which
builds Apple Silicon and Intel macOS packages plus a Windows x64 NSIS package.
The release remains a draft until it has been installed and verified.

The source repository and its GitHub Releases are private. After all native
builds finish, the workflow copies the public installer and updater artifacts
to the `releases/` prefix of the existing `mallard-cloud-prod-demo` R2 bucket.
The public Worker serves them from `https://api.mallard-ai.com/v1/releases/`.

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

The public R2 publication job also requires:

- `CLOUDFLARE_API_TOKEN`, scoped to write objects to the production demo bucket;
- `CLOUDFLARE_ACCOUNT_ID`.

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

Merge both release workflow files into the repository's default branch before
creating a tag. GitHub evaluates the `release: published` workflow from the
default branch when the draft is published.

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

4. Wait for all three release jobs and the public-artifact job. This uploads
   immutable objects under `/v1/releases/v0.2.0/`, but does not change the
   updater's stable `latest.json` endpoint.
5. Inspect the draft GitHub Release, edit its release notes, install each
   first-install package, and verify the versioned R2 manifest and downloads.
6. Publish the draft. The `Promote published update` workflow validates the
   versioned manifest and only then replaces `/v1/releases/latest.json`.

Deploy the release routes in `mallardInternal/cloudflare/api` before the first
release. Until that Worker version is live, the public manifest URL returns
404 even when the R2 objects have been uploaded.

Do not replace artifacts under an existing version. Fix the problem, increment
the semantic version, and publish a new signed release.

## Local updater build

On macOS, a local signed updater bundle can be built with:

```bash
TAURI_SIGNING_PRIVATE_KEY="$HOME/.tauri/mallard.key" \
TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" \
  npm run tauri build -- --bundles app,dmg
```

The normal DMG is the first-install package. The updater consumes the generated
`.app.tar.gz` and `.sig` artifacts. On Windows it consumes the NSIS setup
executable and its `.sig` file. `tauri-action` uploads these files and generates
the multi-platform `latest.json` file automatically. The publication step also
adds a `downloads` map with the DMG and NSIS installer URLs, sizes, and SHA-256
digests. The website reads that map, while Tauri ignores the extra field and
continues to use the signed `platforms` entries.

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
