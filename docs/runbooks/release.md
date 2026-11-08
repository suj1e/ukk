# Release runbook

Use this runbook for unsigned development packages and future signed releases.
The packaging contract is [docs/release.md](../release.md).

## Build and verify

1. Merge with CI green on the exact `main` SHA.
2. Open the `Release (unsigned packages)` workflow run for that SHA.
3. Download all three artifacts and verify the outer checksum:

   ```bash
   gh run download <run-id> -D dist
   shasum -a 256 -c dist/*/*.sha256
   tar -xzf dist/*macos*/*.tar.gz
   ./ukk-*/ukk-host --version
   ./ukk-*/ukk-desktop --version
   ```

4. Record the run URL, commit, archive hashes and OS versions in the PR or
   [release.md](../release.md). Do not claim signing, notarization or
   real-device acceptance.

## Tag

Tags are for a tested commit only:

```bash
git switch main && git pull --ff-only
git tag v0.1.0
git push origin v0.1.0
```

The tag run produces the same unsigned archives with the tag version. It does
not create a GitHub Release, sign, notarize or publish externally.

## Rollback

- **Bad package, source still good:** rebuild from the previous known-good SHA;
  do not reuse a failed artifact.
- **Bad merge:** revert on a new branch, run CI, merge; never force-push `main`.
- **Bad tag:** delete the tag only with owner approval; publish a new patch tag
  instead of moving an existing tag.
- **Host metadata damage:** stop clients, follow
  [host-lifecycle.md](host-lifecycle.md), restore the verified backup, then
  restart the Host. Agent terminals are not stopped by metadata restore.

## Stop conditions

Do not publish when checksums fail, a platform job is missing, the exact SHA
has no green CI, or the build used `runtime_shaders`. Stop and fix the source.
