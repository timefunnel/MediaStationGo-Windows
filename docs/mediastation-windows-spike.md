# MediaStation Windows Spike

## Scope

This branch validates whether Jellium Desktop can host a dedicated
MediaStationGo Windows client. It is not the final application repository.
The Android TV project remains a read-only protocol and interaction reference.

The active RIFE precision, TensorRT profile, and VRR refresh-matching work is
tracked in [windows-rife-tensorrt-profiles.md](windows-rife-tensorrt-profiles.md).

The Spike must prove these paths before product UI work expands:

1. Resolve standard Emby and MediaStationGo PlaybackInfo streams through an
   explicit redirect chain.
2. Reuse the final server or CDN URL for the playback session.
3. Keep server credentials on the configured server origin only.
4. Probe the final URL with `Range: bytes=0-0` and report failures explicitly.
5. Pass session headers to libmpv without exposing tokens to JavaScript.
6. Validate SDR and HDR10 output, subtitle composition, and track switching on a
   real Windows HDR display.

Standard Emby is the baseline playback contract. The client uses
`MediaSources[].Container`, `MediaSources[].Id`, `PlaySessionId`, and the
default stream indexes to construct
`/Videos/{Id}/stream?Container=...&Static=true`. The server filesystem
`Path` is metadata only and is never exposed to mpv or the renderer.
MediaStationGo may additionally provide `DirectStreamUrl` and
`PlaybackPreferences`; those are optional extensions, not requirements for
standard Emby playback. When `DirectStreamUrl` is present, it remains the
authoritative MediaStationGo source and the standard URL constructor is not
used.

## Ownership Boundaries

Rust owns:

- MediaStationGo and standard Emby authentication and API requests.
- Standard Emby and MediaStationGo PlaybackInfo parsing and stable track
  identifiers.
- Redirect handling, CDN URL expiry, Range probing, and session reuse.
- Playing, Progress, and Stopped reports.
- Optional PlaybackPreferences reads and writes. A standard Emby server without
  that extension uses PlaybackInfo defaults and keeps track changes for the
  current native session only.
- Sensitive headers and the libmpv load request.

The packaged CEF frontend owns:

- Navigation, focus state, page restoration, drawers, and presentation state.
- Home, library, details, season/episode, and player-control views.
- Asynchronous IPC calls identified by request IDs.

The frontend must not receive the MediaStationGo token, place credentials in a
URL, perform authenticated media requests, or keep a second media-library data
store. Local persistence is limited to non-authoritative UI state and cache
metadata.

## Native Account Session

Authentication is native-owned. The renderer may submit a server URL,
username, password, client profile, and proxy mode to the native login IPC, but
it never receives the resulting token or authorization header. The native API calls
`Users/AuthenticateByName`, validates the returned account fields, constructs
the authenticated header, and configures playback only after persistence has
succeeded.

The spike persists the active session in Windows Credential Manager under
`MediaStationGo.Windows.ActiveSession.v1` and keeps one credential per saved
account under `MediaStationGo.Windows.Account.<accountId>.v1`. `accountId` is a
stable lowercase SHA-256 digest of the normalized server URL and user ID. It is
only an opaque selector; the renderer cannot derive or receive the token from
it. The regular JSON settings file contains no account secret; its only
account-related value is the selected server URL.

Credential payload schema v2 stores the connection profile with the secret.
Schema v1 credentials remain readable and migrate as `mediastation_go` with
`direct` proxy mode. Supported combinations are intentionally closed:
MediaStationGo always uses its original client identity and defaults to
explicit direct agents, while allowing an explicit system-proxy selection;
standard Emby uses either the SenPlayer or Infuse identity and follows the
same per-server proxy selection (direct by default). API requests, redirect/
Range probes, subtitles,
playback reporting, and mpv HTTP playback all use that per-server profile.

Startup restores the active credential only when its exact normalized server
URL matches the selected server. It also creates or refreshes that account's
per-account credential as a migration step, so installations with the former
single-account credential become selectable without another login. Malformed,
unreadable, mismatched, or target/content-inconsistent credentials fail
explicitly and are not silently deleted or skipped.

The account drawer enumerates the per-account Credential Manager entries with a
target-prefix filter and groups users by normalized server URL. It receives
only `accountId`, `serverId`, `baseUrl`, `userId`, `userName`, `serverType`,
`clientProfile`, and `proxyMode`. Selecting an account reads that exact credential by stable ID,
revalidates its identity, updates the active session, invalidates in-flight
native work, and reloads the catalog. There is no renderer-side account store,
token cache, fallback account, or list-index identity.

The drawer can also start a new login while retaining the active native
session. Cancel returns to the original account, page, and focus target. A
successful authentication upserts that account's secure entry and makes it
active; it does not remove the other saved accounts.
Adding a user under an existing server inherits and locks that server's
connection profile. Updating a user re-authenticates only that stable account
and cannot change the server profile. Deleting an inactive account leaves the
active session untouched; deleting the active account clears the active session
only after both credential deletions succeed.

Login commits, saved-account switches, and logout are serialized under the
native session generation. If logout or another account change happens while a
login request is in flight, the late login result receives `session_changed`
and cannot write a credential or reactivate the account. Updating the server
setting, per-account credential, and active credential is transactional: a
later failure restores every earlier changed value, and a failed rollback is
reported separately. Logout deletes both the current per-account credential
and active credential before clearing the in-memory session; deletion failure
leaves the current session intact and restores a deleted active credential.
Other saved accounts remain available.

The renderer-facing account calls are:

```javascript
window.jmpNative.mediaStationAuthenticate(
  requestId, baseUrl, username, password, clientProfile, proxyMode
);
window.jmpNative.mediaStationSessionStatus(requestId);
window.jmpNative.mediaStationListAccounts(requestId);
window.jmpNative.mediaStationSwitchAccount(requestId, accountId);
window.jmpNative.mediaStationUpdateAccount(
  requestId, accountId, username, password, clientProfile, proxyMode
);
window.jmpNative.mediaStationDeleteAccount(requestId, accountId);
window.jmpNative.mediaStationLogout(requestId);
```

They respond through `_onMediaStationResponse` with operations
`authenticate`, `session_status`, `list_accounts`, `switch_account`,
`update_account`, `delete_account`, and `logout`. No response contains a token,
password, or authorization header. Logout also reports whether a stored
credential was deleted.

## Native Async Load IPC

Native host code configures or clears a `MediaStationSession` through the Rust
API. There is intentionally no JavaScript API that accepts a token or private
authorization header.

The renderer starts a load with:

```javascript
window.jmpNative.mediaStationLoad(requestId, mediaId, startMilliseconds);
```

The browser process performs PlaybackInfo, optional PlaybackPreferences,
redirect, and Range work on a background thread. Only one load request may
resolve at a time.
Duplicate or concurrent requests receive explicit errors instead of spawning
unbounded network work.

Results are delivered to:

```javascript
window._onMediaStationResponse(requestId, "load", ok, payloadJson);
```

A successful response means the validated request was queued to libmpv; it does
not claim that playback or the first frame has started. The payload contains
only redacted probe metrics and selected stable track keys. It never contains a
media URL, token, authorization header, or raw server response body. Changing
the native account/session invalidates in-flight work with `session_changed`.
While a native MediaStation session is configured, legacy JavaScript URL IPC
such as `playerLoad`, `playerAddSubtitle`, and `playerAddAudio` is rejected so
the renderer cannot bypass native URL and credential ownership.

The playback coordinator must start before the main CEF browser. Restoring the
Credential Manager session during browser creation initializes the
MediaStation runtime and registers its playback event sink. Sink registration
returns an explicit failure if the coordinator does not exist; events are
never accepted and silently discarded.

Selected external subtitles are fetched by the native API into a bounded local
temporary file before libmpv receives the load request. Subtitle redirects are
followed independently from the main media session: MediaStation credentials
are attached only on the exact server origin and are removed after a
cross-origin redirect. The renderer and libmpv never receive the authenticated
subtitle URL. Temporary subtitle files are released on replacement, stop,
terminal playback events, account changes, and browser close.

Playback reporting is driven by the native playback coordinator, not renderer
hints. For the Windows gpu-next path, mpv `playback-restart` is the first-frame
readiness signal when `video-frame-info` does not produce a usable property
edge. It promotes only a Starting video with no active buffering; it cannot
resume an explicit pause. `Playing` is queued only after that promotion.
`Progress` is bounded to a single background worker and is
reported every 10 seconds, plus immediately after pause, resume, or a completed
seek. `Stopped` uses the last native position retained before the playback
state machine clears its terminal snapshot. Account changes and replacement
loads close the previous started session explicitly.

## Track Metadata Contract

PlaybackInfo is the authoritative track catalog. Every Audio and Subtitle
entry in the selected MediaSource is parsed, stable keys prefer the server
MediaStream `Index`, and missing indices use the documented diagnostic hash.
The renderer receives descriptors and stable keys but no track URLs.

A live test item demonstrated an important server boundary: PlaybackInfo
returned one AC3 audio entry while libmpv demuxed nine audio tracks from the
same MKV. The client records the PlaybackInfo count and stable descriptors in
debug logs but does not merge the libmpv list as a second source. Exposing those
additional tracks requires a server/API contract that can persist and restore
their stable identities before playback starts. Until then, the UI accurately
shows the one authoritative server track.

## Playback Session Rules

- The PlaybackInfo source URL must start on the configured server origin.
- Every redirect is followed manually with a maximum of six hops.
- `X-Emby-Token` and `X-Emby-Authorization` are attached only to the exact
  server origin. Scheme, host, and effective port must all match.
- HTTPS-to-HTTP redirects are rejected.
- A cross-origin final URL is accepted only after the server redirect chain and
  only when byte ranges are supported.
- Direct CDN sessions contain no private server headers.
- Standard Emby requests require a configured Windows system proxy. Its mpv
  HTTP loads receive the proxy as a per-file `http-proxy` option. MediaStationGo
  defaults to explicit direct API/probe agents and the `mediastation://` stream
  reader, preserving the existing direct MSG chain; an explicit MSG system
  proxy profile applies to both.
- Same-origin streams constructed from the standard Emby contract are handed
  to mpv as ordinary HTTP URLs so `X-Emby-Token` and
  `X-Emby-Authorization` are applied to every range request. The existing
  MediaStationGo `DirectStreamUrl` path remains on the native
  `mediastation://` reader, including its cross-origin CDN behavior.
- External subtitle responses are limited to 16 MiB and use an explicit format
  allowlist. Empty, oversized, unknown-format, or insecurely redirected
  subtitles fail without silently disabling the saved subtitle preference.
- Credential query parameters returned on same-origin media or subtitle URLs
  are removed structurally and replaced by native origin-scoped headers.
  Cross-origin credential query parameters are rejected.
- Missing `Location`, malformed length metadata, denied ranges, expired URLs,
  and transport errors remain distinguishable errors.
- Known credentials and the complete query component of every HTTP(S) URL are
  redacted in debug output, including signed CDN links emitted by libmpv.

## Jellium Reuse

Keep:

- The native mpv window and Windows compositor relationship.
- CEF overlay composition, Windows input/window integration, and SMTC.
- Playback coordinator events, first-frame gating, and track APIs.
- The `app://` resource scheme after expanding it for packaged frontend assets.

Replace or extend:

- Remote Jellyfin Web loading.
- Jellyfin-specific JavaScript injection and one-way fire-and-forget IPC.
- URL-only `playerLoad` requests.
- Silent no-op behavior on load failures.
- Missing HDR/output/refresh-rate diagnostics.

The Spike currently pins Jellium's mpv fork at
`ab0467ff24c2efc995b2b6e6b50a8b0912d373a6`. This is a compatibility baseline,
not a final long-term mpv selection.

## Blink UI Reference

Blink is a visual and information-architecture reference only. Its browser
`video` playback path is not used.

Adopt these ideas:

- Artwork-led home composition with a focused title connected to the backdrop.
- Clear horizontal media rows and distinct landscape/portrait card ratios.
- Lightweight progress indicators integrated into artwork.
- Full-bleed detail backdrops with readable metadata hierarchy.
- Compact icon controls and direct access to search and account actions.
- Skeleton/loading states that preserve final layout dimensions.

Adapt them for MediaStationGo:

- Keep the brand logo at the fixed top-left position.
- Keep the active user and settings gear at fixed top-right positions on all
  browse pages.
- Open account and settings in right-side drawers and restore focus on close.
- Focus the artwork only, using a restrained scale and thin outline. Titles are
  not wrapped in a focus rectangle.
- Use restrained card radii and a layered dark neutral palette instead of
  Blink's large rounded panels and blue-heavy gradients.
- Continue Watching starts playback directly and uses user-facing episode text.
- Forward navigation resets the new page to the top; Back restores the previous
  page's scroll and focus state.
- Replacement-account login is cancelable and does not clear the active account
  until native authentication succeeds.
- Resume feedback and the primary control layer close together after one second;
  seeking while controls are hidden does not expand the primary layer.

Do not copy:

- Browser media playback, token-bearing URLs, or frontend-owned authentication.
- Material-style phone navigation patterns.
- Hover-only actions, heavy backdrop blur, 20px card radii, or pill decoration.
- LocalStorage playback preferences or a local media-library mirror.
- Independent focus and scrolling animations that complete at different times.

## Validation Gates

The Spike is successful only when all of the following have evidence:

- Unit tests cover credential isolation, redirects, Range behavior, expiry,
  user-scoped reuse, and log redaction.
- A real MediaStationGo account can resolve and start a server or CDN stream.
- Playing, Progress, and Stopped reports are visible on the server.
- Subtitle and audio preference failures cannot create a startup crash loop.
- SDR and HDR10 are verified on a real HDR display with source format, decoded
  format, display capability, and actual output mode reported separately.
- Unsupported Dolby Vision content fails explicitly instead of producing a
  green or purple image.
