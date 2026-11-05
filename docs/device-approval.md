# Device approval for RTSP readers

**Status: proposed design; not implemented.**

## Problem

The managed MediaMTX configuration currently allows RTSP reads from any
reachable client without a password. A client that can reach the streaming
ports can watch every published display. The control API remains a separate,
localhost-only, bearer-token-protected service.

## Desired behavior

- The TUI warns when an untrusted device attempts to read a stream.
- An untrusted device receives no video while its request is pending or
  rejected.
- The user can approve or reject the request in the TUI.
- Approval is bound to both the source IP address and the RTSP client
  fingerprint, and trusted devices are stored in
  `/etc/castdesk/trusted-devices.toml`.
- A trusted device can read published streams only while both identity fields
  match. A changed IP or fingerprint requires approval again.

## Approval flow

1. MediaMTX reports a read attempt to `castdeskd`, including its source IP,
   requested stream, and available RTSP client metadata.
2. `castdeskd` compares the request with the trusted-device store. It must
   reject unmatched requests before sending stream data, then report a
   deduplicated pending request to the TUI over the authenticated local API.
3. The TUI displays the IP, client fingerprint, requested stream, and
   **Approve** / **Reject** actions. Repeated attempts from the same
   IP/fingerprint pair should update the existing prompt rather than flood
   the UI.
4. On approval, `castdeskd` adds the exact IP/fingerprint pair to the trust
   store and applies the updated read policy. On rejection, the request stays
   blocked. Errors applying or saving a decision must be shown to the user;
   they must not be reported as a successful approval.
5. Unknown devices remain blocked after a daemon restart. Removing a trust
   entry immediately revokes that device's access.

The managed MediaMTX configuration must default to deny reads unless a
request is authorized. This design does not apply automatically to an
externally managed MediaMTX instance; the TUI must clearly show that approval
enforcement is unavailable unless equivalent access control is configured.

## Identity and security limits

For this design, a device fingerprint means the exact source IP address plus
the normalized RTSP `User-Agent` (or another documented, stable RTSP client
identifier, if MediaMTX exposes one). Both values are required for a match.
The value should be displayed to the user and stored as observed; do not treat
a missing identifier as a wildcard.

This is **not a cryptographic device signature**. RTSP `User-Agent` values are
client-supplied, can be copied or spoofed, and may change between software
versions. An IP address can also be shared, reassigned, or spoofed on some
networks. The pair is a useful approval heuristic, but it does not prove a
device's identity or protect against an attacker able to imitate a trusted
client on the same network. A future stronger design should pair devices
using a key or certificate and authenticate possession of the corresponding
private key.

Until request metadata can be observed and access denied before video is
released, the TUI warning alone is not a security control. Connection logs or
post-connection notifications are insufficient if they allow a reader to
receive frames first.

## Trust-store location and permissions

Use `/etc/castdesk/trusted-devices.toml` as the system-wide trust store, as
requested. Keep `castdeskd` running as the logged-in user; do not run the
daemon as root just to write this file. The installer or an administrator
must provision the directory and grant only the daemon's dedicated group the
minimum required access. The TUI should surface a clear error if the store is
not writable. Writes must be atomic, validate entries, and preserve restrictive
permissions. The trust store must not contain passwords or private keys.

Example format:

```toml
[[device]]
label = "living-room TV"
ip = "192.168.1.42"
user_agent = "ExampleReceiver/1.0"
approved_at = "2026-01-01T12:00:00Z"
```

The timestamp is informational, not an identity factor. Parsing must reject
malformed addresses and duplicate entries. The application must not silently
fall back to allowing all readers if the file is absent, unreadable, or invalid.

## Acceptance criteria

- With an empty trust store, a new reader is denied video and produces a
  visible TUI prompt containing its IP and fingerprint.
- Rejecting a request leaves it blocked; approving it allows only the matching
  IP/fingerprint pair.
- A different IP or fingerprint is treated as untrusted, even if one field
  matches.
- Approval persists across daemon restarts; revocation blocks subsequent
  reads.
- Missing metadata, unavailable MediaMTX access-control integration, or trust
  store errors fail closed and are visible in the TUI/logs.
- Documentation and UI call the identifier a **client fingerprint**, not a
  cryptographic signature.
