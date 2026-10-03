// Render the DMG window background (an SVG, 660×400 points) to a PNG with WebKit,
// so the pencil filters and handwriting font come out exactly as in Safari.
//
//   swiftc -O packaging/dmg/render.swift -o /tmp/render-dmg
//   /tmp/render-dmg packaging/dmg/background.svg /tmp/bg.png
//   sips -z 800 1320 /tmp/bg.png --out packaging/dmg/background@2x.png
//   sips -z 400 660 /tmp/bg.png --out packaging/dmg/background.png
import AppKit
import WebKit

let args = CommandLine.arguments
let input = URL(fileURLWithPath: args[1])
let output = URL(fileURLWithPath: args[2])
_ = NSApplication.shared
let web = WKWebView(frame: NSRect(x: 0, y: 0, width: 660, height: 400))

final class Snapshot: NSObject, WKNavigationDelegate {
    func webView(_ webView: WKWebView, didFinish _: WKNavigation!) {
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
            let config = WKSnapshotConfiguration()
            config.snapshotWidth = 1320 // points; at least 1320 px on any display
            webView.takeSnapshot(with: config) { image, error in
                guard let image, let tiff = image.tiffRepresentation,
                      let png = NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:])
                else {
                    FileHandle.standardError.write("render failed: \(String(describing: error))\n".data(using: .utf8)!)
                    exit(1)
                }
                try! png.write(to: output)
                exit(0)
            }
        }
    }
}

let delegate = Snapshot()
web.navigationDelegate = delegate
// Inline the SVG: WebKit may not be allowed to read files from the home folder.
let svg = try! Data(contentsOf: input).base64EncodedString()
web.loadHTMLString(
    "<html><body style='margin:0'><img src='data:image/svg+xml;base64,\(svg)' style='width:660px;height:400px;display:block'></body></html>",
    baseURL: nil
)
NSApplication.shared.run()
