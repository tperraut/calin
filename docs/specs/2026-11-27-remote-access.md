# Remote access: sign-in, server install, phone and desktop

Status: accepted · 2026-11-27 · milestone M5

Until now Calin only listens on loopback and trusts whoever reaches it. This document specifies
what lets it run on a server and be used from a phone and other machines: sign-in, the rules for
when sign-in is required, access for non-browser clients, installing it as a service, the
installable phone app with notifications, and the desktop app.

A Calin server gives whoever is signed in a shell and agents with a shell. Everything here errs on
the side of refusing.

## 1. Two modes

| | Local | Public |
| --- | --- | --- |
| Selected by | default | `CALIN_PUBLIC_URL` is set |
| Listens on | a loopback address | a loopback address, behind a reverse proxy that terminates TLS |
| Who may call the API | any local process whose request passes the Host and Origin guard | signed-in browsers and holders of a device token |
| Sign-in | none | required |

- The server never listens on a non-loopback address itself. `--bind` with a non-loopback address
  stays an error. Public reach is always through a reverse proxy on the same machine, so TLS is
  never Calin's job and a misconfigured bind cannot expose it.
- In public mode the server refuses to start unless sign-in is fully configured and the allow-list
  is not empty.
- `CALIN_PUBLIC_URL` is the exact origin users type, for example `https://calin.example.com`. It
  must be `https`. In public mode the Host guard accepts that host (and loopback), and the Origin
  guard accepts that origin.
- The tool endpoint for agents (`/api/v1/mcp/{token}`) is never reachable from outside in either
  mode: it answers only when the request came straight from loopback with no forwarding header.

## 2. Sign-in with OpenID Connect

Configuration, all from the environment (the install command writes them to a file readable only
by the service user):

| Variable | Meaning |
| --- | --- |
| `CALIN_PUBLIC_URL` | The public origin. Setting it selects public mode. |
| `CALIN_OIDC_ISSUER` | The provider's issuer URL, for example `https://your-tenant.eu.auth0.com/`. |
| `CALIN_OIDC_CLIENT_ID` | The client id registered with the provider. |
| `CALIN_OIDC_CLIENT_SECRET` | Optional. Without it Calin is a public client and relies on PKCE alone. |
| `CALIN_ALLOWED_USERS` | Comma-separated identities that may sign in: email addresses, or `sub:<subject>`. |

The redirect URI to register with the provider is `<CALIN_PUBLIC_URL>/auth/callback`.

Flow:

1. `GET /auth/login?next=<path>` creates a pending login (state, nonce, PKCE verifier, `next`),
   stored server-side for 10 minutes and referenced by a short-lived cookie, then redirects to the
   provider's authorization endpoint with scope `openid email`, the state, the nonce and the code
   challenge. `next` must be a path on this origin; anything else becomes `/`.
2. `GET /auth/callback` checks the state against the pending login, exchanges the code with the
   verifier, and validates the ID token: signature against the provider's published keys, issuer,
   audience, expiry and nonce.
3. The identity is the `email` claim when `email_verified` is true, otherwise `sub:<sub>`. It must
   match an entry of the allow-list exactly (emails compared case-insensitively). An email that is
   not verified never matches an email entry. A user whose email is verified is identified by that
   email only: a `sub:` entry does not admit them.
4. On success Calin creates its own session and redirects to `next`. On any failure it shows a
   plain page saying sign-in failed, with no detail that helps an attacker, and logs the reason.

The provider's metadata and keys are fetched at startup and refreshed when a token names an unknown
key (at most once a minute). If the provider cannot be reached at startup the server still starts
and retries; sign-in fails until it succeeds, and existing sessions keep working.

## 3. Sessions

- A session is a random 256-bit token. The server stores only its SHA-256, with the identity,
  creation time, last use and user agent, in `<home>/auth.json` (written atomically, mode 0600).
- Cookie `__Host-calin_session`: `HttpOnly`, `Secure`, `SameSite=Lax`, `Path=/`, `Max-Age` of 30
  days. The `__Host-` prefix stops a sibling subdomain from planting it. The pending-login cookie
  is `__Secure-calin_login`, limited to `/auth` and 10 minutes. `Lax` rather than
  `Strict` so that following a link to Calin from elsewhere arrives signed in; state-changing
  requests are additionally protected by the Origin guard.
- A session lasts 30 days from its last use and is extended as it is used (the cookie is re-issued
  at most once a day). It ends on sign-out, on revocation, or when its identity leaves the
  allow-list (checked on every request).
- `POST /auth/logout` ends the current session. `GET /api/v1/me` returns the identity and the
  mode, or 401.
- Every `/api/v1` route and both WebSocket endpoints require a session or a device token in public
  mode, except `GET /api/v1/health`, which then returns only `{ "name": "calin", "auth": "required" }`.
  The web app's static files are served without a session; the app redirects to `/auth/login` when
  `GET /api/v1/me` says 401.
- Failed callbacks are rate-limited per client address (10 per 10 minutes). A failure counts only
  when the request carried a live pending login issued by this server, so nobody can lock a user
  out by making their browser send junk callbacks. The client address is
  the peer address, or the last `X-Forwarded-For` entry when the peer is loopback (the reverse
  proxy).

## 4. Device tokens

For the desktop app, the `calin` CLI and scripts.

- Created by a signed-in user in Settings: a name, shown once as `calin_<random>`, stored hashed
  like sessions with the identity that created it.
- Sent as `Authorization: Bearer <token>`. For WebSockets, where a browser cannot set headers, a
  non-browser client sends the same header; the browser never uses device tokens.
- Listed (name, created, last used) and revoked in Settings. A token dies with its creator's
  allow-list entry.
- Routes: `GET /api/v1/tokens`, `POST /api/v1/tokens`, `DELETE /api/v1/tokens/{id}`. Sessions are
  listed and revoked the same way: `GET /api/v1/auth/sessions`, `DELETE /api/v1/auth/sessions/{id}`.
- In local mode there is nobody to sign in: `GET /api/v1/me` says so, and the token and session
  routes answer 409.
- Only RS256 and ES256 ID tokens are accepted. Providers behind a private certificate authority
  and outbound proxies are not supported.

## 5. Installing on a server

`calin server install` prepares a Linux machine with systemd. It writes files and prints the
commands that need root; it never runs `sudo` itself.

- A systemd unit running `calin serve` as a dedicated unprivileged user, restarting on failure,
  with `NoNewPrivileges`, a private `/tmp` and the data directory as its only writable state path
  besides the repos it works in.
- An environment file for section 2's variables, mode 0600.
- A Caddy configuration for the domain: TLS, a reverse proxy to the loopback port, WebSocket
  upgrade, and no caching of `/api`.
- `calin server check` verifies an installation: the unit is active, the public URL answers, the
  provider's metadata is reachable, the agent CLIs it finds are signed in, `gh` is signed in.
- `calin server uninstall` prints what to remove. It deletes nothing by itself.

The agent CLIs (`claude`, `gh`) are signed in once by the service user, through Calin's own
terminal after the first sign-in.

## 6. The phone app

- The web app is installable: a manifest (name, icons, standalone display, theme colors for both
  themes) and a service worker that caches the app's static files so the shell opens instantly and
  shows a clear "offline" state. It never caches `/api` or `/auth`.
- Notifications use Web Push. The server creates a VAPID key pair on first use, kept in the data
  directory. A signed-in browser subscribes from Settings ("Notify this device"), which asks for
  permission only then. Subscriptions belong to the identity and are listed and removed in
  Settings.
- A push is sent when a session needs the user: it starts `waiting` (a permission request, a
  question or a gate), reaches `review`, or `failed`; and when a session's pull requests are all
  merged. Each kind can be switched off. At most one push per session per minute; none while a
  window of that identity has the session open and visible.
- The payload is encrypted end to end by the Web Push standard. It carries the session's title, a
  one-line reason and the session's path. Tapping it opens that session.
- Routes: `GET /api/v1/push/key`, `POST /api/v1/push/subscriptions`,
  `DELETE /api/v1/push/subscriptions/{id}`, `GET`/`PUT /api/v1/push/preferences`.
- Push needs a secure origin, so it exists only in public mode (and on `localhost` for development).

## 7. The desktop app

A Tauri 2 shell around the same web app, with connection profiles:

- **Local**: the Calin core runs inside the app's process on a loopback port chosen at start, with
  the data directory of the user. It is local mode exactly: no sign-in.
- **Remote**: a server URL and a device token, kept in the operating system's keychain. The app
  loads the server's web app from that URL and adds the token to its requests.
- Profiles are listed at start and can be switched from the app menu. The last one used opens by
  default.
- The app adds only what a browser cannot: a tray icon with the count of sessions that need the
  user, native notifications in local mode (where Web Push is unavailable), and "open in the system
  file manager or editor" for a session's worktree in local mode.

Building it needs system packages (WebKitGTK and friends on Linux) that require root to install.

## 8. What is not in this milestone

- Several users, roles or per-user data. One allow-list, one shared workspace.
- Sandboxing agents. They still run as the service user with a full shell.
- Any listening address other than loopback.
- Signing and notarizing desktop builds, and automatic updates.

## 9. Testing

- Sign-in is tested against a small OpenID Connect provider written for the tests, which issues
  real signed tokens: the full flow, and each refusal (wrong state, wrong nonce, wrong audience,
  expired, unknown key, unverified email, identity not on the list, replayed code).
- Every route is tested for 401 without credentials in public mode, and local mode keeps working
  with no configuration.
- Web Push is tested against a fake push service that decrypts the payload with the subscription's
  keys.
- The install command is tested by reading the files it generates, not by installing.
- A run against a real provider, a real domain and a real phone needs the owner: see `status.md`.
