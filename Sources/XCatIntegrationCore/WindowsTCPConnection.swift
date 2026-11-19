#if os(Windows)
import CClusterSocket
import Foundation

/// One queue owns the socket from connect through close. The protocol owner handles
/// protocol framing, command timeouts, generation checks and reconnect policy.
final class WindowsTCPConnection: @unchecked Sendable {
    enum State { case ready, failed(Error) }
    enum Completion { case contentProcessed(@Sendable (Error?) -> Void) }
    typealias Reader = @Sendable (Data?, Void?, Bool, Error?) -> Void

    var stateUpdateHandler: (@Sendable (State) -> Void)?
    private let host: String
    private let port: UInt16
    private let lane: DispatchQueue
    private let token = xlog_socket_cancellation_create()
    private var socket: OpaquePointer?
    private var closed = false
    private var buffer = [UInt8](repeating: 0, count: 16_384)

    init(host: String, port: UInt16, queue: DispatchQueue) {
        self.host = host
        self.port = port
        lane = queue
    }

    init(accepted: OpaquePointer) {
        host = "127.0.0.1"
        port = 0
        lane = DispatchQueue(label: "com.om0rx.xlog.cat-client")
        socket = accepted
    }

    deinit { if let socket { xlog_socket_close(socket) }; xlog_socket_cancellation_destroy(token) }

    func start() {
        lane.async { [self] in
            if !closed, socket != nil { stateUpdateHandler?(.ready); return }
            guard !closed, let token, let connected = xlog_socket_connect(host, port, 5000, token) else {
                if !closed { stateUpdateHandler?(.failed(XCatError.notConnected)) }
                return
            }
            socket = connected
            stateUpdateHandler?(.ready)
        }
    }

    func cancel() {
        if let token { xlog_socket_cancel(token) }
        lane.async { [self] in
            guard !closed else { return }
            closed = true
            if let socket { xlog_socket_close(socket) }
            socket = nil
            if port == 0 { stateUpdateHandler?(.failed(XCatError.notConnected)) }
        }
    }

    func receive(minimumIncompleteLength: Int, maximumLength: Int, completion: @escaping Reader) {
        lane.async { [self] in
            guard let socket, let token, xlog_socket_cancelled(token) == 0 else {
                completion(nil, nil, true, nil)
                return
            }
            let count = buffer.withUnsafeMutableBytes {
                xlog_socket_read(socket, $0.baseAddress, Int32(min(maximumLength, $0.count)), 40)
            }
            if count == -2 {
                receive(minimumIncompleteLength: minimumIncompleteLength, maximumLength: maximumLength, completion: completion)
            } else if count > 0 {
                completion(Data(buffer.prefix(Int(count))), nil, false, nil)
            } else {
                completion(nil, nil, true, count < 0 ? XCatError.notConnected : nil)
            }
        }
    }

    func send(content: Data, completion: Completion, permit: DigitalPTTPermit? = nil, permitOwner: DigitalPTTPermit.OwnerIdentity? = nil, permitGeneration: Int = 0) {
        let deadline = ContinuousClock.now + .seconds(4)
        let finished: @Sendable (Error?) -> Void
        switch completion { case let .contentProcessed(callback): finished = callback }
        lane.async { [self] in
            guard let socket, let token else { finished(XCatError.notConnected); return }
            var offset = 0
            while offset < content.count {
                guard xlog_socket_cancelled(token) == 0, ContinuousClock.now < deadline else {
                    cancel()
                    finished(XCatError.notConnected)
                    return
                }
                let count: Int32
                do {
                    let write = {
                        content.withUnsafeBytes {
                            xlog_socket_write(socket, $0.baseAddress!.advanced(by: offset), Int32($0.count - offset))
                        }
                    }
                    if let permit {
                        guard let permitOwner else { throw XCatError.notConnected }
                        count = try permit.withSendPermission(owner: permitOwner, connectionGeneration: permitGeneration, write)
                    }
                    else { count = write() }
                } catch {
                    // A partial protocol line cannot safely be followed by OFF.
                    // Retire this exact socket and keep the owner's duty uncertain.
                    if offset > 0 { cancel() }
                    finished(error); return
                }
                if count == -2 { Thread.sleep(forTimeInterval: 0.01); continue }
                guard count > 0 else {
                    cancel()
                    finished(XCatError.notConnected)
                    return
                }
                offset += Int(count)
            }
            finished(nil)
        }
    }
}
#endif
