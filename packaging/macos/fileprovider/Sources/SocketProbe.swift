// Stage 1 entry gate: prove the sandboxed extension reaches a Unix
// socket inside the App Group container. The probe connects to a
// stub listener — not the owner — at the production path, so a pass
// isolates the sandbox premise from all daemon work. The owner-side
// bind and the real client arrive in Stage 2; this file then grows
// into the framing layer.
//
// Naming rule: the entitlement value stays literal
// (`group.md.enesis.wyrd`), but the container directory macOS
// materialises for a third-party team gains the team prefix:
// `~/Library/Group Containers/<TeamID>.group.md.enesis.wyrd`
// (`JU2RFHWQPH.group.md.enesis.wyrd` for the development team).
// The probe resolves the directory through `containerURL`, never
// from a hardcoded path — the listener recipe in the README does
// the same by reading the resolved path back out of the probe log.
//
// Procedure: run the containing app once first so the system
// materialises the container, read the resolved path from the probe
// log (`unreachable <path>` on the first pass), start the listener
// there, then re-trigger the extension:
//   p=<path from the probe log>; rm -f \"$p\"; python3 -c \\
//     \"import socket,os,sys; s=socket.socket(socket.AF_UNIX);\" \\
//     \"s.bind(sys.argv[1]); s.listen(1); print('listening', sys.argv[1]);\" \\
//     \"s.accept()\" \"$p\"
// Never `mkdir` the container yourself: on a fresh machine that
// creates a decoy directory the sandboxed extension cannot reach.

import Darwin
import Foundation

enum SocketProbe {
    static let groupIdentifier = "group.md.enesis.wyrd"
    static let socketName = "wyrd-probe.sock"

    /// Human-readable outcome of one connect attempt.
    static func run() -> String {
        guard let container = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: groupIdentifier
        ) else {
            return "no group container for \(groupIdentifier)"
        }
        let path = container
            .appendingPathComponent("Sockets")
            .appendingPathComponent(socketName)
            .path
        let pathBytes = Array(path.utf8)
        guard pathBytes.count < 104 else {
            return "socket path too long (\(pathBytes.count) bytes)"
        }
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else {
            return "socket() failed: \(errnoString())"
        }
        defer { close(fd) }
        // Bound the probe: a non-blocking connect with a short
        // deadline, so a deaf listener (backlog full, never accepts)
        // cannot stall the caller. The extension invokes this off
        // the init path as well; both halves matter.
        let flags = fcntl(fd, F_GETFL, 0)
        _ = fcntl(fd, F_SETFL, flags | O_NONBLOCK)
        var addr = sockaddr_un()
        memset(&addr, 0, MemoryLayout<sockaddr_un>.size)
        addr.sun_family = sa_family_t(AF_UNIX)
        path.withCString { cString in
            withUnsafeMutableBytes(of: &addr.sun_path) { raw in
                let dest = raw.baseAddress!.assumingMemoryBound(to: CChar.self)
                strncpy(dest, cString, 103)
            }
        }
        let rc = withUnsafePointer(to: &addr) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sock in
                connect(fd, sock, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        if rc == 0 {
            return "reachable: \(path)"
        }
        guard errno == EINPROGRESS else {
            return "unreachable \(path): \(errnoString())"
        }
        var pfd = pollfd(fd: fd, events: Int16(POLLOUT), revents: 0)
        let ready = poll(&pfd, 1, 2000)
        if ready == 0 {
            return "unreachable \(path): connect timed out"
        }
        guard ready > 0 else {
            return "unreachable \(path): \(errnoString())"
        }
        var err: Int32 = 0
        var len = socklen_t(MemoryLayout<Int32>.size)
        getsockopt(fd, SOL_SOCKET, SO_ERROR, &err, &len)
        if err == 0 {
            return "reachable: \(path)"
        }
        return "unreachable \(path): \(String(cString: strerror(err)))"
    }

    private static func errnoString() -> String {
        String(cString: strerror(errno))
    }
}
