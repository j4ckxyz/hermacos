import AppKit
import HermesCore
import ImageIO
import UniformTypeIdentifiers

/// A file or image on a message: waiting in the composer, or already sent.
struct Attachment: Identifiable, Equatable {
    enum Source: Equatable {
        case file(URL)
        case data(Data)
        /// Already on the server: staged earlier, or part of a stored message.
        case remote
    }

    let id = UUID()
    var name: String
    var mime: String
    var isImage: Bool
    var byteCount: Int
    var source: Source
    var thumbnail: NSImage?
    /// `@file:` reference returned when a file was staged; goes into the prompt text.
    var refText: String?
    /// Where a staged image lives on the server; re-attaches it without another upload.
    var serverPath: String?

    /// The gateway refuses larger uploads.
    static let maxBytes = 25 * 1024 * 1024
    /// Image formats the gateway accepts as they are; anything else is converted to PNG.
    private static let passthroughImageTypes: [UTType] = [.png, .jpeg, .gif, .webP]

    enum LoadError: LocalizedError {
        case folder(String)
        case unreadable(String)
        case tooLarge(String)

        var errorDescription: String? {
            switch self {
            case .folder(let name): "“\(name)” is a folder. Attach the files inside it instead."
            case .unreadable(let name): "“\(name)” couldn't be read."
            case .tooLarge(let name): "“\(name)” is larger than 25 MB."
            }
        }
    }

    /// A file picked, dropped or pasted from disk.
    init(fileURL url: URL) throws {
        let name = url.lastPathComponent
        let values = try? url.resourceValues(forKeys: [.isDirectoryKey, .fileSizeKey, .contentTypeKey])
        if values?.isDirectory == true { throw LoadError.folder(name) }
        guard let size = values?.fileSize else { throw LoadError.unreadable(name) }
        guard size <= Self.maxBytes else { throw LoadError.tooLarge(name) }
        let type = values?.contentType ?? UTType(filenameExtension: url.pathExtension) ?? .data
        self.name = name
        self.mime = type.preferredMIMEType ?? "application/octet-stream"
        self.isImage = type.conforms(to: .image)
        self.byteCount = size
        self.source = .file(url)
        self.thumbnail = isImage ? Self.thumbnail(for: CGImageSourceCreateWithURL(url as CFURL, nil)) : nil
    }

    /// Image bytes with no file behind them (a pasted screenshot).
    init(imageData data: Data, name: String) throws {
        guard data.count <= Self.maxBytes else { throw LoadError.tooLarge(name) }
        self.name = name
        self.mime = UTType(filenameExtension: (name as NSString).pathExtension)?.preferredMIMEType ?? "image/png"
        self.isImage = true
        self.byteCount = data.count
        self.source = .data(data)
        self.thumbnail = Self.thumbnail(for: CGImageSourceCreateWithData(data as CFData, nil))
    }

    /// Media on a message read back from the server.
    init(stored: MessageAttachment) {
        name = stored.name
        mime = stored.kind == .image ? "image/png" : "application/octet-stream"
        isImage = stored.kind == .image
        byteCount = 0
        source = .remote
        refText = stored.refText
        serverPath = stored.serverPath
    }

    /// Small decoded preview; the full image is never kept in memory.
    private static func thumbnail(for source: CGImageSource?) -> NSImage? {
        guard let source else { return nil }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: 240,
        ]
        guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else { return nil }
        return NSImage(cgImage: image, size: NSSize(width: image.width, height: image.height))
    }

    /// The bytes to upload, converted when the server wouldn't take the original format.
    func payload() throws -> (name: String, mime: String, data: Data) {
        let data: Data
        switch source {
        case .file(let url):
            guard let contents = try? Data(contentsOf: url, options: .mappedIfSafe) else { throw LoadError.unreadable(name) }
            data = contents
        case .data(let contents):
            data = contents
        case .remote:
            throw LoadError.unreadable(name)
        }
        guard isImage else { return (name, mime, data) }
        let type = UTType(mimeType: mime) ?? .png
        if Self.passthroughImageTypes.contains(where: { type.conforms(to: $0) }) { return (name, mime, data) }
        // HEIC, TIFF and friends: re-encode as PNG.
        guard let converted = NSBitmapImageRep(data: data)?.representation(using: .png, properties: [:]),
              converted.count <= Self.maxBytes
        else { throw LoadError.unreadable(name) }
        let base = (name as NSString).deletingPathExtension
        return ("\(base).png", "image/png", converted)
    }

    var sizeLabel: String? {
        byteCount > 0 ? ByteCountFormatter.string(fromByteCount: Int64(byteCount), countStyle: .file) : nil
    }

    var symbol: String {
        if isImage { return "photo" }
        switch (name as NSString).pathExtension.lowercased() {
        case "pdf": return "doc.richtext"
        case "zip", "gz", "tar", "tgz", "7z": return "doc.zipper"
        case "csv", "tsv", "xlsx", "xls", "numbers": return "tablecells"
        case "json", "yaml", "yml", "toml", "xml", "html", "css", "js", "ts", "py", "rs", "swift", "go", "sh", "c", "cpp", "java":
            return "chevron.left.forwardslash.chevron.right"
        case "mp3", "wav", "m4a", "flac", "ogg": return "waveform"
        case "mp4", "mov", "mkv", "webm": return "film"
        default: return "doc.text"
        }
    }
}

extension NSPasteboard {
    /// Files on the pasteboard (copied in Finder, or dragged in).
    var fileURLs: [URL] {
        (readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL]) ?? []
    }

    /// Image bytes on the pasteboard (a screenshot, "Copy Image"), as PNG or JPEG.
    ///
    /// Spreadsheets and word processors also put a picture of the selection on the pasteboard
    /// next to the text; for those the text is what the user means, so no image is reported.
    var imagePayload: (data: Data, name: String)? {
        let present = Set(types ?? [])
        let textual: Set<NSPasteboard.PasteboardType> = [.rtf, .rtfd, .tabularText]
        guard present.isDisjoint(with: textual) else { return nil }
        if let png = data(forType: .png) { return (png, "Pasted image.png") }
        if let jpeg = data(forType: NSPasteboard.PasteboardType("public.jpeg")) { return (jpeg, "Pasted image.jpg") }
        if let tiff = data(forType: .tiff),
           let png = NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:]) {
            return (png, "Pasted image.png")
        }
        return nil
    }
}
