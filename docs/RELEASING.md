# Releasing and updating Agu Chat

Agu Chat updates itself from GitHub Releases of `TrinityTF/agu-chat` (the repo does not exist yet; the user
decides name and visibility, then updates `repository.url` and `build.publish` in `package.json` if it differs).
The release pipeline mirrors the Esmount VOD Downloader: a workflow builds the installer, a draft release is
created first, electron-builder uploads into it, and only then is it published, so the app never sees a
half-uploaded release.

## Channels

| Channel | Gets | GitHub release type | Version |
| --- | --- | --- | --- |
| **beta** | prereleases and full releases, whichever is newest | prerelease (never "latest") | `<base>-beta.<run number>` |
| **stable** | full releases only | full release, marked "latest" | `<base>` |

The user picks the channel in settings (`settings.updateChannel`, `"beta"` default). `updater.setChannel()` takes
effect at once: it re-checks immediately. `<base>` is `package.json`'s version without its prerelease part
(`0.0.1-beta.1` -> `0.0.1`). "v0.01" means `0.0.1-beta.1`.

### How CI produces them (`.github/workflows/release.yml`)

- **Push to `main`** -> beta: `node scripts/prepare-release.mjs beta <run_number>` sets `0.0.1-beta.<run_number>`,
  writes the release notes (commits since the previous release), creates the draft with `--prerelease`, builds
  (`npm run release` = vite build + `electron-builder --win --publish always`), checks that `.exe`, `.blockmap`
  and `.yml` were uploaded, then publishes with `--prerelease --latest=false`.
- **Manual run (workflow_dispatch) with channel = stable**, from `main` only -> `prepare-release.mjs stable`
  sets `0.0.1`, notes are every commit since the previous *stable* tag, the draft is created without
  `--prerelease` and published with `--latest`.
- Docs-only pushes (`**.md`, `docs/**`) do not build. Runs never overlap (`concurrency: release`).
- `<base>` moves up by itself: if a stable tag `v<base>` already exists, the base becomes the next free patch
  (after `v0.0.1`, pushes make `0.0.2-beta.N`). That is what keeps every later beta *newer* than the last stable.
  To start a new minor/major line, edit `version` in `package.json` (e.g. `0.1.0-beta.1`) and push.
- The run number only ever grows, so betas of the same base always sort in release order.

Local builds: `npm run dist` builds the installer into `dist/` without publishing (version stays whatever
`package.json` says, e.g. `Agu-Chat-Setup-0.0.1-beta.1.exe`). `npm run icon` re-renders `build/icon.png` from
`build/icon.svg`.

## What the app does (`src/main/updater.js`)

- Not packaged (dev): `status = "dev"`, nothing is checked.
- Packaged: first check ~5 s after start, then every 4 h; a failed check retries after 15 min.
- A found update downloads automatically (`status: downloading`, `progress` 0..1) and ends in `ready`;
  `autoInstallOnAppQuit` is on, so it installs silently when the app quits. The UI can show "Restart to update"
  while `ready` and call `install()` (`quitAndInstall(true, true)`: silent installer, app relaunches).
- Statuses: `idle | dev | checking | none | available | downloading | ready | error`. (`available` is not used:
  downloads start by themselves.) Errors are one friendly line (offline, rate limit, update files missing).
  "Nothing published for this channel yet" is reported as `none`, not as an error.
- Release notes come from the release body; HTML is turned into plain lines (max 12).

## Update rules (verified against electron-updater 6.8.11)

The app calls the library with `allowPrerelease = (channel === "beta")`, `allowDowngrade = false`, `autoDownload = false`
(the app starts the download itself) and never sets `autoUpdater.channel`. Why, with the library files read:

1. **Which release is picked** (`node_modules/electron-updater/out/providers/GitHubProvider.js`, `getLatestVersion`):
   - `allowPrerelease = false` (stable): reads `GET /TrinityTF/agu-chat/releases/latest`, which GitHub resolves to the
     newest non-prerelease, non-draft release. Prereleases are therefore invisible. This is why beta releases must be
     published as prereleases and never as "latest".
   - `allowPrerelease = true` (beta): reads `releases.atom` (newest first, drafts excluded). If the installed version
     is a beta (`0.0.1-beta.1`) the current channel is `beta`, and the first entry whose tag is a valid semver
     that is a `beta` or a stable version wins; if the installed version is stable, the first entry wins. Either way
     it is the *newest by release order*, and `AppUpdater.isUpdateAvailable` (`AppUpdater.js`) then compares
     semver, so an older entry is never installed.
2. **Which yml**: for the GitHub provider electron-builder always writes `latest.yml`, even for a prerelease version
   (`app-builder-lib/out/publish/updateInfoBuilder.js`, `computeChannelNames`: for `provider: github` only
   `publishConfig.channel || "latest"`, and the channel detected from a `-beta.N` version is not applied to GitHub
   configs in `publish/PublishManager.js`). Local build confirmed: `dist/latest.yml` with `version: 0.0.1-beta.1`.
   For a beta tag the library first tries `beta.yml` on that release, gets a 404 and falls back to `latest.yml`
   (the "Allow fallback to `latest.yml`" branch). Do **not** set `publish.channel` or `autoUpdater.channel`: a
   packaged `channel: beta` would make a stable-mode check look for `beta.yml` on the stable release and fail, and
   setting `autoUpdater.channel` forces `allowDowngrade = true`.
3. **Beta user on `0.0.1-beta.1`** gets `0.0.1-beta.2`, and also a later stable `0.0.1` (and `0.0.2-beta.N` after it):
   semver orders `0.0.1-beta.1 < 0.0.1-beta.2 < 0.0.1 < 0.0.2-beta.1`.
4. **Stable user never gets a prerelease**: see 1. With the stable channel selected the prerelease tags are never read,
   whatever version is installed. (A race, where a check that started before a channel switch
   returns a prerelease, is filtered by `allowedOnChannel` in `updater.js`, and an already downloaded prerelease is
   discarded and `autoInstallOnAppQuit` switched off.)
5. **Switching beta -> stable never downgrades** (`allowDowngrade = false`, a deliberate choice). A user on
   `0.0.2-beta.3` who picks stable simply stays on that build until a stable release newer than it exists
   (`0.0.2` or later). A forced downgrade would install an older build over a newer one (settings files written by
   the newer build may not load) and would surprise users; someone who really wants the older stable can
   install it manually, user data is kept. Switching stable -> beta takes the newest beta/stable on the next check.
6. **A half-uploaded release is never offered**: drafts are not in `releases.atom` or `/releases/latest`; the release is
   published only after all assets exist (workflow checks `.exe`, `.blockmap`, `.yml`).

Empirical check: `electron-updater`'s real `GitHubProvider` was run against a fake GitHub (local HTTP server serving
`releases.atom`, `/releases/latest` and `latest.yml`) for 11 scenarios (beta new beta, beta new stable, beta after stable,
already current, stable never prerelease, stable new stable, beta->stable without downgrade, beta->stable with a newer
stable, stable->beta, stable user on beta channel, only older betas): all matched the rules above.

## First release checklist

1. Create the GitHub repo (`TrinityTF/agu-chat`), push `main`; Actions needs "Read and write permissions" for the
   `GITHUB_TOKEN` (Settings, Actions, General, Workflow permissions) because the workflow creates releases.
2. The first push builds `0.0.1-beta.<run number>`; install it from the release page.
3. For the first stable release, run the "Build and release" workflow manually with channel = stable.
4. The installer is unsigned (SmartScreen warns on first install); signing is a later step (`win.certificateFile`
   and `publisherName`; once `publisherName` is set the updater also verifies the signature).
5. If the repo is private the updater needs a token; keep it public (or a public releases-only repo).
