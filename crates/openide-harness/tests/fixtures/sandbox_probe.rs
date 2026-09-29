use std::io::Write;
use std::net::TcpStream;
use std::time::Duration;

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    match args.get(1).map(String::as_str) {
        Some("--echo-stdin") => {
            let mut input = String::new();
            std::io::stdin().read_line(&mut input).unwrap();
            println!("OUT:{input}");
            eprintln!("ERR:{input}");
        }
        Some("--flood") => {
            let bytes = vec![b'x'; 256 * 1024];
            std::io::stdout().write_all(&bytes).unwrap();
            std::io::stderr().write_all(&bytes).unwrap();
        }
        Some("--write") => {
            println!("write={:?}", std::fs::write(&args[2], b"escaped"));
        }
        Some("--delayed-write") => {
            std::fs::write(&args[2], b"started").unwrap();
            std::thread::sleep(Duration::from_secs(2));
            std::fs::write(&args[3], b"survived").unwrap();
        }
        Some("--spawn-child") => {
            let child = std::process::Command::new(&args[0])
                .args(["--delayed-write", &args[2], &args[3]])
                .spawn();
            println!("child_spawned={}", child.is_ok());
            std::thread::sleep(Duration::from_secs(10));
        }
        _ => {
            let inside = std::fs::write(&args[1], b"inside");
            let outside = std::fs::write(&args[2], b"outside");
            let through_link = std::fs::write(&args[4], b"reparse");
            let outside_read = std::fs::read(&args[5]);
            let through_link_read = std::fs::read(&args[6]);
            let address = format!("127.0.0.1:{}", &args[3]).parse().unwrap();
            // LPAC can reject Winsock initialization itself; Rust's Winsock
            // wrapper panics in that case, so probe in a separate thread.
            let network = std::thread::spawn(move || {
                TcpStream::connect_timeout(&address, Duration::from_secs(2))
            })
            .join();
            println!("inside={inside:?}");
            println!("outside={outside:?}");
            println!("through_link={through_link:?}");
            println!("outside_read={outside_read:?}");
            println!("through_link_read={through_link_read:?}");
            println!("network_denied={}", !matches!(network, Ok(Ok(_))));
            println!(
                "credential_present={}",
                std::env::var_os("OPENAI_API_KEY").is_some()
            );
            println!(
                "secret_canary_present={}",
                std::env::var_os("OPENIDE_SANDBOX_SECRET_CANARY").is_some()
            );
        }
    }
}
