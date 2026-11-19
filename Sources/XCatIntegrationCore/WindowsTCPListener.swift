#if os(Windows)
import CClusterSocket
import Foundation

/// The listener owns its handle until cancel returns. Each accepted connection
/// has its own I/O queue and independent Winsock reference.
final class WindowsTCPListener: @unchecked Sendable {
    let port: UInt16
    private let timer: DispatchSourceTimer
    private let lock = NSLock()
    private var socket: OpaquePointer?

    init(port: UInt16, queue: DispatchQueue) throws {
        var bound: UInt16 = 0
        guard let socket = xlog_tcp_listen_loopback(port, &bound) else {
            throw XCatError.server("Cannot bind 127.0.0.1:\(port).")
        }
        self.socket = socket
        self.port = bound
        timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now(), repeating: .milliseconds(25))
        timer.resume()
    }

    func start(_ accept: @escaping @Sendable (WindowsTCPConnection) -> Void) {
        timer.setEventHandler { [weak self] in
            guard let self else { return }
            let client = lock.withLock { self.socket.flatMap(xlog_tcp_accept_loopback).map(WindowsTCPConnection.init(accepted:)) }
            if let client { accept(client) }
        }
    }

    func cancel() {
        timer.cancel()
        lock.withLock {
            if let socket { xlog_socket_close(socket); self.socket = nil }
        }
    }
    deinit { cancel() }
}
#endif
