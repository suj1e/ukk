import CoreGraphics
import Darwin
import Foundation
import ImageIO
import UniformTypeIdentifiers

/// Validates the one user-controlled filesystem write in this helper.
///
/// The frame is written to a temporary sibling and atomically renamed into
/// place, so a failed encode never leaves a half-written PNG and a pre-existing
/// regular file is replaced only after a complete encode. Symlinks, directories
/// and special files are refused: the helper must not be redirected into
/// writing through a link or into a device/FIFO.
enum OutputPath {
    /// macOS `PATH_MAX` is 1024 bytes; rejecting earlier keeps failures local
    /// and deterministic instead of relying on `ENAMETOOLONG` from the kernel.
    static let maxUTF8Bytes = 1024

    static func validate(_ raw: String, fileManager: FileManager = .default) throws -> URL {
        guard !raw.isEmpty else {
            throw HelperError.usage("--out path is empty")
        }
        guard raw.utf8.count <= maxUTF8Bytes else {
            throw HelperError.usage("--out path is longer than \(maxUTF8Bytes) bytes")
        }
        guard !raw.contains("\0") else {
            throw HelperError.usage("--out path contains a NUL byte")
        }

        let expanded = (raw as NSString).expandingTildeInPath
        let base = URL(fileURLWithPath: fileManager.currentDirectoryPath, isDirectory: true)
        let url: URL
        if expanded.hasPrefix("/") {
            url = URL(fileURLWithPath: expanded).standardizedFileURL
        } else {
            url = URL(fileURLWithPath: expanded, relativeTo: base).standardizedFileURL
        }

        guard url.pathExtension.lowercased() == "png" else {
            throw HelperError.usage("--out must end in .png (got `\(url.pathExtension)`)")
        }

        let parent = url.deletingLastPathComponent()
        var parentIsDirectory: ObjCBool = false
        guard fileManager.fileExists(atPath: parent.path, isDirectory: &parentIsDirectory) else {
            throw HelperError.usage("--out parent directory does not exist: \(parent.path)")
        }
        guard parentIsDirectory.boolValue else {
            throw HelperError.usage("--out parent is not a directory: \(parent.path)")
        }

        var info = stat()
        if lstat(url.path, &info) == 0 {
            switch info.st_mode & mode_t(S_IFMT) {
            case mode_t(S_IFDIR):
                throw HelperError.usage("--out is an existing directory: \(url.path)")
            case mode_t(S_IFLNK):
                throw HelperError.usage(
                    "--out is a symbolic link; choose a regular file path: \(url.path)"
                )
            case mode_t(S_IFREG):
                break
            default:
                throw HelperError.usage("--out is not a regular file: \(url.path)")
            }
        } else if errno != ENOENT {
            throw HelperError.outputFailed(
                "cannot inspect --out path \(url.path): \(String(cString: strerror(errno)))"
            )
        }

        return url
    }

    static func writePNG(_ image: CGImage, to url: URL) throws {
        let temporary = url.deletingLastPathComponent()
            .appendingPathComponent(".ukk-capture-\(UUID().uuidString).png", isDirectory: false)

        // Screen pixels are sensitive: create the temporary file owner-only so
        // there is no window in which another local user can read it. `rename`
        // preserves the mode on the final output.
        let descriptor = open(temporary.path, O_WRONLY | O_CREAT | O_EXCL, mode_t(0o600))
        guard descriptor >= 0 else {
            let code = errno
            throw HelperError.outputFailed(
                "cannot create temporary PNG for \(url.path): \(String(cString: strerror(code)))"
            )
        }
        close(descriptor)

        guard let destination = CGImageDestinationCreateWithURL(
            temporary as CFURL,
            UTType.png.identifier as CFString,
            1,
            nil
        ) else {
            try? FileManager.default.removeItem(at: temporary)
            throw HelperError.outputFailed("cannot create PNG destination for \(url.path)")
        }

        CGImageDestinationAddImage(destination, image, nil)

        guard CGImageDestinationFinalize(destination) else {
            try? FileManager.default.removeItem(at: temporary)
            throw HelperError.outputFailed("cannot encode PNG at \(url.path)")
        }

        // ImageIO may create its own output file with the process umask, so
        // enforce owner-only again before the atomic rename.
        guard chmod(temporary.path, mode_t(0o600)) == 0 else {
            let code = errno
            try? FileManager.default.removeItem(at: temporary)
            throw HelperError.outputFailed(
                "cannot set owner-only permissions on \(url.path): \(String(cString: strerror(code)))"
            )
        }

        guard rename(temporary.path, url.path) == 0 else {
            let code = errno
            try? FileManager.default.removeItem(at: temporary)
            throw HelperError.outputFailed(
                "cannot move PNG into place at \(url.path): \(String(cString: strerror(code)))"
            )
        }
    }
}
