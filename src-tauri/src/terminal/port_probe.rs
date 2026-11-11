//! Silent loopback TCP port radar for live terminal sessions.
//!
//! Queries process file descriptors directly via OS kernel tables (`proc_pidinfo`
//! and `proc_pidfdinfo` on macOS, `/proc/<pid>/net/tcp{,6}` on Linux) without
//! spawning external `lsof` subprocesses, catching dev servers that omit URL
//! stdout lines.

/// Maximum candidate PIDs (session root + descendants/session members) inspected per probe.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const MAX_CANDIDATE_PIDS: usize = 64;

/// Maximum open file/socket descriptors inspected per candidate process.
/// Dev servers (Vite, Next.js, Cargo) routinely hold hundreds of watch FDs or
/// IPC sockets alongside their HTTP listener, so 256 truncated before reaching
/// high-numbered listening sockets.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const MAX_FDS_PER_PID: usize = 4_096;

/// Returns sorted, deduplicated TCP ports in `LISTEN` state bound to loopback
/// (`127.0.0.0/8`, `::1`) or wildcard (`0.0.0.0`, `::`) owned by `root_pid` or
/// its live session/descendant processes.
pub fn listening_loopback_ports_for_root_pid(root_pid: u32) -> Vec<u16> {
    if root_pid == 0 {
        return Vec::new();
    }
    #[cfg(target_os = "macos")]
    {
        macos::listening_ports(root_pid)
    }
    #[cfg(target_os = "linux")]
    {
        linux::listening_ports(root_pid)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = root_pid;
        Vec::new()
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{MAX_CANDIDATE_PIDS, MAX_FDS_PER_PID};
    use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
    use std::mem::{size_of, MaybeUninit};
    use std::net::{Ipv4Addr, Ipv6Addr};

    const MAX_SCANNED_PROCESSES: usize = 16_384;
    const PROC_PIDLISTFDS: i32 = 1;
    const PROC_PIDT_SHORTBSDINFO: i32 = 13;
    const PROC_PIDFDSOCKETINFO: i32 = 3;
    const PROX_FDTYPE_SOCKET: u32 = 2;
    const SOCKINFO_TCP: i32 = 2;
    const TSI_S_LISTEN: i32 = 1;
    const INI_IPV4: u8 = 0x1;
    const INI_IPV6: u8 = 0x2;
    const SOCK_MAXADDRLEN: usize = 255;

    /// `struct proc_fdinfo` from `<sys/proc_info.h>`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ProcFdInfo {
        proc_fd: i32,
        proc_fdtype: u32,
    }

    /// `struct proc_bsdshortinfo` from `<sys/proc_info.h>`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ProcBsdShortInfo {
        pbsi_pid: u32,
        pbsi_ppid: u32,
        pbsi_pgid: u32,
        pbsi_status: u32,
        pbsi_comm: [libc::c_char; 16],
        pbsi_flags: u32,
        pbsi_uid: libc::uid_t,
        pbsi_gid: libc::gid_t,
        pbsi_ruid: libc::uid_t,
        pbsi_rgid: libc::gid_t,
        pbsi_svuid: libc::uid_t,
        pbsi_svgid: libc::gid_t,
        pbsi_rfu: u32,
    }

    /// `struct proc_fileinfo` from `<sys/proc_info.h>`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ProcFileInfo {
        fi_openflags: u32,
        fi_status: u32,
        fi_offset: i64,
        fi_type: i32,
        fi_guardflags: u32,
    }

    /// `struct sockbuf_info` from `<sys/proc_info.h>`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct SockbufInfo {
        sbi_cc: u32,
        sbi_hiwat: u32,
        sbi_mbcnt: u32,
        sbi_mbmax: u32,
        sbi_lowat: u32,
        sbi_flags: i16,
        sbi_timeo: i16,
    }

    /// `struct in4in6_addr` from `<sys/proc_info.h>`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct In4In6Addr {
        i46a_pad32: [u32; 3],
        i46a_addr4: libc::in_addr,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    union InAddrUnion {
        ina_46: In4In6Addr,
        ina_6: libc::in6_addr,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct In4Tos {
        in4_tos: u8,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct In6SockOpts {
        in6_hlim: u8,
        in6_cksum: i32,
        in6_ifindex: u16,
        in6_hops: i16,
    }

    /// `struct in_sockinfo` from `<sys/proc_info.h>`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct InSockInfo {
        insi_fport: i32,
        insi_lport: i32,
        insi_gencnt: u64,
        insi_flags: u32,
        insi_flow: u32,
        insi_vflag: u8,
        insi_ip_ttl: u8,
        rfu_1: u32,
        insi_faddr: InAddrUnion,
        insi_laddr: InAddrUnion,
        insi_v4: In4Tos,
        insi_v6: In6SockOpts,
    }

    /// `struct tcp_sockinfo` from `<sys/proc_info.h>`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct TcpSockInfo {
        tcpsi_ini: InSockInfo,
        tcpsi_state: i32,
        tcpsi_timer: [i32; 4],
        tcpsi_mss: i32,
        tcpsi_flags: u32,
        rfu_1: u32,
        tcpsi_tp: u64,
    }

    /// `struct un_sockinfo` from `<sys/proc_info.h>` (largest union arm of `soi_proto`).
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct UnSockInfo {
        unsi_conn_so: u64,
        unsi_conn_pcb: u64,
        unsi_addr: [u8; SOCK_MAXADDRLEN],
        unsi_caddr: [u8; SOCK_MAXADDRLEN],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    union SoiProtoUnion {
        pri_in: InSockInfo,
        pri_tcp: TcpSockInfo,
        pri_un: UnSockInfo,
    }

    /// `struct socket_info` from `<sys/proc_info.h>`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct SocketInfo {
        soi_stat: libc::vinfo_stat,
        soi_so: u64,
        soi_pcb: u64,
        soi_type: i32,
        soi_protocol: i32,
        soi_family: i32,
        soi_options: i16,
        soi_linger: i16,
        soi_state: i16,
        soi_qlen: i16,
        soi_incqlen: i16,
        soi_qlimit: i16,
        soi_timeo: i16,
        soi_error: u16,
        soi_oobmark: u32,
        soi_rcv: SockbufInfo,
        soi_snd: SockbufInfo,
        soi_kind: i32,
        rfu_1: u32,
        soi_proto: SoiProtoUnion,
    }

    /// `struct socket_fdinfo` from `<sys/proc_info.h>`.
    /// Guarded at runtime by checking `written == size_of::<SocketFdInfo>()`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct SocketFdInfo {
        pfi: ProcFileInfo,
        psi: SocketInfo,
    }

    fn list_all_pids() -> Option<Vec<i32>> {
        // SAFETY: passing null buffer queries current PID count.
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        let count = usize::try_from(count).ok()?;
        if count == 0 || count > MAX_SCANNED_PROCESSES {
            return None;
        }
        let mut pids = vec![0i32; count + 64];
        let bytes = i32::try_from(pids.len() * size_of::<i32>()).ok()?;
        // SAFETY: `bytes` is the exact byte length of `pids`.
        let written = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        let written = usize::try_from(written).ok()?;
        if written >= pids.len() {
            return None;
        }
        pids.truncate(written);
        Some(pids)
    }

    fn short_bsdinfo(pid: i32) -> Option<ProcBsdShortInfo> {
        let size = size_of::<ProcBsdShortInfo>();
        let len = i32::try_from(size).ok()?;
        let mut info = MaybeUninit::<ProcBsdShortInfo>::zeroed();
        // SAFETY: buffer matches `ProcBsdShortInfo` and is read only when filled completely.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                PROC_PIDT_SHORTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                len,
            )
        };
        if usize::try_from(written).ok() != Some(size) {
            return None;
        }
        // SAFETY: kernel reported filling every byte of `ProcBsdShortInfo`.
        Some(unsafe { info.assume_init() })
    }

    fn collect_candidate_pids(root_pid: i32) -> Vec<i32> {
        let mut seen = HashSet::with_capacity(MAX_CANDIDATE_PIDS);
        let mut queue = VecDeque::with_capacity(MAX_CANDIDATE_PIDS);
        seen.insert(root_pid);
        queue.push_back(root_pid);

        let Some(all_pids) = list_all_pids() else {
            return vec![root_pid];
        };

        // SAFETY: `getsid` takes an integer PID and touches no memory.
        let root_sid = unsafe { libc::getsid(root_pid) };
        let match_session = root_sid == root_pid && root_sid > 1;

        let mut children_by_ppid: HashMap<i32, Vec<i32>> = HashMap::new();
        for pid in all_pids {
            if pid <= 1 {
                continue;
            }
            if match_session {
                // SAFETY: `getsid` takes an integer PID and touches no memory.
                if unsafe { libc::getsid(pid) } == root_sid
                    && seen.len() < MAX_CANDIDATE_PIDS
                    && seen.insert(pid)
                {
                    queue.push_back(pid);
                }
            }
            if let Some(info) = short_bsdinfo(pid) {
                if let Ok(ppid) = i32::try_from(info.pbsi_ppid) {
                    if ppid > 1 {
                        children_by_ppid.entry(ppid).or_default().push(pid);
                    }
                }
            }
        }

        let mut ordered = Vec::with_capacity(seen.len().min(MAX_CANDIDATE_PIDS));
        while let Some(pid) = queue.pop_front() {
            ordered.push(pid);
            if ordered.len() >= MAX_CANDIDATE_PIDS {
                break;
            }
            if let Some(kids) = children_by_ppid.get(&pid) {
                for &kid in kids {
                    if seen.len() < MAX_CANDIDATE_PIDS && seen.insert(kid) {
                        queue.push_back(kid);
                    }
                }
            }
        }
        ordered
    }

    fn is_loopback_or_wildcard_tcp(ini: &InSockInfo) -> bool {
        let mut ok = false;
        if (ini.insi_vflag & INI_IPV4) != 0 {
            // SAFETY: `INI_IPV4` selects the IPv4 `ina_46` union arm.
            let raw = unsafe { ini.insi_laddr.ina_46 }.i46a_addr4.s_addr;
            let addr = Ipv4Addr::from(u32::from_be(raw));
            if addr.is_loopback() || addr.is_unspecified() {
                ok = true;
            }
        }
        if !ok && (ini.insi_vflag & INI_IPV6) != 0 {
            // SAFETY: `INI_IPV6` selects the IPv6 `ina_6` union arm.
            let octets = unsafe { ini.insi_laddr.ina_6 }.s6_addr;
            let addr = Ipv6Addr::from(octets);
            if addr.is_loopback()
                || addr.is_unspecified()
                || addr
                    .to_ipv4_mapped()
                    .is_some_and(|v4| v4.is_loopback() || v4.is_unspecified())
            {
                ok = true;
            }
        }
        ok
    }

    fn inspect_pid_sockets(pid: i32, out: &mut BTreeSet<u16>) {
        // Query needed byte size for open file descriptors.
        // SAFETY: null buffer queries required byte count for `PROC_PIDLISTFDS`.
        let needed =
            unsafe { libc::proc_pidinfo(pid, PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        let Ok(needed_usize) = usize::try_from(needed) else {
            return;
        };
        if needed_usize == 0 {
            return;
        }
        let fd_size = size_of::<ProcFdInfo>();
        let cap = (needed_usize / fd_size).saturating_add(32).min(8_192);
        let mut fds = vec![
            ProcFdInfo {
                proc_fd: 0,
                proc_fdtype: 0,
            };
            cap
        ];
        let Ok(buf_bytes) = i32::try_from(fds.len() * fd_size) else {
            return;
        };
        // SAFETY: `buf_bytes` equals `fds.len() * size_of::<ProcFdInfo>()`.
        let written = unsafe {
            libc::proc_pidinfo(pid, PROC_PIDLISTFDS, 0, fds.as_mut_ptr().cast(), buf_bytes)
        };
        let Ok(written_usize) = usize::try_from(written) else {
            return;
        };
        fds.truncate(written_usize / fd_size);

        let sock_size = size_of::<SocketFdInfo>();
        let Ok(sock_len) = i32::try_from(sock_size) else {
            return;
        };

        let mut inspected_sockets = 0usize;
        for entry in fds {
            if entry.proc_fdtype != PROX_FDTYPE_SOCKET {
                continue;
            }
            inspected_sockets += 1;
            if inspected_sockets > MAX_FDS_PER_PID {
                break;
            }
            let mut sock = MaybeUninit::<SocketFdInfo>::zeroed();
            // SAFETY: `sock` has size `sock_len` matching `SocketFdInfo`.
            let got = unsafe {
                libc::proc_pidfdinfo(
                    pid,
                    entry.proc_fd,
                    PROC_PIDFDSOCKETINFO,
                    sock.as_mut_ptr().cast(),
                    sock_len,
                )
            };
            if usize::try_from(got).ok() != Some(sock_size) {
                continue;
            }
            // SAFETY: zeroed and verified to have `sock_size` bytes written by the kernel.
            let sock = unsafe { sock.assume_init() };
            if sock.psi.soi_kind != SOCKINFO_TCP {
                continue;
            }
            // SAFETY: `soi_kind == SOCKINFO_TCP` selects `pri_tcp`.
            let tcp = unsafe { sock.psi.soi_proto.pri_tcp };
            if tcp.tcpsi_state != TSI_S_LISTEN {
                continue;
            }
            let port = u16::from_be((tcp.tcpsi_ini.insi_lport & 0xffff) as u16);
            if port == 0 {
                continue;
            }
            if is_loopback_or_wildcard_tcp(&tcp.tcpsi_ini) {
                out.insert(port);
            }
        }
    }

    pub(super) fn listening_ports(root_pid: u32) -> Vec<u16> {
        let Ok(root) = i32::try_from(root_pid) else {
            return Vec::new();
        };
        if root <= 1 {
            return Vec::new();
        }
        let mut ports = BTreeSet::new();
        for pid in collect_candidate_pids(root) {
            inspect_pid_sockets(pid, &mut ports);
        }
        ports.into_iter().collect()
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{MAX_CANDIDATE_PIDS, MAX_FDS_PER_PID};
    use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
    use std::fs;
    use std::net::Ipv4Addr;

    fn parse_stat_ppid(pid: u32) -> Option<u32> {
        let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let close = text.rfind(')')?;
        let rest = text.get(close + 2..)?;
        let mut parts = rest.split_whitespace();
        let _state = parts.next()?;
        parts.next()?.parse::<u32>().ok()
    }

    fn collect_candidate_pids(root_pid: u32) -> Vec<u32> {
        let mut seen = HashSet::with_capacity(MAX_CANDIDATE_PIDS);
        let mut queue = VecDeque::with_capacity(MAX_CANDIDATE_PIDS);
        seen.insert(root_pid);
        queue.push_back(root_pid);

        let mut children_by_ppid: HashMap<u32, Vec<u32>> = HashMap::new();
        if let Ok(entries) = fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
                    continue;
                };
                if pid <= 1 {
                    continue;
                }
                if let Some(ppid) = parse_stat_ppid(pid) {
                    if ppid > 1 {
                        children_by_ppid.entry(ppid).or_default().push(pid);
                    }
                }
            }
        }

        let mut ordered = Vec::new();
        while let Some(pid) = queue.pop_front() {
            ordered.push(pid);
            if ordered.len() >= MAX_CANDIDATE_PIDS {
                break;
            }
            if let Some(kids) = children_by_ppid.get(&pid) {
                for &kid in kids {
                    if seen.len() < MAX_CANDIDATE_PIDS && seen.insert(kid) {
                        queue.push_back(kid);
                    }
                }
            }
        }
        ordered
    }

    fn collect_socket_inodes(pids: &[u32]) -> HashSet<u64> {
        let mut inodes = HashSet::new();
        for &pid in pids {
            let Ok(entries) = fs::read_dir(format!("/proc/{pid}/fd")) else {
                continue;
            };
            for (idx, entry) in entries.flatten().enumerate() {
                if idx >= MAX_FDS_PER_PID {
                    break;
                }
                let Ok(target) = fs::read_link(entry.path()) else {
                    continue;
                };
                let s = target.to_string_lossy();
                if let Some(ino_str) = s.strip_prefix("socket:[").and_then(|r| r.strip_suffix(']'))
                {
                    if let Ok(ino) = ino_str.parse::<u64>() {
                        if ino > 0 {
                            inodes.insert(ino);
                        }
                    }
                }
            }
        }
        inodes
    }

    fn is_loopback_or_wildcard_hex_v4(hex_ip: &str) -> bool {
        let Ok(raw) = u32::from_str_radix(hex_ip, 16) else {
            return false;
        };
        let addr = Ipv4Addr::from(u32::from_be(raw.to_be()));
        // `/proc/net/tcp` stores IPv4 in host-endian hex (`0100007F` on LE for `127.0.0.1`).
        let host_bytes = raw.to_ne_bytes();
        let host_addr = Ipv4Addr::from(host_bytes);
        let _ = addr;
        host_addr.is_loopback() || host_addr.is_unspecified()
    }

    fn is_loopback_or_wildcard_hex_v6(hex_ip: &str) -> bool {
        if hex_ip.len() != 32 {
            return false;
        }
        if hex_ip.chars().all(|c| c == '0') {
            return true;
        }
        // `::1` in `/proc/net/tcp6` on little-endian is `00000000000000000000000001000000`
        // and on big-endian is `00000000000000000000000000000001`.
        let lower = hex_ip.to_ascii_lowercase();
        if lower == "00000000000000000000000001000000"
            || lower == "00000000000000000000000000000001"
        {
            return true;
        }
        // IPv4-mapped `::ffff:x.x.x.x`
        if lower.starts_with("0000000000000000ffff0000") {
            return is_loopback_or_wildcard_hex_v4(&lower[24..]);
        }
        false
    }

    fn scan_proc_net_tcp(path: &str, ipv6: bool, inodes: &HashSet<u64>, out: &mut BTreeSet<u16>) {
        let Ok(content) = fs::read_to_string(path) else {
            return;
        };
        for line in content.lines().skip(1) {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() < 10 {
                continue;
            }
            // State `0A` == TCP_LISTEN
            if !cols[3].eq_ignore_ascii_case("0a") {
                continue;
            }
            let Ok(inode) = cols[9].parse::<u64>() else {
                continue;
            };
            if !inodes.contains(&inode) {
                continue;
            }
            let Some((hex_ip, hex_port)) = cols[1].split_once(':') else {
                continue;
            };
            let Ok(port) = u16::from_str_radix(hex_port, 16) else {
                continue;
            };
            if port == 0 {
                continue;
            }
            let ok = if ipv6 {
                is_loopback_or_wildcard_hex_v6(hex_ip)
            } else {
                is_loopback_or_wildcard_hex_v4(hex_ip)
            };
            if ok {
                out.insert(port);
            }
        }
    }

    pub(super) fn listening_ports(root_pid: u32) -> Vec<u16> {
        if root_pid <= 1 {
            return Vec::new();
        }
        let pids = collect_candidate_pids(root_pid);
        let inodes = collect_socket_inodes(&pids);
        if inodes.is_empty() {
            return Vec::new();
        }
        let mut ports = BTreeSet::new();
        scan_proc_net_tcp(
            &format!("/proc/{root_pid}/net/tcp"),
            false,
            &inodes,
            &mut ports,
        );
        scan_proc_net_tcp(
            &format!("/proc/{root_pid}/net/tcp6"),
            true,
            &inodes,
            &mut ports,
        );
        ports.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::listening_loopback_ports_for_root_pid;
    use std::net::{TcpListener, UdpSocket};

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn detects_live_loopback_and_wildcard_tcp_listeners_and_drops_closed_ports() {
        let lo = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let lo_port = lo.local_addr().expect("loopback addr").port();

        let any = TcpListener::bind("0.0.0.0:0").expect("bind wildcard listener");
        let any_port = any.local_addr().expect("wildcard addr").port();

        let v6 = TcpListener::bind("[::1]:0").ok();
        let v6_port = v6
            .as_ref()
            .and_then(|l| l.local_addr().ok())
            .map(|a| a.port());

        let ports = listening_loopback_ports_for_root_pid(std::process::id());
        assert!(
            ports.contains(&lo_port),
            "expected loopback port {lo_port} in {ports:?}"
        );
        assert!(
            ports.contains(&any_port),
            "expected wildcard port {any_port} in {ports:?}"
        );
        if let Some(p6) = v6_port {
            assert!(
                ports.contains(&p6),
                "expected IPv6 loopback port {p6} in {ports:?}"
            );
        }
        let mut sorted = ports.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(ports, sorted, "ports must be sorted and deduplicated");

        drop(lo);
        let after_drop = listening_loopback_ports_for_root_pid(std::process::id());
        assert!(
            !after_drop.contains(&lo_port),
            "closed port {lo_port} must no longer appear in {after_drop:?}"
        );
        assert!(
            after_drop.contains(&any_port),
            "still-open port {any_port} must remain in {after_drop:?}"
        );
    }

    #[test]
    fn returns_empty_for_zero_or_nonexistent_pid() {
        assert!(listening_loopback_ports_for_root_pid(0).is_empty());
        assert!(listening_loopback_ports_for_root_pid(u32::MAX).is_empty());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn detects_listener_beyond_256_open_sockets() {
        let mut filler = Vec::with_capacity(300);
        for _ in 0..300 {
            let Ok(sock) = UdpSocket::bind("127.0.0.1:0") else {
                break;
            };
            filler.push(sock);
        }
        assert!(
            filler.len() >= 260,
            "test must hold >256 open sockets before binding TCP listener (opened {})",
            filler.len()
        );
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("bind TCP listener after >256 sockets");
        let port = listener.local_addr().expect("listener addr").port();

        let ports = listening_loopback_ports_for_root_pid(std::process::id());
        assert!(
            ports.contains(&port),
            "expected high-FD loopback port {port} in {ports:?} with {} open sockets",
            filler.len()
        );
        drop(listener);
        drop(filler);
    }
}
