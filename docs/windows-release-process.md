# Windows development and release process

The Windows packages use a candidate-and-promotion process. A release always
reuses the exact artifact that was tested; pushing a tag does not rebuild the
application.

## 1. Batch development changes

All Windows client work accumulates on the single long-lived development
branch, `codex/mediastation-windows-spike`, mirrored to
`mediastation/codex/mediastation-windows-spike`. It is the only branch that
receives feature and fix commits.

- Do not create or keep `release/*` branches. A published version is a tag
  created by the promotion workflow, not a branch; a version branch would only
  drift away from the development branch, and the candidate workflow does not
  need one. The retired `release/v1.0.3` branch is not a precedent to follow.
- `main` receives stage-stable batches only. A release candidate does not have
  to be on `main`: promote the exact commit that was built and tested.
- Accumulate a coherent batch of commits on the development branch.
- Run focused local checks for the changed code before requesting a candidate.
- Keep unrelated or untracked local files out of the release commits.
- Update `src/Cargo.toml` to the intended release version before the candidate
  build. The stable release tag must be the same version with a `v` prefix.

`build-windows` runs automatically for pull requests and pushes to `main`. For
the development branch, dispatch it manually only when the batch is ready for
end-to-end packaging, and pass the development branch (or the exact commit) as
the workflow ref.

## 2. Build a candidate once

Run `.github/workflows/build-windows.yml` with `workflow_dispatch` on the exact
branch or commit to test. The workflow builds x64 mpv and MediaStationGo,
creates the installer, portable archive, source archive, and `SHA256SUMS.txt`,
then stores them in the `windows-x64` artifact for 14 days.

Record the successful workflow Run ID. Failed or cancelled runs cannot be
promoted.

When publishing a new optional RIFE component, set `build_rife_component=true`.
The same candidate produces a separate `rife-component-x64-v1` artifact with
the ZIP and its checksum. Ordinary app releases leave this input disabled.
The base package is checked to exclude the optional payload and to start
successfully without it.

## 3. Test the candidate

Download `windows-x64` from the successful Run and test the package that will
be released. For an updater change, exercise both automatic and manual checks,
download progress, SHA-256 verification, confirmation, installer handoff,
settings retention, and launch after the upgrade.

Do not create or push the release tag during candidate testing.

## 4. Promote without rebuilding

Run `.github/workflows/release-windows.yml` manually with:

- `build_run_id`: the tested successful `build-windows` Run ID.
- `release_tag`: the matching stable tag, such as `v0.1.1`.
- `publish_rife_component`: enable only for the first publication of the fixed
  component version, using a candidate built with `build_rife_component=true`.

The promotion workflow verifies the source workflow, repository, event,
completion result, commit SHA, artifact file set, versioned filenames, and all
package hashes. It then tags the candidate commit and publishes those same four
files as the GitHub Release.

Component publication validates the ZIP and every manifest file, then publishes
it under `rife-component-x64-v1` with the candidate's matching source archive.
This separate Release must be a prerelease and must not become the latest app
release. Existing fixed component assets are never overwritten; a new component
contract requires a new versioned tag and client support. The main app Release
continues to contain exactly four files.

If validation fails, fix the source, create a new candidate Run, retest it, and
promote the new Run ID. Never replace assets on an existing stable release.
