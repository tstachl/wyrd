// Stage 2 owner client: one typed connection per request over the
// drive-owner socket. Framing is the normative wire contract
// (u32 big-endian length prefix plus JSON): the Harness checks the
// request shape against `vectors.json`. Connections are cheap and
// short-lived — every operation opens, requests, reads, and closes,
// so recovery is always a new connection, never a resumed stream.
// A new connection per operation also makes reconnect-on-demand
// structural: there is no persistent connection to go stale.

import Darwin
import Foundation

/// The client-side answer. `refused` is the only terminal failure;
/// everything transport-shaped is `unavailable` or `unconfirmed`,
/// and everything protocol-shaped is `incompatible` — a retry
/// decision depends on which one came back.
enum OwnerClientError: Error {
    /// No transport: the socket is absent or the connection
    /// refused. No request was sent.
    case unavailable(String)
    /// The request never got a terminal answer: the connection
    /// broke, a frame tore, or the wait expired. Reconnect (a new
    /// connection per operation does this by construction), then
    /// reconcile before deciding whether retry is safe.
    case unconfirmed(op: String, detail: String)
    /// The owner answered with a defined refusal. Terminal: the
    /// request did not commit and will not commit on retry unless
    /// the caller changes what it asked for.
    case refused(OwnerError)
    /// Wrong version, echoed id mismatch, or an operation answered
    /// with a shape it never produces. Explicit, never generic.
    case incompatible(String)
}

/// Operation names: the exact wire vocabulary. Reads plus the
/// two mutation-side operations: `capture-parent` binds a create
/// to its parent incarnation, `submit` forwards one mutation to
/// the owner's single queue. No UI path constructs a mutation in
/// Stage 3 sub-PR 1; the Harness exercises both against a stub.
enum OwnerOp {
    static let lookup = "lookup"
    static let stat = "stat"
    static let readdir = "readdir"
    static let read = "read"
    static let captureParent = "capture-parent"
    static let submit = "submit"
}

/// Largest single frame in bytes, mirroring the owner's bound. A
/// length past this closes the connection; the break is
/// `unconfirmed`, never a refusal.
private let maxFrameBytes = 96 << 20

/// How long one request waits for the terminal answer, matching
/// the Rust client's bound: demand-bounded reads resolve inside
/// this, and expiry is `unconfirmed`, never an invented answer.
private let responseTimeout: TimeInterval = 120

final class OwnerClient {
    private let socketPath: String
    private var nextId: UInt64 = 1

    init(socketPath: String) {
        self.socketPath = socketPath
    }

    func lookup(path: String) throws -> (node: OwnerNode, generation: UInt64, revision: UInt64) {
        let response = try roundTrip(op: OwnerOp.lookup, params: .path(OwnerPathParams(path: path)))
        guard case .ok(let ok) = response.result else {
            throw refusedOrIncompatible(response: response, op: OwnerOp.lookup)
        }
        guard case .lookup(let node, let generation, let revision) = ok else {
            throw OwnerClientError.incompatible("lookup answered \(describe(ok))")
        }
        return (node, generation, revision)
    }

    func stat(path: String) throws -> (attr: OwnerAttr, generation: UInt64, revision: UInt64) {
        let response = try roundTrip(op: OwnerOp.stat, params: .path(OwnerPathParams(path: path)))
        guard case .ok(let ok) = response.result else {
            throw refusedOrIncompatible(response: response, op: OwnerOp.stat)
        }
        guard case .stat(let attr, let generation, let revision) = ok else {
            throw OwnerClientError.incompatible("stat answered \(describe(ok))")
        }
        return (attr, generation, revision)
    }

    func readdir(path: String) throws -> (
        entries: [OwnerDirEntry], generation: UInt64, revision: UInt64
    ) {
        let response = try roundTrip(op: OwnerOp.readdir, params: .path(OwnerPathParams(path: path)))
        guard case .ok(let ok) = response.result else {
            throw refusedOrIncompatible(response: response, op: OwnerOp.readdir)
        }
        guard case .readdir(let entries, let generation, let revision) = ok else {
            throw OwnerClientError.incompatible("readdir answered \(describe(ok))")
        }
        return (entries, generation, revision)
    }

    func read(path: String) throws -> (bytes: [UInt8], generation: UInt64, revision: UInt64) {
        let response = try roundTrip(op: OwnerOp.read, params: .path(OwnerPathParams(path: path)))
        guard case .ok(let ok) = response.result else {
            throw refusedOrIncompatible(response: response, op: OwnerOp.read)
        }
        guard case .read(let bytesHex, let generation, let revision) = ok else {
            throw OwnerClientError.incompatible("read answered \(describe(ok))")
        }
        do {
            return (try HexCodec.decode(bytesHex), generation, revision)
        } catch {
            throw OwnerClientError.incompatible("read carried undecodable bytes: \(error)")
        }
    }

    /// Capture the parent-directory incarnation a create sends
    /// against. `path` is the parent directory itself, never the
    /// file: `""` for the drive root. The token is single-attempt —
    /// a pass that replaces the namespace invalidates it — so the
    /// caller re-captures on `StaleParent` instead of reusing it.
    func captureParent(path: String) throws -> OwnerParentToken {
        let response = try roundTrip(op: OwnerOp.captureParent, params: .path(OwnerPathParams(path: path)))
        guard case .ok(let ok) = response.result else {
            throw refusedOrIncompatible(response: response, op: OwnerOp.captureParent)
        }
        guard case .parentToken(let token) = ok else {
            throw OwnerClientError.incompatible("capture-parent answered \(describe(ok))")
        }
        return token
    }

    /// Forward one mutation to the owner's single queue and block
    /// for its terminal answer. A sent request is not a committed
    /// write: only the terminal answer says what happened. No UI
    /// path calls this in Stage 3 sub-PR 1; the Harness proves the
    /// shape against a stub.
    func submit(kind: OwnerMutationKind) throws -> OwnerMutationOutcome {
        let response = try roundTrip(
            op: OwnerOp.submit,
            params: .submit(OwnerSubmitParams(kind: kind))
        )
        guard case .ok(let ok) = response.result else {
            throw refusedOrIncompatible(response: response, op: OwnerOp.submit)
        }
        guard case .submitted(let outcome) = ok else {
            throw OwnerClientError.incompatible("submit answered \(describe(ok))")
        }
        return outcome
    }

    private func refusedOrIncompatible(response: OwnerResponse, op: String) -> OwnerClientError {
        guard case .err(let err) = response.result else {
            return OwnerClientError.incompatible("\(op) answered neither ok nor err")
        }
        return OwnerClientError.refused(err)
    }

    private func describe(_ ok: OwnerOk) -> String {
        switch ok {
        case .lookup: return "Lookup"
        case .stat: return "Stat"
        case .readdir: return "Readdir"
        case .read: return "Read"
        case .parentToken: return "ParentToken"
        case .submitted: return "Submitted"
        }
    }

    private func roundTrip(op: String, params: OwnerParams) throws -> OwnerResponse {
        let id = nextId
        nextId &+= 1
        let request = OwnerRequest(v: 2, id: id, op: op, params: params)
        let body: Data
        do {
            body = try JSONEncoder().encode(request)
        } catch {
            throw OwnerClientError.incompatible("cannot encode \(op) request: \(error)")
        }
        let frame = try transact(op: op, body: body)
        let response: OwnerResponse
        do {
            response = try JSONDecoder().decode(OwnerResponse.self, from: frame)
        } catch {
            throw OwnerClientError.incompatible("cannot decode \(op) response: \(error)")
        }
        guard response.v == 2 else {
            throw OwnerClientError.incompatible("\(op) answered version \(response.v), want 2")
        }
        guard response.id == id else {
            throw OwnerClientError.incompatible("\(op) echoed id \(response.id), want \(id)")
        }
        return response
    }

    private func transact(op: String, body: Data) throws -> Data {
        let fd = try connectSocket(op: op)
        defer { close(fd) }
        var length = UInt32(body.count).bigEndian
        try withUnsafeBytes(of: &length) { pointer in
            try writeAll(fd: fd, bytes: pointer, op: op)
        }
        try body.withUnsafeBytes { pointer in
            try writeAll(fd: fd, bytes: pointer, op: op)
        }
        let prefix = try readExactly(fd: fd, count: 4, op: op)
        // Assemble the length byte-by-byte: a socket-read buffer
        // carries no alignment guarantee, so no direct load.
        let lengthValue = (UInt32(prefix[0]) << 24)
            | (UInt32(prefix[1]) << 16)
            | (UInt32(prefix[2]) << 8)
            | UInt32(prefix[3])
        guard lengthValue <= UInt32(maxFrameBytes) else {
            throw OwnerClientError.unconfirmed(
                op: op,
                detail: "response length \(lengthValue) past the frame bound"
            )
        }
        return try readExactly(fd: fd, count: Int(lengthValue), op: op)
    }

    private func connectSocket(op: String) throws -> Int32 {
        let pathBytes = Array(socketPath.utf8)
        guard pathBytes.count < 104 else {
            throw OwnerClientError.unavailable(
                "socket path too long (\(pathBytes.count) bytes)"
            )
        }
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else {
            throw OwnerClientError.unavailable("socket() failed: \(errorString(errno))")
        }
        let flags = fcntl(fd, F_GETFL, 0)
        guard flags >= 0, fcntl(fd, F_SETFL, flags | O_NONBLOCK) >= 0 else {
            close(fd)
            throw OwnerClientError.unavailable(
                "cannot set non-blocking (\(errorString(errno)))"
            )
        }
        let rc = withSockaddr(path: socketPath) { pointer, length in
            connect(fd, pointer, length)
        }
        if rc != 0, errno != EINPROGRESS, errno != EINTR {
            let detail = errorString(errno)
            close(fd)
            throw OwnerClientError.unavailable("connect refused: \(detail)")
        }
        if rc != 0 {
            var pfd = pollfd(fd: fd, events: Int16(POLLOUT), revents: 0)
            var ready = poll(&pfd, 1, 2000)
            if ready < 0, errno == EINTR {
                ready = poll(&pfd, 1, 2000)
            }
            guard ready > 0 else {
                close(fd)
                if ready == 0 {
                    throw OwnerClientError.unavailable("connect timed out")
                }
                throw OwnerClientError.unavailable("connect failed (\(errorString(errno)))")
            }
            var err: Int32 = 0
            var len = socklen_t(MemoryLayout<Int32>.size)
            guard getsockopt(fd, SOL_SOCKET, SO_ERROR, &err, &len) == 0 else {
                close(fd)
                throw OwnerClientError.unavailable(
                    "cannot read connect result (\(errorString(errno)))"
                )
            }
            guard err == 0 else {
                close(fd)
                throw OwnerClientError.unavailable("connect refused: \(errorString(err))")
            }
        }
        guard fcntl(fd, F_SETFL, flags) >= 0 else {
            close(fd)
            throw OwnerClientError.unavailable(
                "cannot restore blocking (\(errorString(errno)))"
            )
        }
        var timeout = timeval(
            tv_sec: Int(responseTimeout),
            tv_usec: 0
        )
        let timeoutLength = socklen_t(MemoryLayout<timeval>.size)
        guard setsockopt(
            fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, timeoutLength
        ) == 0,
            setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, timeoutLength) == 0
        else {
            close(fd)
            throw OwnerClientError.unavailable(
                "cannot set socket timeouts (\(errorString(errno)))"
            )
        }
        return fd
    }

    private func withSockaddr<T>(path: String, body: (UnsafePointer<sockaddr>, socklen_t) -> T) -> T {
        var addr = sockaddr_un()
        memset(&addr, 0, MemoryLayout<sockaddr_un>.size)
        addr.sun_family = sa_family_t(AF_UNIX)
        path.withCString { cString in
            withUnsafeMutableBytes(of: &addr.sun_path) { raw in
                let dest = raw.baseAddress!.assumingMemoryBound(to: CChar.self)
                strncpy(dest, cString, 103)
            }
        }
        return withUnsafePointer(to: &addr) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { sock in
                body(sock, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
    }

    private func writeAll(fd: Int32, bytes: UnsafeRawBufferPointer, op: String) throws {
        var written = 0
        while written < bytes.count {
            let base = bytes.baseAddress!.advanced(by: written)
            let rc = Darwin.write(fd, base, bytes.count - written)
            if rc <= 0 {
                throw OwnerClientError.unconfirmed(
                    op: op,
                    detail: "write failed (\(errorString(errno)))"
                )
            }
            written += rc
        }
    }

    private func readExactly(fd: Int32, count: Int, op: String) throws -> Data {
        var out = Data(count: count)
        var done = 0
        try out.withUnsafeMutableBytes { raw in
            let base = raw.baseAddress!.assumingMemoryBound(to: UInt8.self)
            while done < count {
                let rc = Darwin.read(fd, base.advanced(by: done), count - done)
                if rc > 0 {
                    done += rc
                    continue
                }
                if rc == 0 {
                    throw OwnerClientError.unconfirmed(
                        op: op,
                        detail: done == 0 ? "peer closed mid-frame" : "peer closed mid-body"
                    )
                }
                // A read timeout surfaces EAGAIN/EWOULDBLOCK on a
                // blocking socket with SO_RCVTIMEO: a deadline, not
                // a severed connection — still unconfirmed, but
                // named so the wait is diagnosable.
                if errno == EAGAIN || errno == EWOULDBLOCK {
                    throw OwnerClientError.unconfirmed(op: op, detail: "read timed out")
                }
                throw OwnerClientError.unconfirmed(
                    op: op,
                    detail: "read failed (\(errorString(errno)))"
                )
            }
        }
        return out
    }

    private func errorString(_ code: Int32) -> String {
        String(cString: strerror(code))
    }
}
