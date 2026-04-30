#!/usr/bin/env bash
# Capture the running Protocol window to a PNG.
# Usage: ./capture.sh [out.png]
# Needs Screen Recording permission only (no Accessibility) — granted on first run.

set -euo pipefail

OUT="${1:-/tmp/protocol.png}"

# Find the on-screen window ID owned by the "protocol" process.
WID=$(/usr/bin/swift - <<'SWIFT'
import CoreGraphics
import Foundation

// gpui creates auxiliary off-screen windows alongside the real one (input
// buffers, shadow surfaces). Match on the titled, on-screen one.
let opts: CGWindowListOption = [.optionAll]
guard let list = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] else { exit(0) }
for w in list {
    let owner = w[kCGWindowOwnerName as String] as? String ?? ""
    let name = w[kCGWindowName as String] as? String ?? ""
    let onScreen = (w[kCGWindowIsOnscreen as String] as? Bool) ?? false
    let layer = w[kCGWindowLayer as String] as? Int ?? -1
    if owner == "protocol" && layer == 0 && onScreen && !name.isEmpty {
        if let n = w[kCGWindowNumber as String] as? Int { print(n); exit(0) }
    }
}
SWIFT
)

if [[ -z "${WID:-}" ]]; then
    echo "protocol window not found — is the app running?" >&2
    exit 1
fi

screencapture -x -o -l "$WID" "$OUT"
echo "wrote $OUT (window id $WID)"
