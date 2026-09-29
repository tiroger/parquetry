// Quick Look preview extension for Parquet files.
//
// A data-based preview (macOS 12+): the Rust static library (crates/quicklook) renders a
// self-contained HTML document from the file footer and the first rows, and Quick Look
// displays it in its HTML view.

import Foundation
import QuickLookUI
import UniformTypeIdentifiers
import ParquetryQL

/// Number of rows rendered in the preview table.
private let previewRowLimit: UInt32 = 200

final class PreviewProvider: QLPreviewProvider, QLPreviewingController {
    func providePreview(for request: QLFilePreviewRequest) async throws -> QLPreviewReply {
        let fileURL = request.fileURL
        let reply = QLPreviewReply(
            dataOfContentType: .html,
            contentSize: CGSize(width: 1100, height: 760)
        ) { reply in
            reply.stringEncoding = .utf8
            return Self.renderHTML(for: fileURL)
        }
        reply.stringEncoding = .utf8
        return reply
    }

    /// Calls into Rust; always returns an HTML document (the preview or an error page).
    private static func renderHTML(for fileURL: URL) -> Data {
        let accessing = fileURL.startAccessingSecurityScopedResource()
        defer {
            if accessing { fileURL.stopAccessingSecurityScopedResource() }
        }

        var length = 0
        let buffer: UnsafeMutablePointer<UInt8>? = fileURL.withUnsafeFileSystemRepresentation { path in
            parquetry_ql_preview(path, previewRowLimit, &length)
        }
        guard let buffer else {
            return Data("<!DOCTYPE html><html><body><p>Unable to render preview.</p></body></html>".utf8)
        }
        defer { parquetry_ql_free(buffer, length) }
        return Data(bytes: buffer, count: length)
    }
}
