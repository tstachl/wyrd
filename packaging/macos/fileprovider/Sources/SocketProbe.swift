// Stage 1 entry gate: prove the sandboxed extension reaches a Unix
// socket inside the App Group container. The probe connects to a
// stub listener — not the owner — at the production path, so a pass
// isolates the sandbox premise from all daemon work. The owner-side
// bind and the real client arrive in Stage 2; this file then grows
// into the framing layer. The container URL comes from the system,
// never from a hardcoded path: the group id is granted literally.
//
// Run a listener before loading the extension, for example:
//   python3 -c "import socket,os; p=os.path.expanduser(
//     '~/Library/Group Containers/group.md.enesis.wyrd/Sockets/' +
//     'wyrd-probe.sock'); os.makedirs(os.path.dirname(p),
//     exist_ok=True); s=socket.socket(socket.AF_UNIX);
//     s.bind(p); s.listen(1); print('listening', p); s.accept()"

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
        var addr = sockaddr_un()
        memset(&addr, 0, MemoryLayout<sockaddr_un>.size)
        addr.sun_family = sa_family_t(AF_UNIX)
        path.withCString { cString in
            withUnsafeMutableBytes(of: &addr.sun_path) { raw in
                let dest = raw.baseAddress!.assumingMemoryBound(to: CChar.self)
                strncpy(dest, cString, 103)
            }
        }
        let connected = withUnsafePointer(to: &addr) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sock in
                connect(fd, sock, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        if connected == 0 {
            return "reachable: \(path)"
        }
        return "unreachable \(path): \(errnoString())"
    }

    private static func errnoString() -> String {
        String(cString: strerror(errno))
    }
}
