# Fork release identity

Tags use `vX.Y.P+fork.N.gSHA` and record where on upstream the fork is up to
date, like `git describe`:

- `vX.Y.Z` is the newest upstream stable release tag the fork commit contains.
- `P` is `Z * 1000 + fork build` (build 1..999, reset for each upstream
  release). jcode compares only the numeric `X.Y.P`, so this keeps every fork
  release ordered within and across upstream releases.
- `N` is the number of upstream commits merged past `vX.Y.Z` (0 = exactly that
  release), and `SHA` is the upstream commit the fork is merged up to.

Example: `v1.0.2001+fork.85.gbe8c616` is fork build 1 on upstream `v1.0.2`
plus 85 commits, up to `be8c616`. A fork release made exactly at upstream
`v1.0.3` would be `v1.0.3001+fork.0.g<sha>`.

`scripts/fork/release-tag.sh next [REF]` prints the next tag for a commit,
taking the build number from existing fork tags for the same upstream release.
`scripts/fork/release-tag.sh verify TAG [REF]` re-derives every field and fails
on any mismatch; release preflight runs it on the tagged commit. Upstream
commits are fetched with `--no-tags` and upstream tags are read with
`ls-remote`, so upstream tags never enter the fork.

Earlier releases used `vMAJOR.MINOR.PATCH+fork.pN` (N = upstream
`v1.0.0-preview.N`), up to `v0.99.1008+fork.p199`. They remain valid installed
versions, and `v1.0.2001` sorts above them, so they update normally. The
workflow no longer accepts the old format for new releases.

Build metadata survives the build/runtime version override. SemVer precedence
and release identity ignore it, so patch 1003 updates to 1004, patch 1004 never
automatically returns to 1003, and metadata alone cannot cause a reinstall.
Explicit stable selection retains its existing intentional rollback behavior.

The Rust updater selects platform archives and SHA256SUMS by asset name and
uses GitHub API `browser_download_url` verbatim for both downloads; it does not
construct release download URLs. Asset names, archive members and checksums
are unchanged. Shell and PowerShell installers construct GitHub URLs and now
encode the tag's plus as `%2B`; the shell redirect lookup also decodes `%2B`.
A literal plus is legal in a URL path (it is not form/query space encoding).
Other release URL generators (package updates and cloud-managed setup) use
literal plus in path segments and therefore need no change.

The updater strips only the leading v before joining `builds/versions/<version>`.
Build support writes the complete version to `current-version` and
`stable-version`, and readers only trim whitespace. Shell and PowerShell
installers likewise retain metadata in version paths and stable markers.
The Linux wrapper quotes its resolved directory and executable path; plus is
legal in these filesystem paths. No asset lookup depends on the release tag.

Regression tests: `release_identity_preserves_build_metadata`,
`update_versions_follow_semver_precedence_not_build_metadata`, and
`fork_release_metadata_preserves_update_identity_and_assets`.

Release preflight runs before the build matrix. It checks out the tag with
full history and runs `release-tag.sh verify`, which stops the workflow before
compilation on a malformed tag, a wrong upstream release, distance or commit,
or a missing upstream release. The publish job consumes the verified upstream
tag, distance and SHA as job outputs and titles the release
`jcode fork X.Y.P (upstream vX.Y.Z+N)`. Build checkouts retain full history for
the embedded 700-commit changelog.
