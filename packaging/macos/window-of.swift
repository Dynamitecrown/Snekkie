// Prints "WINDOW_ID WIDTHxHEIGHT" for the first normal on-screen window owned
// by process PID, or exits 1 if it has none. Used by scripts/test-macos-app.sh.
// Window owners and bounds need no screen-recording permission.
import CoreGraphics
import Foundation

guard CommandLine.arguments.count == 2, let pid = Int32(CommandLine.arguments[1]) else {
    FileHandle.standardError.write("Usage: window-of PID\n".data(using: .utf8)!)
    exit(2)
}
let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
for window in windows {
    guard (window[kCGWindowOwnerPID as String] as? Int32) == pid,
          (window[kCGWindowLayer as String] as? Int) == 0,
          let number = window[kCGWindowNumber as String] as? Int,
          let bounds = window[kCGWindowBounds as String] as? [String: Any],
          let width = bounds["Width"] as? Double, let height = bounds["Height"] as? Double,
          width > 100, height > 100
    else { continue }
    print("\(number) \(Int(width))x\(Int(height))")
    exit(0)
}
exit(1)
