# Windows development and release process

The Windows packages use a candidate-and-promotion process. A release always
reuses the exact artifact that was tested; pushing a tag does not rebuild the
application.

## 1. Batch development changes

- Develop on a feature branch and accumulate a coherent batch of commits.
- Run focused local checks for the changed code before requesting a candidate.
- Keep unrelated or untracked local files out of the release commits.
- Update `src/Cargo.toml` to the intended release version before the candidate
  build. The stable release tag must be the same version with a `v` prefix.

The Windows workflow still runs for pull requests and pushes to `main`. For a
feature branch, run `build-windows` manually only when the batch is ready for
end-to-end packaging.

## 2. Build a candidate once

Run `.github/workflows/build-windows.yml` with `workflow_dispatch` on the exact
branch or commit to test. The workflow builds x64 mpv and MediaStationGo,
creates the installer, portable archive, source archive, and `SHA256SUMS.txt`,
then stores them in the `windows-x64` artifact for 14 days.

Record the successful workflow Run ID. Failed or cancelled runs cannot be
promoted.

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

The promotion workflow verifies the source workflow, repository, event,
completion result, commit SHA, artifact file set, versioned filenames, and all
package hashes. It then tags the candidate commit and publishes those same four
files as the GitHub Release.

If validation fails, fix the source, create a new candidate Run, retest it, and
promote the new Run ID. Never replace assets on an existing stable release.
