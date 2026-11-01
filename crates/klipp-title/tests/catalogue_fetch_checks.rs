//! A catalogue fetch keeps a file only when its hash is the catalogue's,
//! the licence beside the font; a file that does not match is refused and
//! nothing is kept.
//!
//! A test binary of its own, as `fetched_face_redraw`: a finished fetch is
//! counted for the whole process, and every renderer in it drops what it
//! has drawn (#2734). Among the crate's own tests it made a renderer
//! another test was counting draw its titles again (#3262).

use std::io::{Read, Write};
use std::sync::atomic::AtomicBool;

use klipp_title::catalogue::{fetch_in, is_fetched_in, CatalogueFamily, CatalogueFile};

/// Serves `body` once per request on a local port.
fn serve(body: Vec<u8>, requests: usize) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for _ in 0..requests {
            let Ok((mut s, _)) = listener.accept() else { break };
            let _ = s.read(&mut [0u8; 4096]);
            let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
            let _ = s.write_all(&body);
        }
    });
    port
}

fn sha(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

#[test]
fn a_fetch_checks_every_file_and_keeps_its_licence() {
    let font = klipp_title::bundled::BUNDLED[0].bytes.to_vec();
    let licence = b"SIL Open Font License 1.1".to_vec();
    let family = |font_url: String, licence_url: String, font_sha: String| CatalogueFamily {
        name: "Test Face".into(),
        style: vec!["sans".into()],
        mood: vec!["test".into()],
        scripts: vec!["latin".into()],
        licence: "OFL-1.1".into(),
        licence_url,
        licence_sha256: sha(&licence),
        licence_size: licence.len() as u64,
        files: vec![CatalogueFile { name: "TestFace.ttf".into(), url: font_url, sha256: font_sha, size: font.len() as u64 }],
    };
    let root = std::env::temp_dir().join(format!("klipp-font-fetch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (fp, lp) = (serve(font.clone(), 1), serve(licence.clone(), 1));
    let good = family(format!("http://127.0.0.1:{fp}/TestFace.ttf"), format!("http://127.0.0.1:{lp}/OFL.txt"), sha(&font));
    assert!(!is_fetched_in(&root, &good));
    let dir = fetch_in(&root, &good, &AtomicBool::new(false)).unwrap();
    assert!(is_fetched_in(&root, &good));
    assert_eq!(std::fs::read(dir.join("TestFace.ttf")).unwrap(), font);
    assert_eq!(std::fs::read(dir.join("OFL.txt")).unwrap(), licence, "the licence beside it");

    let other = root.join("other");
    let (fp, lp) = (serve(font.clone(), 1), serve(licence.clone(), 1));
    let bad = family(format!("http://127.0.0.1:{fp}/TestFace.ttf"), format!("http://127.0.0.1:{lp}/OFL.txt"), "0".repeat(64));
    assert!(fetch_in(&other, &bad, &AtomicBool::new(false)).is_err(), "a file that does not match is refused");
    assert!(!is_fetched_in(&other, &bad) && !other.join("test-face").join("TestFace.ttf").exists(), "and not kept");
    let _ = std::fs::remove_dir_all(&root);
}
