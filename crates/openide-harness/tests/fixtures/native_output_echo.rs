use std::fs::OpenOptions;
use std::io::{self, Read, Write};

fn main() {
    let mut received = OpenOptions::new()
        .create(true)
        .append(true)
        .open("received")
        .unwrap();
    let mut chunk = [0u8; 256];
    loop {
        let count = io::stdin().read(&mut chunk).unwrap();
        if count == 0 {
            break;
        }
        received.write_all(&chunk[..count]).unwrap();
        received.flush().unwrap();
        io::stdout().write_all(&chunk[..count]).unwrap();
        io::stdout().flush().unwrap();
    }
}
