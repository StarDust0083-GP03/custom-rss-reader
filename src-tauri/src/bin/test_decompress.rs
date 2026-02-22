use std::fs;

fn main() {
    println!("=== Testing Decompression Function ===\n");

    // Test file paths
    let test_files = vec![
        "/home/hsf/.local/share/com.hsf.rss-reader/debug_logs/raw_20260221_055223_0_https___plink.anyfeeder.com_weixin_gh_10a6b96351a9.xml",
        "/home/hsf/.local/share/com.hsf.rss-reader/debug_logs/raw_20260221_055225_0_https___www.paradedb.com_feed.xml.xml",
    ];

    for file_path in test_files {
        println!("Testing: {}", file_path);

        match fs::read(file_path) {
            Ok(bytes) => {
                println!("  File size: {} bytes", bytes.len());

                // Check magic bytes
                if bytes.len() >= 2 {
                    let magic = &bytes[0..2];
                    println!("  Magic bytes: {:02x} {:02x}", magic[0], magic[1]);

                    if magic[0] == 0x1f && magic[1] == 0x8b {
                        println!("  ✓ Detected gzip compression");

                        // Try to decompress
                        match decompress_gzip(&bytes) {
                            Ok(text) => {
                                let preview = if text.len() > 200 {
                                    format!("{}...", &text[..200])
                                } else {
                                    text.clone()
                                };
                                println!("  ✓ Decompressed successfully!");
                                println!("  Preview: {}", preview);
                            }
                            Err(e) => {
                                println!("  ✗ Decompression failed: {}", e);
                            }
                        }
                    } else {
                        println!("  Not gzip compressed");
                    }
                }
            }
            Err(e) => {
                println!("  ✗ Failed to read file: {}", e);
            }
        }
        println!();
    }
}

fn decompress_gzip(bytes: &[u8]) -> Result<String, String> {
    use flate2::read::GzDecoder;
    use std::io::Read;

    let mut decoder = GzDecoder::new(bytes);
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed)
        .map_err(|e| format!("Gzip decompression failed: {}", e))?;

    String::from_utf8(decompressed)
        .map_err(|_| "Decompressed data is not valid UTF-8".to_string())
}
