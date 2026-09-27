# Windows presentation build

Based on `new-main-temp` (hosted-login prototype `db928457`), descended from
`new-main`. Build with:

```powershell
cargo build --locked --release -p litecord-desktop --features browser-login,screenshots
python scripts/package_windows_presentation.py --output C:/path/to/Litecord-Presentation
```

The packaging command requires Python 3.11+, a clean committed checkout, and
Cargo's downloaded dependency cache. It bundles the x64 WebView2 loader from
the exact locked dependency version, verifies executable/DLL architecture,
records source and file hashes, includes source.zip, and checks ZIP integrity.
It refuses to overwrite an existing output directory. The installed Microsoft
Edge WebView2 Runtime is required for the hosted sign-in view.

## Run

- **Start Presentation Demo.cmd** uses synthetic data and a synthetic bot. It
  needs no Discord account and stores data in `%LOCALAPPDATA%/Litecord/presentation-demo`.
- **Start Litecord Account.cmd** explicitly selects UserSession and stores data
  in `%LOCALAPPDATA%/Litecord/account`. The included account config is read-only.
  Open Settings and use **Open Discord sign-in** for owner-operated verification.
  The password API cannot complete CAPTCHA challenges.

The launchers work after moving the extracted folder. Keep the executable and
DLL together. Git updates do not update an already-built executable. Demo and
account databases must remain separate. The package contains neither credentials
nor cached account data. A known account database may reuse its OS-stored
credential when its owner launches account mode.

## Reply crash fix

The original Reply handler requested focus using an unscoped `Id::new`, while
the composer uses a UI-scoped TextEdit `id_salt`. A synthetic right-click/Reply
test reproduced the Windows `accesskit_consumer 0.35.0` panic: the requested
focused ID was absent from the node list.

Reply now queues focus for its conversation and applies it using the enabled
TextEdit's actual response on the next frame. Navigation cancels pending focus.
Private-note focus similarly uses a rendered response and is disabled when
privacy mode hides that editor. Accessibility remains enabled.

The regression tests validate each emitted accessibility tree through the same
consumer used on Windows. Both pointer selection and keyboard/assistive focus
followed by Enter preserve the draft and focus a real multiline editor without
submitting a message. Run:

```powershell
cargo test -p litecord-ui reply_
node crates/litecord-ui/tests/browser_login.cjs
```

Local validation on September 27, 2026: the browser-enabled UI suite passed
31 tests, including two native hosted-login policy checks and the two Reply
regressions. All six JavaScript handoff scenarios passed offline. The optimized
Windows build started and exited cleanly in both Demo and UserSession modes
with isolated databases. Its only additional non-system DLL import is
WebView2Loader.dll, which the package includes. Settings rendered the hosted
sign-in button and the read-only configuration state. No real login was attempted.

## Limits

These are synthetic/local checks. They do not establish live authentication,
full Discord compatibility, complete history coverage, or protection against
account enforcement. The hosted page does not turn unofficial native account
access into an approved integration. A disabled Discord account requires
Discord's review process; this UI fix cannot restore it.

The existing hosted-login prototype is included. No fingerprint spoofing,
automatic CAPTCHA solving, account restoration, or Windows security changes
were added. Runtime login cancellation/expiry and every WebView2 environment
remain separate checks; use the offline demo for a predictable presentation.
