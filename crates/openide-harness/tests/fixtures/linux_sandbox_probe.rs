use std::fs;
use std::io;
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

unsafe extern "C" {
    fn socket(domain: i32, kind: i32, protocol: i32) -> i32;
    fn syscall(number: i64, ...) -> i64;
    fn fork() -> i32;
    fn setsid() -> i32;
    fn unshare(flags: i32) -> i32;
    fn _exit(code: i32) -> !;
}

fn denied(error: io::Error) {
    assert_eq!(
        error.raw_os_error(),
        Some(1),
        "expected seccomp EPERM: {error}"
    );
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    match args[1].as_str() {
        "confinement" => {
            let outside = Path::new(&args[2]);
            fs::write("inside", b"workspace-ok").unwrap();
            assert!(fs::read(outside.join("secret")).is_err());
            assert!(fs::write(outside.join("secret"), b"changed").is_err());
            assert!(fs::write("escape", b"changed").is_err());
            assert!(fs::write("/usr/openide-sandbox-write-probe", b"changed").is_err());
            fs::write("/tmp/private-temp", b"allowed").unwrap();
            assert!(fs::write("/unmounted-root-file", b"changed").is_err());
            denied(TcpStream::connect(&args[3]).unwrap_err());
            denied(UnixStream::connect("local.sock").unwrap_err());
            for family in [2, 10, 1, 40] {
                // IPv4, IPv6, UNIX, VSOCK
                assert_eq!(unsafe { socket(family, 1, 0) }, -1);
                denied(io::Error::last_os_error());
            }
            // Both supported syscall ABIs assign io_uring_setup number 425.
            assert_eq!(unsafe { syscall(425, 1u32, std::ptr::null::<u8>()) }, -1);
            denied(io::Error::last_os_error());
            assert_eq!(unsafe { unshare(0x10000000) }, -1); // CLONE_NEWUSER
            denied(io::Error::last_os_error());
            let status = fs::read_to_string("/proc/self/status").unwrap();
            assert!(status.contains("NoNewPrivs:\t1"));
            assert!(status.contains("Seccomp:\t2"));
            let fds = fs::read_dir("/proc/self/fd")
                .unwrap()
                .filter_map(Result::ok)
                .collect::<Vec<_>>();
            // read_dir owns one descriptor. Every other non-stdio descriptor
            // must already have been consumed/closed before the target exec.
            assert!(
                fds.len() <= 4,
                "unexpected inherited descriptors: {}",
                fds.len()
            );
            assert!(fs::read(format!("/proc/self/fd/{}", args[4])).is_err());
            assert!(fs::read("/proc/1/root/../oldroot").is_err());
            println!("confinement-ok");
        }
        "tree" => {
            let pid = unsafe { fork() };
            assert!(pid >= 0);
            if pid == 0 {
                assert!(unsafe { setsid() } > 0);
                fs::write("detached-started", b"ready").unwrap();
                loop {
                    let old = fs::read_to_string("heartbeat")
                        .unwrap_or_default()
                        .parse::<u64>()
                        .unwrap_or_default();
                    fs::write("heartbeat", (old + 1).to_string()).unwrap();
                    std::thread::sleep(Duration::from_millis(15));
                }
            }
            fs::write("parent-started", b"ready").unwrap();
            if args.get(2).is_some_and(|mode| mode == "exit") {
                while !Path::new("detached-started").exists() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                unsafe { _exit(0) };
            }
            loop {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        "echo" => {
            use std::io::{Read, Write};
            let mut bytes = Vec::new();
            io::stdin().read_to_end(&mut bytes).unwrap();
            fs::write("received", &bytes).unwrap();
            io::stdout().write_all(&bytes).unwrap();
        }
        #[cfg(target_arch = "x86_64")]
        "abi-x32" => {
            fs::write("abi-before", b"entered").unwrap();
            unsafe { syscall(0x40000000 | 39) };
            fs::write("abi-bypass", b"bad").unwrap();
            panic!("x32 ABI bypassed the native-architecture filter");
        }
        #[cfg(target_arch = "x86_64")]
        "abi-compat" => {
            fs::write("abi-before", b"entered").unwrap();
            unsafe {
                std::arch::asm!("int 0x80", inout("eax") 20u32 => _, options(nostack));
            }
            fs::write("abi-bypass", b"bad").unwrap();
            panic!("compat ABI bypassed the native-architecture filter");
        }
        _ => panic!("unknown mode"),
    }
}
