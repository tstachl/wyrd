// Stage 1 entry gate: prove the sandboxed extension reaches a Unix
// socket inside the App Group container. The probe connects to a
// stub listener — not the owner — at the production path, so a pass
// isolates the sandbox premise from all daemon work. The owner-side
// bind and the real client arrive in Stage 2; this file then grows
// into the framing layer.
//
// Naming rule: the App Group container directory is the literal
// group id (`group.md.enesis.wyrd`) — the system applies no team
// prefix. Verified live 2026-11-11: the system materialised
// `~/Library/Group Containers/group.md.enesis.wyrd/` for the
// development team, confirmed by the probe log and fileproviderd.
// The probe resolves the directory through `containerURL`, never
// from a hardcoded path — the listener recipe below does the same
// by reading the resolved path back out of the probe log.
//
// Procedure: run the containing app once first so the system
// materialises the container, read the resolved path from the probe
// log (`unreachable <path>` on the first pass), create only the
// `Sockets` directory inside that resolved container, start the
// listener there, then force the extension to re-instantiate (quit
// and relaunch the containing app — reopening the domain alone may
// not re-run a surviving extension process):
//
//   p='<path from the probe log>'
//   mkdir -p "$(dirname "$p")"
//   rm -f "$p"
//   python3 - "$p" <<'EOF'
//   import socket, sys
//   s = socket.socket(socket.AF_UNIX)
//   s.bind(sys.argv[1])
//   s.listen(1)
//   print('listening', sys.argv[1], flush=True)
//   s.accept()
//   EOF
//
// Scope the `mkdir` precisely: `Sockets/` inside the resolved
// container is the listener's to create. Never create the group
// container itself by hand — on a fresh machine that makes a decoy
// directory the sandboxed extension cannot reach. If the probe log
// says `no group container`, the entitlement is not effective in
// the running build (ad-hoc signing, missing provisioning); fix
// signing before listening.

import Darwin
import Foundation

/// Result of one connect attempt. A tiny type, not a string
/// protocol: the caller branches on the case, never on wording.
enum ProbeOutcome {
    case reachable(path: String)
    case unreachable(detail: String)

    var message: String {
        switch self {
        case .reachable(let path):
            return "reachable: \(path)"
        case .unreachable(let detail):
            return detail
        }
    }

    var isReachable: Bool {
        if case .reachable = self {
            return true
        }
        return false
    }
}

enum SocketProbe {
    static let groupIdentifier = "group.md.enesis.wyrd"
    static let socketName = "wyrd-probe.sock"

    /// Outcome of one connect attempt.
    static func run() -> ProbeOutcome {
        guard let container = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: groupIdentifier
        ) else {
            return .unreachable(
                detail: "no group container for \(groupIdentifier)"
            )
        }
        let path = container
            .appendingPathComponent("Sockets")
            .appendingPathComponent(socketName)
            .path
        let pathBytes = Array(path.utf8)
        guard pathBytes.count < 104 else {
            return .unreachable(
                detail: "socket path too long (\(pathBytes.count) bytes)"
            )
        }
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else {
            return .unreachable(detail: "socket() failed: \(errnoString())")
        }
        defer { close(fd) }
        // Bound the probe: a non-blocking connect with a short
        // deadline, so a deaf listener (backlog full, never accepts)
        // cannot stall the caller. The extension invokes this off
        // the init path as well; both halves matter.
        let flags = fcntl(fd, F_GETFL, 0)
        guard flags >= 0, fcntl(fd, F_SETFL, flags | O_NONBLOCK) >= 0 else {
            return .unreachable(
                detail: "unreachable \(path): cannot set non-blocking (\(errnoString()))"
            )
        }
        let rc = attemptConnect(fd: fd, path: path)
        let connectErrno = errno
        if rc == 0 {
            return .reachable(path: path)
        }
        // EINTR and EINPROGRESS both leave the outcome unknown: fall
        // through to the poll/SO_ERROR path, which resolves it,
        // rather than re-issuing connect — a second connect on an
        // in-flight socket misreports EALREADY/EISCONN.
        if connectErrno != EINPROGRESS, connectErrno != EINTR {
            if connectErrno == EAGAIN || connectErrno == EALREADY {
                // The listener exists but cannot take the
                // connection — a busy stub, not a failed premise.
                return .unreachable(
                    detail: "listener busy \(path): \(errorString(connectErrno))"
                )
            }
            return .unreachable(
                detail: "unreachable \(path): \(errorString(connectErrno))"
            )
        }
        var pfd = pollfd(fd: fd, events: Int16(POLLOUT), revents: 0)
        var ready = poll(&pfd, 1, 2000)
        if ready < 0, errno == EINTR {
            ready = poll(&pfd, 1, 2000)
        }
        if ready == 0 {
            return .unreachable(detail: "unreachable \(path): connect timed out")
        }
        guard ready > 0 else {
            return .unreachable(detail: "unreachable \(path): \(errnoString())")
        }
        var err: Int32 = 0
        var len = socklen_t(MemoryLayout<Int32>.size)
        guard getsockopt(fd, SOL_SOCKET, SO_ERROR, &err, &len) == 0 else {
            return .unreachable(
                detail: "unreachable \(path): cannot read connect result (\(errnoString()))"
            )
        }
        if err == 0 {
            return .reachable(path: path)
        }
        return .unreachable(detail: "unreachable \(path): \(errorString(err))")
    }

    private static func attemptConnect(fd: Int32, path: String) -> Int32 {
        var addr = sockaddr_un()
        memset(&addr, 0, MemoryLayout<sockaddr_un>.size)
        addr.sun_family = sa_family_t(AF_UNIX)
        path.withCString { cString in
            withUnsafeMutableBytes(of: &addr.sun_path) { raw in
                let dest = raw.baseAddress!.assumingMemoryBound(to: CChar.self)
                strncpy(dest, cString, 103)
            }
        }
        return withUnsafePointer(to: &addr) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sock in
                connect(fd, sock, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
    }

    private static func errnoString() -> String {
        errorString(errno)
    }

    private static func errorString(_ code: Int32) -> String {
        String(cString: strerror(code))
    }
}
