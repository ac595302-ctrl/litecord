# Experimental owner-operated Discord login

The `browser-login` feature adds a Windows-only temporary sign-in view to
Settings. It loads `https://discord.com/login` using the platform WebView2
engine. The owner completes authentication and any verification themselves.
The existing password exchange remains available as a fallback.

## Build and use

```sh
cargo build --locked --release -p litecord-desktop --features browser-login
```

Launch the resulting binary in account mode with a separate account database,
then choose **Open Discord sign-in** in Settings. Do not use a demo database.
On the Windows GNU toolchain, ship the dependency's x64 `WebView2Loader.dll`
beside the executable. The installed Microsoft Edge WebView2 Runtime is also
required. Without the loader DLL, Windows exits before the app starts.
The view temporarily occupies the Litecord window under its own sign-in header;
**Cancel sign-in** returns to Settings. Resizing follows the native window.
The view closes automatically after ten minutes or after one credential is
handed to the account backend. Real login and CAPTCHA acceptance are unverified.

The backend checks the candidate through its existing authenticated current-user
request before saving it in OS credential storage and starting its account
connection. Gateway readiness remains a separate step; receiving a credential
does not prove that the account connection works. This is an unofficial session
handoff, not OAuth or Social SDK authorization.

## Boundaries

- A new temporary browser-data directory and incognito view are created for each
  attempt. Existing browser profiles, cookies and saved credentials are not read.
- The platform engine's ordinary identity is retained. No fingerprint spoofing,
  CAPTCHA solving service, or manufactured challenge response is used.
- Script execution exits outside the top-level Discord origin. It observes only
  Authorization headers on this view's outgoing same-origin versioned API calls;
  it does not inspect passwords, storage, or login/CAPTCHA response bodies.
- Native handoff accepts one bounded ASCII credential with an unpredictable
  per-attempt capability and checks the reported HTTPS Discord origin. Pending
  native handoff buffers are zeroized. Credential values are never logged.
- Navigation is restricted to HTTPS Discord and hCaptcha hosts. Popups,
  downloads, development tools, and permission requests are disabled.
- Cancellation, timeout, and successful handoff drop the view, context, profile
  handle, and pending channel. The platform engine can keep files locked briefly;
  this is not a guarantee of immediate forensic erasure of OS browser artifacts.
- macOS and Linux do not receive a login button in this first implementation.

## Local validation, September 27, 2026

The JavaScript handoff passed six synthetic offline scenarios: first credential
and duplicate suppression; disallowed routes, malformed/oversized values and
expiry; wrong origin; child frames; XHR; and Fetch Request headers. Underlying
request forwarding is checked as well. These are not live authentication tests.

Earlier compilation attempts were blocked by Windows Application Control with
OS error 4551. After the owner manually changed Smart App Control settings,
the release build succeeded. Both native origin/capability/navigation policy
tests passed. The packaged executable started in UserSession mode, and its
Settings screen showed the new sign-in button. The first packaging attempt
omitted WebView2Loader.dll; adding the dependency's x64 DLL fixed startup.
Cancellation, resize behavior, CAPTCHA acceptance and live sign-in remain
unverified. The earlier account executable remains in its separate folder.

References:

- https://github.com/ViceVerse-cz/Serein/blob/main/docs/authentication.md
- https://docs.rs/wry/0.57.0/wry/struct.WebViewBuilder.html

The architectural comparison with Serein does not claim its unofficial session
handoff is platform-approved or live-verified.

## Known issues from owner testing

The owner reported reaching server browsing and manually sending messages, then
receiving a Discord account-disable notice for spam/platform abuse. The exact
enforcement trigger is unknown. Manual authentication and CAPTCHA completion do
not make the subsequent unofficial account transport supported. Do not treat
this prototype as evidence of account safety or use it to avoid enforcement.

The owner also reported a crash when selecting Reply from a server context menu.
The saved local log shows an `accesskit_consumer` panic because the focused node
ID was absent from the node list. This is an unresolved accessibility/focus
issue; the precise reproduction and root cause need offline investigation.
There is no evidence linking that UI panic to the account suspension.

Next work should reproduce the Reply transition with synthetic messages, audit
duplicate workers/reconnects/history queues/ambiguous writes with fake transports,
add redacted diagnostics, and assess supported integration capabilities before
further live testing. Browser-like identity handling in another client does not
establish protection against account enforcement.
