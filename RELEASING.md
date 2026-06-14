# Releasing Stratify

Releases are automated by cargo-dist. A pushed `v*` tag builds cross-platform
binaries, attaches them to a GitHub Release with checksums, updates the
Homebrew tap, and publishes a shell installer.

## Cut a release

1. Make sure `version` in the root `Cargo.toml` `[workspace.package]` matches
   the tag you are about to push (for example `0.1.0` for tag `v0.1.0`).
2. Tag and push:

   ```sh
   git tag v0.1.0
   git push origin v0.1.0
   ```

3. Watch the `Release` workflow in GitHub Actions. When it finishes, the
   release at `releases/latest` has the binaries, the `brew` formula is in
   `stratify-dev/homebrew-tap`, and the installer is published.

## Verify

```sh
brew install stratify-dev/tap/stratify
stratify --version
```

## Publish the Action to the GitHub Marketplace (once)

1. Open the published release page for the tag.
2. Ensure 2FA is enabled on your account and accept the GitHub Marketplace
   Developer Agreement when prompted.
3. Check "Publish this Action to the GitHub Marketplace".
4. Pick categories (Code quality, Continuous integration) and publish.

The Action's `name` ("Stratify Quality Gate") must be unique across the
Marketplace. If it is taken, change `name:` in `action.yml`, push, and re-tag.

## Prerequisite: Homebrew tap token

The release workflow pushes the formula to `stratify-dev/homebrew-tap`, a
separate repo. That requires a Personal Access Token with write access to the
tap, stored as the `HOMEBREW_TAP_TOKEN` secret on `stratify-dev/stratify`. Set
it once before the first release.
