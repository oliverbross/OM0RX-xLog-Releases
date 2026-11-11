#if os(Linux)
import Foundation
import Glibc

/// A single pipe chunk on the contest host's existing dedicated I/O threads.
enum ContestHostOutputReader {
    static func readAvailableData(from handle: FileHandle) throws -> Data {
        // Linux Foundation's availableData traps on EINTR. Its throwing
        // read(upToCount:) instead waits to fill the buffer or reach EOF.
        // Keep the existing 8-KiB, single-read streaming behavior here.
        var data = Data(count: 8 * 1024)
        let count = try data.withUnsafeMutableBytes { buffer in
            try readByteCount {
                let count = Glibc.read(handle.fileDescriptor, buffer.baseAddress, buffer.count)
                let error = count < 0 ? errno : 0
                return (count, error)
            }
        }
        data.removeSubrange(count..<data.count)
        return data
    }

    static func readByteCount(
        isCancelled: () -> Bool = { Thread.current.isCancelled },
        read: () -> (count: Int, error: Int32)
    ) throws -> Int {
        while !isCancelled() {
            let result = read()
            if result.count >= 0 { return result.count }
            if result.error == EINTR { continue }
            throw NSError(domain: NSPOSIXErrorDomain, code: Int(result.error))
        }
        return 0
    }
}
#endif
