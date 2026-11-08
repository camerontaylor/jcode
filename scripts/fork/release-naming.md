# Fork release identity

Tags use `vMAJOR.MINOR.PATCH+fork.pN`: PATCH retains upstream patch * 1000 +
fork build; N names upstream `v1.0.0-preview.N`. The next release is
`v0.99.1004+fork.p199`, based on upstream
`7fd42339c907de223021bd63e5bb5df778c43a01` and custom
`bf9f0b22f5a4374b9e90a8ac6c74750930349305`.
`v0.99.1003` remains a valid installed release. The release workflow requires
new-style tags even for manual dispatch, avoiding ambiguous new publication.

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
